//! Black-box tests of the `cargo check` guard of the dependency edits of
//! `fix`.
//!
//! The macro crate `mac` is outside the workspace and relates to no member.
//! Its `uses_gamma!` macro writes a `::gamma::` path, so the removal of an
//! unused `gamma` entry breaks the build. The analysis cannot see that path,
//! so the edit of each entry of a table that `mac` serves is guarded. The
//! tests run the real `cargo check`.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// The workspace members that `app` can depend on.
const LIBS: &[&str] = &["gamma", "delta"];

/// The macros of `mac`: one writes a `::gamma::` path, one writes no path.
const MAC: &str = "#[macro_export]\nmacro_rules! uses_gamma { () => { pub fn from_gamma() { ::gamma::f(); } } }\n\
                   #[macro_export]\nmacro_rules! plain { () => { pub fn plain() {} } }\n";

/// Writes the workspace. `root_extra` follows the `[workspace]` table, and
/// `app_manifest` follows the `[package]` table of `app/Cargo.toml`.
fn workspace(root_extra: &str, app_manifest: &str, lib: &str) -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    write(
        dir.path(),
        "Cargo.toml",
        &format!(
            "[workspace]\nmembers = [\"app\", \"libs/gamma\", \"libs/delta\"]\nexclude = [\"ext\"]\nresolver = \"3\"\n{root_extra}"
        ),
    );
    write(
        dir.path(),
        "app/Cargo.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n{app_manifest}"
        ),
    );
    write(dir.path(), "app/src/lib.rs", lib);
    for (folder, text) in LIBS
        .iter()
        .map(|name| (format!("libs/{name}"), "pub fn f() {}\n"))
        .chain(std::iter::once(("ext/mac".to_owned(), MAC)))
    {
        let name = folder.rsplit('/').next().expect("a folder has a name");
        write(
            dir.path(),
            &format!("{folder}/Cargo.toml"),
            &format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n"
            ),
        );
        write(dir.path(), &format!("{folder}/src/lib.rs"), text);
    }
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("a file path has a parent")).expect("create a folder");
    fs::write(full, text).expect("write a file");
}

fn read(dir: &TempDir, path: &str) -> String {
    fs::read_to_string(dir.path().join(path)).expect("read a file")
}

fn dep(name: &str, folder: &str) -> String {
    format!("{name} = {{ path = \"../{folder}/{name}\" }}\n")
}

/// Runs `fix --yes --only deps` and returns the exit code, the report and
/// stderr.
fn fix(dir: &TempDir, env: &[(&str, &str)]) -> (i32, Value, String) {
    let mut command = Command::cargo_bin("cargo-fallow").expect("the binary exists");
    command.current_dir(dir.path()).args([
        "fix", "--yes", "--only", "deps", "--format", "json", "--quiet",
    ]);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().expect("run the binary");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is JSON ({err}): {}\nstderr: {stderr}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    (output.status.code().unwrap_or(-1), report, stderr)
}

/// The edit of `key` in `section`.
fn edit<'a>(report: &'a Value, section: &str, key: &str) -> &'a Value {
    report["fixes"]
        .as_array()
        .expect("fixes is an array")
        .iter()
        .find(|fix| fix["section"] == section && fix["dependency"] == key)
        .unwrap_or_else(|| panic!("no edit of {section} {key}: {report}"))
}

#[test]
fn the_guard_leaves_out_a_removal_that_breaks_the_build() {
    let manifest = format!(
        "[dependencies]\n{}gamma = {{ workspace = true }}\n{}",
        dep("mac", "ext"),
        dep("delta", "libs"),
    );
    let dir = workspace(
        "\n[workspace.dependencies]\ngamma = { path = \"libs/gamma\" }\n",
        &manifest,
        "mac::uses_gamma!();\n",
    );
    let (code, report, stderr) = fix(&dir, &[]);
    assert_eq!(code, 0, "{stderr}\n{report}");
    let gamma = edit(&report, "dependencies", "gamma");
    assert_eq!(gamma["guarded"], true, "{report}");
    assert_eq!(gamma["applied"], false, "{report}");
    assert_eq!(gamma["skip_reason"], "compiler_error", "{report}");
    let diagnostic = gamma["diagnostic"].as_str().unwrap_or_default();
    assert!(diagnostic.contains("`gamma`"), "{diagnostic}");
    // The member entry stays, so the workspace entry that it inherits stays.
    let inherited = edit(&report, "workspace.dependencies", "gamma");
    assert_eq!(inherited["applied"], false, "{report}");
    assert_eq!(inherited["skip_reason"], "compiler_error", "{report}");
    // `delta` builds without its entry, so the guard keeps its removal.
    assert_eq!(edit(&report, "dependencies", "delta")["applied"], true);
    assert_eq!(report["total_fixed"], 1, "{report}");
    assert_eq!(report["skipped"], 2, "{report}");
    let app = read(&dir, "app/Cargo.toml");
    assert!(app.contains("gamma = { workspace = true }"), "{app}");
    assert!(!app.contains("delta"), "{app}");
    assert!(read(&dir, "Cargo.toml").contains("gamma = { path"));
}

