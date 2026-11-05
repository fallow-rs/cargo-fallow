//! Black-box tests of `complexity.cyclomatic-policy`.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// A lookup table: a flat `match` with five arms and no branch in an arm.
const TABLE: &str = "pub fn width(c: char) -> u8 {\n    match c {\n        'a' => 1,\n        'b' => 2,\n        'c' | 'd' => 3,\n        'e' => 4,\n        _ => 0,\n    }\n}\n";

/// A `match` with a guard, so the arms are not a plain table.
const GUARDED: &str = "pub fn sign(n: i32) -> i8 {\n    match n {\n        0 => 0,\n        1 => 1,\n        2 => 1,\n        x if x > 0 => 1,\n        _ => -1,\n    }\n}\n";

/// A `match` with a branch in an arm.
const BRANCHED: &str = "pub fn pick(n: u8, b: bool) -> u8 {\n    match n {\n        0 => 0,\n        1 => 1,\n        2 => if b { 2 } else { 3 },\n        3 => 4,\n        _ => 5,\n    }\n}\n";

/// Writes a one-crate workspace with `complexity` settings and `code`.
fn workspace(settings: &str, code: &str) -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    write(
        dir.path(),
        "Cargo.toml",
        &format!(
            "[package]\nname = \"table\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[workspace]\n\n[workspace.metadata.fallow.complexity]\nmax-cyclomatic = 3\n{settings}"
        ),
    );
    write(dir.path(), "src/lib.rs", code);
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("a file path has a parent")).expect("create a folder");
    fs::write(full, text).expect("write a file");
}

fn complexity(dir: &TempDir, args: &[&str]) -> (i32, Value) {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args(["fallow", "complexity", "--format", "json", "--quiet"])
        .args(args)
        .output()
        .expect("run the binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report = serde_json::from_str(&stdout)
        .unwrap_or_else(|err| panic!("stdout is JSON ({err}): {stdout}"));
    (output.status.code().unwrap_or(-1), report)
}

/// The `(name, cyclomatic)` of each finding.
fn findings(report: &Value) -> Vec<(String, u64)> {
    report["findings"]
        .as_array()
        .expect("the report has findings")
        .iter()
        .map(|finding| {
            (
                finding["name"].as_str().unwrap_or_default().to_owned(),
                finding["cyclomatic"].as_u64().unwrap_or_default(),
            )
        })
        .collect()
}

#[test]
fn the_default_policy_counts_each_arm() {
    let dir = workspace("", TABLE);
    let (code, report) = complexity(&dir, &[]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(findings(&report), [("width".to_owned(), 5)], "{report}");
    assert!(report["findings"][0].get("standard_cyclomatic").is_none());
}

#[test]
fn flat_match_counts_a_table_as_one_decision() {
    let dir = workspace("cyclomatic-policy = \"flat-match\"\n", TABLE);
    let (code, report) = complexity(&dir, &["--fail-on-complexity"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(findings(&report), [], "{report}");
}

#[test]
fn flat_match_reports_the_standard_count_of_a_finding() {
    // Two tables and one `if`: each adds 1, so the gate value is 4.
    let code = "pub fn two(c: char, d: char) -> u8 {\n    let a = match c { 'a' => 1, 'b' => 2, 'c' => 3, _ => 0 };\n    let b = match d { 'a' => 1, 'b' => 2, 'c' => 3, _ => 0 };\n    if a > b { a } else { b }\n}\n";
    let dir = workspace("cyclomatic-policy = \"flat-match\"\n", code);
    let (_, report) = complexity(&dir, &["--max-cyclomatic", "2"]);
    assert_eq!(findings(&report), [("two".to_owned(), 4)], "{report}");
    assert_eq!(report["findings"][0]["standard_cyclomatic"], 8, "{report}");
}

#[test]
fn flat_match_keeps_the_arm_count_of_a_guarded_match() {
    let dir = workspace("cyclomatic-policy = \"flat-match\"\n", GUARDED);
    let (_, report) = complexity(&dir, &[]);
    assert_eq!(findings(&report), [("sign".to_owned(), 6)], "{report}");
}

#[test]
fn flat_match_keeps_the_arm_count_of_a_match_with_a_branch_in_an_arm() {
    let dir = workspace("cyclomatic-policy = \"flat-match\"\n", BRANCHED);
    let (_, report) = complexity(&dir, &[]);
    assert_eq!(findings(&report), [("pick".to_owned(), 6)], "{report}");
}

#[test]
fn an_unknown_policy_is_a_config_error() {
    let dir = workspace("cyclomatic-policy = \"one\"\n", TABLE);
    let (code, report) = complexity(&dir, &[]);
    assert_eq!(code, 2, "{report}");
    assert_eq!(report["code"], "CARGO_FALLOW_INVALID_CONFIG", "{report}");
}
