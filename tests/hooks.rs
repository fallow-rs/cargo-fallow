//! Black-box tests of `hooks install`, `hooks uninstall` and the agent gate.
//!
//! Each test makes a git repository with a small workspace, installs the gate
//! with the binary, and runs the generated script with bash and a payload in
//! the shape of Claude Code and Codex: JSON on stdin with `cwd` and
//! `tool_input.command`. Exit code 2 is a block.
#![cfg(unix)]
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_cargo-fallow");
const SCRIPT: &str = ".claude/hooks/cargo-fallow-gate.sh";

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("a parent")).expect("create a folder");
    fs::write(full, text).expect("write a file");
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("run git");
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

/// A workspace with two members, `app` and `helper`, in a git repository with
/// one commit. No code has a finding.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\", \"helper\"]\nresolver = \"3\"\n",
    );
    write(
        root,
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    );
    write(root, "app/src/main.rs", "fn main() {}\n");
    write(
        root,
        "helper/Cargo.toml",
        "[package]\nname = \"helper\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    );
    write(root, "helper/src/lib.rs", "");
    git(root, &["init", "-q"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "init"]);
    dir
}

/// Adds a dependency on `helper` that no code of `app` uses.
fn add_unused_dependency(root: &Path) {
    write(
        root,
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[dependencies]\nhelper = { path = \"../helper\" }\n",
    );
}

fn cargo_fallow(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run the binary")
}

fn install(root: &Path) {
    let output = cargo_fallow(root, &["hooks", "install", "--target", "agent"]);
    assert!(output.status.success(), "{output:?}");
}

/// Runs the gate script with a payload. `bin` is the binary that the script
/// runs, and `env` holds more variables.
fn gate_with(root: &Path, payload: &str, bin: &str, env: &[(&str, &str)]) -> (i32, String) {
    let mut command = Command::new("bash");
    command
        .arg(root.join(SCRIPT))
        .current_dir(root)
        .env("CARGO_FALLOW_GATE_BIN", bin)
        .env_remove("CARGO_FALLOW_GATE_REPORT")
        .env_remove("CARGO_FALLOW_GATE_MIN_VERSION")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command.spawn().expect("run bash");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write the payload");
    let output = child.wait_with_output().expect("wait for the gate");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn payload(root: &Path, command: &str) -> String {
    json!({
        "session_id": "s",
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": root,
        "tool_input": { "command": command },
    })
    .to_string()
}

fn gate(root: &Path, command: &str) -> (i32, String) {
    gate_with(root, &payload(root, command), BIN, &[])
}

#[test]
fn a_commit_with_a_new_unused_dependency_is_blocked() {
    let dir = repo();
    install(dir.path());
    add_unused_dependency(dir.path());
    let (code, stderr) = gate(dir.path(), "git add -A && git commit -m 'add helper'");
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("deps/unused-dependency"), "{stderr}");
    assert!(stderr.contains("app/Cargo.toml"), "{stderr}");
    assert!(stderr.contains("error-severity-findings"), "{stderr}");
    let report = dir.path().join(".fallow/cargo-gate-report.json");
    assert!(
        stderr.contains(&report.canonicalize().expect("report").display().to_string())
            || stderr.contains(".fallow/cargo-gate-report.json"),
        "{stderr}"
    );
    let report: Value =
        serde_json::from_str(&fs::read_to_string(report).expect("read the report")).expect("JSON");
    assert_eq!(report["kind"], "check");
    assert_eq!(
        fs::read_to_string(dir.path().join(".fallow/.gitignore"))
            .ok()
            .as_deref(),
        Some("*\n")
    );
}

#[test]
fn a_push_with_a_new_finding_is_blocked() {
    let dir = repo();
    install(dir.path());
    git(dir.path(), &["branch", "base"]);
    add_unused_dependency(dir.path());
    git(dir.path(), &["commit", "-q", "-am", "add helper"]);
    let output = Command::new("git")
        .args(["branch", "--set-upstream-to=base"])
        .current_dir(dir.path())
        .output()
        .expect("run git");
    assert!(output.status.success(), "{output:?}");
    let (code, stderr) = gate(dir.path(), "git push origin main");
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("deps/unused-dependency"), "{stderr}");
}

#[test]
fn a_clean_commit_and_other_commands_pass() {
    let dir = repo();
    install(dir.path());
    write(
        dir.path(),
        "app/src/main.rs",
        "fn main() {\n    let _x = 1;\n}\n",
    );
    for command in [
        "git commit -am 'edit'",
        "git status",
        "cargo test",
        "git log commit.txt",
    ] {
        let (code, stderr) = gate(dir.path(), command);
        assert_eq!(code, 0, "{command}: {stderr}");
        assert!(stderr.is_empty(), "{command}: {stderr}");
    }
}