#[test]
fn the_guard_keeps_a_removal_that_builds() {
    let manifest = format!(
        "[dependencies]\n{}{}",
        dep("mac", "ext"),
        dep("gamma", "libs")
    );
    let dir = workspace("", &manifest, "mac::plain!();\n");
    let (code, report, stderr) = fix(&dir, &[]);
    assert_eq!(code, 0, "{stderr}\n{report}");
    let gamma = edit(&report, "dependencies", "gamma");
    assert_eq!(gamma["guarded"], true, "{report}");
    assert_eq!(gamma["applied"], true, "{report}");
    assert!(gamma.get("skip_reason").is_none(), "{report}");
    assert!(!read(&dir, "app/Cargo.toml").contains("gamma"));
}

#[test]
fn a_workspace_that_does_not_build_before_keeps_only_the_guarded_edits() {
    let manifest = format!(
        "[dependencies]\n{}{}\n[dev-dependencies]\n{}",
        dep("mac", "ext"),
        dep("gamma", "libs"),
        dep("delta", "libs"),
    );
    let dir = workspace(
        "",
        &manifest,
        "mac::plain!();\npub fn broken() -> u8 { \"x\" }\n",
    );
    let (code, report, stderr) = fix(&dir, &[]);
    assert_eq!(code, 0, "{stderr}\n{report}");
    let gamma = edit(&report, "dependencies", "gamma");
    assert_eq!(gamma["applied"], false, "{report}");
    assert_eq!(gamma["skip_reason"], "build_fails_before", "{report}");
    assert!(
        stderr.contains("does not build before the edits"),
        "{stderr}"
    );
    // No macro serves `[dev-dependencies]`, so its edit applies as before.
    let delta = edit(&report, "dev-dependencies", "delta");
    assert!(delta.get("guarded").is_none(), "{report}");
    assert_eq!(delta["applied"], true, "{report}");
    let app = read(&dir, "app/Cargo.toml");
    assert!(app.contains("gamma = { path"), "{app}");
    assert!(!app.contains("delta"), "{app}");
}

/// A fake cargo: `check` writes a marker file and fails, each other command
/// runs the real cargo. Returns the path of the script.
#[cfg(unix)]
fn marking_cargo(dir: &TempDir) -> String {
    use std::os::unix::fs::PermissionsExt as _;

    let real = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let script = dir.path().join("fake-cargo");
    let marker = dir.path().join("checked");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = check ] || [ \"$2\" = check ]; then\n  touch '{}'\n  exit 1\nfi\nexec '{real}' \"$@\"\n",
            marker.display()
        ),
    )
    .expect("write the script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("make it executable");
    script.to_str().expect("the path is UTF-8").to_owned()
}

#[cfg(unix)]
#[test]
fn a_run_without_a_guarded_edit_runs_no_check() {
    let manifest = format!(
        "[dependencies]\n{}{}",
        dep("mac", "ext"),
        dep("gamma", "libs")
    );
    let dir = workspace("", &manifest, "pub fn g() { mac::plain!(); }\n");
    let cargo = marking_cargo(&dir);
    let (code, report, stderr) = fix(&dir, &[("CARGO", &cargo)]);
    assert_eq!(code, 0, "{stderr}\n{report}");
    let gamma = edit(&report, "dependencies", "gamma");
    assert!(gamma.get("guarded").is_none(), "{report}");
    assert_eq!(gamma["applied"], true, "{report}");
    assert!(
        !dir.path().join("checked").exists(),
        "fix ran `cargo check`"
    );
}
