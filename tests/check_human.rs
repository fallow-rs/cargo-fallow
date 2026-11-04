//! Black-box tests of the human output of `check`: the summary at the end,
//! the gate block and the hints of the sections.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;

use assert_cmd::Command;

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("a parent")).expect("create a folder");
    fs::write(full, text).expect("write a file");
}

/// Runs the binary and returns the exit code and stdout.
fn run(dir: &Path, args: &[&str]) -> (i32, String) {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run the binary");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

/// A function body that the clone check finds when it occurs twice.
const CLONE: &str = "    let mut total = 0;\n    for value in [1, 2, 3, 4, 5, 6, 7, 8] {\n        if value % 2 == 0 {\n            total += value * 3;\n        } else {\n            total -= value / 2;\n        }\n    }\n    println!(\"{total}\");\n    total\n";

/// A workspace with an unused file and a public item that no code uses.
fn workspace(clones: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    write(
        dir.path(),
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\", \"lib\"]\nresolver = \"3\"\n",
    );
    write(
        dir.path(),
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[dependencies]\nlib = { path = \"../lib\" }\n",
    );
    write(
        dir.path(),
        "app/src/main.rs",
        "fn main() { lib::used(); }\n",
    );
    write(
        dir.path(),
        "lib/Cargo.toml",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    );
    let mut lib = String::from("pub fn used() {}\npub fn unused() {}\n");
    if clones {
        lib.push_str(&format!(
            "pub fn first() -> i32 {{\n{CLONE}}}\npub fn second() -> i32 {{\n{CLONE}}}\n"
        ));
    }
    write(dir.path(), "lib/src/lib.rs", &lib);
    write(dir.path(), "lib/src/orphan.rs", "");
    dir
}

#[test]
fn the_output_ends_with_the_gates_that_fail_and_a_next_step() {
    let dir = workspace(false);
    let (code, text) = run(dir.path(), &["check", "--quiet"]);
    assert_eq!(code, 0, "{text}");
    let tail: Vec<&str> = text.trim_end().lines().rev().take(3).collect();
    assert!(
        tail.iter()
            .any(|line| line.contains("not enforced") && line.contains("--fail-on-issues")),
        "{text}"
    );
    assert!(
        tail.iter()
            .any(|line| line.contains("cargo fallow fix --dry-run")),
        "{text}"
    );

    let (code, text) = run(dir.path(), &["check", "--quiet", "--fail-on-issues"]);
    assert_eq!(code, 1, "{text}");
    let failed = text
        .lines()
        .find(|line| line.starts_with("Failed: "))
        .unwrap_or_else(|| panic!("no Failed line: {text}"));
    assert!(failed.contains("unused-files"), "{failed}");
    assert!(failed.contains("visibility-findings"), "{failed}");
}

#[test]
fn a_clean_run_has_no_next_step() {
    let dir = workspace(false);
    write(dir.path(), "lib/src/lib.rs", "pub fn used() {}\n");
    fs::remove_file(dir.path().join("lib/src/orphan.rs")).expect("remove a file");
    let (code, text) = run(dir.path(), &["check", "--quiet"]);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("No issues found"), "{text}");
    assert!(!text.contains("--fail-on-issues"), "{text}");
    assert!(!text.contains("fix --dry-run"), "{text}");
    assert!(!text.contains("Next:"), "{text}");
    assert!(!text.contains("Failed:"), "{text}");
}

#[test]
fn the_gate_block_does_not_repeat_the_files_of_the_findings() {
    let dir = workspace(false);
    let (_, text) = run(dir.path(), &["check", "--quiet"]);
    let gates = text.split("\nGates:").nth(1).expect("a gate block");
    assert!(!gates.contains("lib/src/lib.rs"), "{gates}");
}

#[test]
fn the_size_hint_names_a_command_that_check_accepts() {
    let dir = workspace(false);
    let (_, text) = run(dir.path(), &["check", "--quiet"]);
    assert!(!text.contains("Add `--base <ref>`"), "{text}");
    assert!(text.contains("cargo fallow size --base <ref>"), "{text}");
}

#[test]
fn dupes_does_not_say_no_clones_when_a_baseline_hides_them() {
    let dir = workspace(true);
    let (code, text) = run(dir.path(), &["dupes", "--quiet"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        !text.contains("No clones found"),
        "the fixture has a clone: {text}"
    );
    let (code, text) = run(
        dir.path(),
        &["dupes", "--quiet", "--save-baseline", "base.json"],
    );
    assert_eq!(code, 0, "{text}");
    let (code, text) = run(dir.path(), &["dupes", "--quiet", "--baseline", "base.json"]);
    assert_eq!(code, 0, "{text}");
    assert!(!text.contains("No clones found"), "{text}");
    assert!(
        text.contains("No clone group to report: the scope, the baseline"),
        "{text}"
    );
}

#[test]
fn fix_init_and_explain_list_only_the_formats_that_they_accept() {
    let dir = workspace(false);
    for command in ["fix", "init", "explain"] {
        let (code, text) = run(dir.path(), &[command, "--help"]);
        assert_eq!(code, 0, "{text}");
        assert!(!text.contains("sarif"), "{command}: {text}");
        assert!(text.contains("json"), "{command}: {text}");
    }
}

#[test]
fn a_save_run_does_not_ask_for_the_flag_that_it_has() {
    let dir = workspace(false);
    let (code, text) = run(
        dir.path(),
        &[
            "check",
            "--quiet",
            "--fail-on-issues",
            "--save-baseline",
            "base.json",
        ],
    );
    assert_eq!(code, 0, "{text}");
    assert!(!text.contains("Add `--fail-on-issues`"), "{text}");
    assert!(
        text.contains("A run with `--save-baseline` enforces no gate."),
        "{text}"
    );
}