#[test]
fn a_finding_that_head_already_has_does_not_block() {
    let dir = repo();
    add_unused_dependency(dir.path());
    git(dir.path(), &["commit", "-q", "-am", "add helper"]);
    install(dir.path());
    // The commit changes the manifest with the old finding.
    let manifest = fs::read_to_string(dir.path().join("app/Cargo.toml")).expect("read");
    write(
        dir.path(),
        "app/Cargo.toml",
        &manifest.replace("version = \"0.1.0\"", "version = \"0.1.1\""),
    );
    let (code, stderr) = gate(dir.path(), "git commit -am 'bump'");
    assert_eq!(code, 0, "{stderr}");
    // A retry uses the cached result of the base commit.
    let (code, _) = gate(dir.path(), "git commit -am 'bump'");
    assert_eq!(code, 0);
    let caches: Vec<_> = fs::read_dir(dir.path().join(".fallow"))
        .expect("the cache folder")
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("cargo-gate-base-")
        })
        .collect();
    assert_eq!(caches.len(), 1);
}

#[test]
fn a_missing_binary_fails_open_with_one_line() {
    let dir = repo();
    install(dir.path());
    let (code, stderr) = gate_with(
        dir.path(),
        &payload(dir.path(), "git commit -m x"),
        "/nonexistent/cargo-fallow",
        &[("PATH", "/usr/bin:/bin"), ("CARGO_HOME", "/nonexistent")],
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("not found"), "{stderr}");
}

#[test]
fn a_bad_payload_fails_open_with_one_line() {
    let dir = repo();
    install(dir.path());
    for payload in ["{\"tool_input\": git commit", "[\"git commit\"]"] {
        let (code, stderr) = gate_with(dir.path(), payload, BIN, &[]);
        assert_eq!(code, 0, "{payload}: {stderr}");
        assert_eq!(stderr.lines().count(), 1, "{payload}: {stderr}");
        assert!(stderr.contains("invalid tool input"), "{payload}: {stderr}");
    }
}

/// Writes a fake binary that answers the probe like the real one and runs
/// `body` for each other call.
fn fake_binary(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("fake-cargo-fallow");
    let version = env!("CARGO_PKG_VERSION");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nif [ \"$3\" = probe ]; then echo \"cargo-fallow-gate-v1 {version}\"; exit 0; fi\n{body}\n"
        ),
    )
    .expect("write the fake binary");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

#[test]
fn a_crash_of_the_runtime_fails_open_with_one_line() {
    let dir = repo();
    install(dir.path());
    let bins = tempfile::tempdir().expect("a folder");
    for body in [
        "echo 'thread main panicked' >&2; echo 'note: backtrace' >&2; exit 101",
        "kill -9 $$",
        // Exit code 1 without the block line is not a block.
        "echo 'some error' >&2; exit 1",
        // A usage error of an older binary.
        "echo 'error: unrecognized subcommand' >&2; exit 2",
    ] {
        let fake = fake_binary(bins.path(), body);
        let (code, stderr) = gate_with(
            dir.path(),
            &payload(dir.path(), "git commit -m x"),
            fake.to_str().expect("UTF-8"),
            &[],
        );
        assert_eq!(code, 0, "{body}: {stderr}");
        assert_eq!(stderr.lines().count(), 1, "{body}: {stderr}");
        assert!(stderr.contains("skipping the check"), "{body}: {stderr}");
    }
}

