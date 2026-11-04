//! Black-box tests of `init` and of the `baseline` key of the configuration.
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

/// A package with an unused file, a public item that no code uses and an
/// unused dependency: findings of `files`, `visibility` and `deps`.
fn package() -> tempfile::TempDir {
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
    write(dir.path(), "app/src/main.rs", "fn main() {}\n");
    write(
        dir.path(),
        "lib/Cargo.toml",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    );
    write(dir.path(), "lib/src/lib.rs", "pub fn unused() {}\n");
    write(dir.path(), "lib/src/orphan.rs", "");
    dir
}

#[test]
fn write_then_check_passes_and_a_new_finding_fails() {
    let dir = package();
    let (code, _) = run(
        dir.path(),
        &["check", "--fail-on-issues", "--quiet", "--format", "json"],
    );
    assert_eq!(code, 1, "the fixture has findings");
    let (code, text) = run(dir.path(), &["init", "--write", "--quiet"]);
    assert_eq!(code, 0, "{text}");
    assert!(dir.path().join("fallow-baseline.json").is_file());
    let manifest = fs::read_to_string(dir.path().join("Cargo.toml")).expect("read the manifest");
    assert!(
        manifest.contains("baseline = \"fallow-baseline.json\""),
        "{manifest}"
    );
    let (code, report) = run(
        dir.path(),
        &["check", "--fail-on-issues", "--quiet", "--format", "json"],
    );
    assert_eq!(code, 0, "{report}");
    write(dir.path(), "lib/src/second.rs", "");
    let (code, _) = run(
        dir.path(),
        &["check", "--fail-on-issues", "--quiet", "--format", "json"],
    );
    assert_eq!(code, 1, "a new unused file fails the run");
}

#[test]
fn a_second_write_changes_nothing() {
    let dir = package();
    let (code, _) = run(dir.path(), &["init", "--write", "--quiet"]);
    assert_eq!(code, 0);
    let manifest = fs::read(dir.path().join("Cargo.toml")).expect("read the manifest");
    let baseline = fs::read(dir.path().join("fallow-baseline.json")).expect("read the baseline");
    let (code, text) = run(
        dir.path(),
        &["init", "--write", "--quiet", "--format", "json"],
    );
    assert_eq!(code, 0, "{text}");
    let report: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(report["written"], false, "{report}");
    assert_eq!(
        fs::read(dir.path().join("Cargo.toml")).expect("read"),
        manifest
    );
    assert_eq!(
        fs::read(dir.path().join("fallow-baseline.json")).expect("read"),
        baseline
    );
}

#[test]
fn print_changes_no_file_and_ci_gives_a_workflow() {
    let dir = package();
    let before = fs::read(dir.path().join("Cargo.toml")).expect("read the manifest");
    let (code, text) = run(dir.path(), &["init", "--quiet"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.contains("[workspace.metadata.fallow.complexity]"),
        "{text}"
    );
    assert_eq!(
        fs::read(dir.path().join("Cargo.toml")).expect("read"),
        before
    );
    assert!(!dir.path().join("fallow-baseline.json").exists());
    let (code, workflow) = run(dir.path(), &["init", "--ci", "github"]);
    assert_eq!(code, 0);
    assert!(
        workflow.contains("uses: fallow-rs/cargo-fallow@"),
        "{workflow}"
    );
}
