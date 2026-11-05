//! Black-box tests of `agent install`, `agent status`, `agent uninstall` and
//! `init --agents`.
#![cfg(unix)]
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_cargo-fallow");
const FALLOW_BLOCK: &str =
    "<!-- fallow:setup-hooks:start -->\nfallow text\n<!-- fallow:setup-hooks:end -->\n";

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("a parent")).expect("create a folder");
    fs::write(full, text).expect("write a file");
}

/// A package in a git repository, with an `AGENTS.md` that holds the block
/// of fallow for JavaScript, and a `.claude` and a `.codex` folder.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    );
    write(root, "src/main.rs", "fn main() {}\n");
    write(root, "AGENTS.md", &format!("# Guide\n\n{FALLOW_BLOCK}"));
    fs::create_dir_all(root.join(".codex")).expect("a folder");
    fs::create_dir_all(root.join(".claude")).expect("a folder");
    let output = Command::new("git")
        .args(["init", "-q"])
        .current_dir(root)
        .output()
        .expect("run git");
    assert!(output.status.success(), "{output:?}");
    dir
}

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(root)
        .output()
        .expect("run the binary")
}

fn json(root: &Path, args: &[&str]) -> Value {
    let output = run(root, args);
    assert!(output.status.success(), "{args:?}: {output:?}");
    serde_json::from_slice(&output.stdout).expect("JSON")
}

fn outcomes(report: &Value) -> Vec<(String, String)> {
    report["files"]
        .as_array()
        .expect("files")
        .iter()
        .map(|file| {
            (
                file["path"].as_str().unwrap_or_default().to_owned(),
                file["outcome"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

#[test]
fn install_writes_each_part_once_and_status_shows_them() {
    let dir = repo();
    let root = dir.path();
    let report = json(root, &["agent", "install", "--format", "json"]);
    let paths: Vec<String> = outcomes(&report)
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    assert_eq!(
        paths,
        [
            "AGENTS.md",
            ".claude/skills/cargo-fallow/SKILL.md",
            ".agents/skills/cargo-fallow/SKILL.md",
            ".claude/hooks/cargo-fallow-gate.sh",
            ".claude/settings.json",
            ".codex/hooks/cargo-fallow-gate.sh",
            ".codex/hooks.json",
        ]
    );
    let guide = fs::read_to_string(root.join("AGENTS.md")).expect("read");
    assert!(
        guide.starts_with(&format!("# Guide\n\n{FALLOW_BLOCK}")),
        "{guide}"
    );
    assert!(
        guide.contains("<!-- cargo-fallow:agents:start -->"),
        "{guide}"
    );
    let skill =
        fs::read_to_string(root.join(".claude/skills/cargo-fallow/SKILL.md")).expect("read");
    assert!(skill.starts_with("---\nname: cargo-fallow\n"), "{skill}");

    let again = json(root, &["agent", "install", "--format", "json"]);
    assert!(
        outcomes(&again)
            .iter()
            .all(|(_, outcome)| outcome == "unchanged"),
        "{again}"
    );

    let status = json(root, &["agent", "status", "--format", "json"]);
    assert_eq!(status["guide"], true, "{status}");
    assert_eq!(status["skills"][0]["state"], "current", "{status}");
    assert_eq!(status["skills"][1]["state"], "current", "{status}");
    assert_eq!(
        status["gate"]["surfaces"][0]["state"], "installed",
        "{status}"
    );
}

#[test]
fn uninstall_keeps_an_edited_skill_and_the_other_block() {
    let dir = repo();
    let root = dir.path();
    json(root, &["agent", "install", "--format", "json"]);
    let skill = root.join(".claude/skills/cargo-fallow/SKILL.md");
    let mut edited = fs::read_to_string(&skill).expect("read");
    edited.push_str("\nMy note.\n");
    fs::write(&skill, &edited).expect("write");
    let status = json(root, &["agent", "status", "--format", "json"]);
    assert_eq!(status["skills"][0]["state"], "edited", "{status}");

    let report = json(root, &["agent", "uninstall", "--format", "json"]);
    let outcomes = outcomes(&report);
    assert!(
        outcomes.contains(&(
            ".claude/skills/cargo-fallow/SKILL.md".to_owned(),
            "kept".to_owned()
        )),
        "{report}"
    );
    assert!(
        outcomes.contains(&("AGENTS.md".to_owned(), "removed".to_owned())),
        "{report}"
    );
    assert_eq!(fs::read_to_string(&skill).ok().as_deref(), Some(&*edited));
    assert_eq!(
        fs::read_to_string(root.join("AGENTS.md")).ok().as_deref(),
        Some(&*format!("# Guide\n\n{FALLOW_BLOCK}"))
    );
    assert!(!root.join(".agents/skills/cargo-fallow/SKILL.md").exists());
    assert!(!root.join(".claude/hooks/cargo-fallow-gate.sh").exists());
    assert!(!root.join(".codex/hooks.json").exists());
}

#[test]
fn without_leaves_out_parts_and_init_agents_installs() {
    let dir = repo();
    let root = dir.path();
    let report = json(
        root,
        &[
            "agent",
            "install",
            "--harness",
            "claude",
            "--without",
            "guide,gate",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        outcomes(&report),
        [(
            ".claude/skills/cargo-fallow/SKILL.md".to_owned(),
            "created".to_owned()
        )]
    );
    let report = json(root, &["init", "--agents", "--format", "json"]);
    assert_eq!(report["kind"], "agent", "{report}");
    assert!(root.join(".codex/hooks/cargo-fallow-gate.sh").is_file());
    let output = run(root, &["init", "--agents", "--write"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}