#[test]
fn a_binary_without_the_runtime_is_below_the_floor() {
    let dir = repo();
    install(dir.path());
    let bins = tempfile::tempdir().expect("a folder");
    let old = bins.path().join("old-cargo-fallow");
    fs::write(
        &old,
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'cargo-fallow 0.1.0'; exit 0; fi\necho 'error: unrecognized subcommand' >&2\nexit 2\n",
    )
    .expect("write");
    fs::set_permissions(&old, fs::Permissions::from_mode(0o755)).expect("chmod");
    let old = old.to_str().expect("UTF-8");
    let commit = payload(dir.path(), "git commit -m x");
    let (code, stderr) = gate_with(dir.path(), &commit, old, &[]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("below the required"), "{stderr}");
    assert!(stderr.contains("cargo-fallow 0.1.0"), "{stderr}");
    let (code, stderr) = gate_with(
        dir.path(),
        &commit,
        old,
        &[("CARGO_FALLOW_GATE_MIN_VERSION", "")],
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
}

#[test]
fn a_report_path_with_a_link_is_not_written_through() {
    let dir = repo();
    install(dir.path());
    let outside = tempfile::tempdir().expect("a folder");
    std::os::unix::fs::symlink(outside.path(), dir.path().join(".fallow")).expect("a link");
    add_unused_dependency(dir.path());
    let (code, stderr) = gate(dir.path(), "git commit -am x");
    assert_eq!(code, 2, "{stderr}");
    assert_eq!(
        fs::read_dir(outside.path()).expect("read").count(),
        0,
        "the gate wrote through the link"
    );
    let named = stderr
        .lines()
        .find_map(|line| line.strip_prefix("cargo-fallow-gate: full report: "))
        .expect("the report line");
    assert!(named.contains("cargo-fallow-gate-report-"), "{stderr}");
    let _ = fs::remove_file(named);
}

#[test]
fn an_edited_script_is_not_replaced_without_force() {
    let dir = repo();
    install(dir.path());
    let script = dir.path().join(SCRIPT);
    let edited = fs::read_to_string(&script).expect("read") + "# my change\n";
    fs::write(&script, &edited).expect("write");
    let output = cargo_fallow(dir.path(), &["hooks", "install", "--target", "agent"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(fs::read_to_string(&script).ok().as_deref(), Some(&*edited));

    let output = cargo_fallow(
        dir.path(),
        &[
            "hooks",
            "uninstall",
            "--target",
            "agent",
            "--format",
            "json",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(report["files"][1]["outcome"], "kept", "{report}");
    assert!(script.exists());

    let output = cargo_fallow(
        dir.path(),
        &["hooks", "install", "--target", "agent", "--force"],
    );
    assert!(output.status.success(), "{output:?}");
    assert_ne!(fs::read_to_string(&script).ok().as_deref(), Some(&*edited));
}

#[test]
fn install_and_uninstall_keep_the_other_hooks() {
    let dir = repo();
    let settings = dir.path().join(".claude/settings.json");
    let fallow = json!({
        "permissions": { "allow": ["Bash(ls)"] },
        "hooks": { "PreToolUse": [{ "matcher": "Bash", "hooks": [
            { "type": "command", "command": "\"$CLAUDE_PROJECT_DIR\"/.claude/hooks/fallow-gate.sh" }
        ] }] },
    });
    write(dir.path(), ".claude/settings.json", &fallow.to_string());
    fs::create_dir_all(dir.path().join(".codex")).expect("a folder");
    let output = cargo_fallow(
        dir.path(),
        &["hooks", "install", "--target", "agent", "--format", "json"],
    );
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON");
    let paths: Vec<&str> = report["files"]
        .as_array()
        .expect("files")
        .iter()
        .filter_map(|file| file["path"].as_str())
        .collect();
    assert_eq!(
        paths,
        [
            SCRIPT,
            ".claude/settings.json",
            ".codex/hooks/cargo-fallow-gate.sh",
            ".codex/hooks.json"
        ]
    );
    let status = cargo_fallow(dir.path(), &["hooks", "status", "--format", "json"]);
    let status: Value = serde_json::from_slice(&status.stdout).expect("JSON");
    assert_eq!(status["surfaces"][0]["state"], "installed", "{status}");
    assert_eq!(status["surfaces"][1]["state"], "installed", "{status}");

    let output = cargo_fallow(dir.path(), &["hooks", "uninstall", "--target", "agent"]);
    assert!(output.status.success(), "{output:?}");
    let after: Value =
        serde_json::from_str(&fs::read_to_string(&settings).expect("read")).expect("JSON");
    assert_eq!(after, fallow);
    assert!(!dir.path().join(".codex/hooks.json").exists());
    assert!(!dir.path().join(SCRIPT).exists());
}

#[test]
fn the_git_hook_blocks_a_commit_with_a_new_finding() {
    let dir = repo();
    let output = cargo_fallow(dir.path(), &["hooks", "install", "--target", "git"]);
    assert!(output.status.success(), "{output:?}");
    let hook = dir.path().join(".git/hooks/pre-commit");
    assert!(hook.is_file());
    let bin_dir = Path::new(BIN).parent().expect("the binary folder");
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let commit = |message: &str| {
        Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-am",
                message,
            ])
            .current_dir(dir.path())
            .env("PATH", &path)
            .output()
            .expect("run git")
    };
    add_unused_dependency(dir.path());
    let blocked = commit("add helper");
    assert!(!blocked.status.success(), "{blocked:?}");
    assert!(
        String::from_utf8_lossy(&blocked.stderr).contains("deps/unused-dependency"),
        "{blocked:?}"
    );
    write(
        dir.path(),
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.1\"\nedition = \"2024\"\npublish = false\n",
    );
    let clean = commit("bump");
    assert!(clean.status.success(), "{clean:?}");

    let output = cargo_fallow(dir.path(), &["hooks", "uninstall", "--target", "git"]);
    assert!(output.status.success(), "{output:?}");
    assert!(!hook.exists());
}

#[test]
fn a_hook_manager_gets_a_step_and_no_file() {
    let dir = repo();
    write(dir.path(), "hk.pkl", "");
    let output = cargo_fallow(
        dir.path(),
        &["hooks", "install", "--target", "git", "--format", "json"],
    );
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(report["manager_step"]["manager"], "hk");
    assert!(!dir.path().join(".git/hooks/pre-commit").exists());
}
