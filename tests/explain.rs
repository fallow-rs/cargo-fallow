//! Black-box tests of `explain` and `--explain`.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;

use assert_cmd::Command;
use serde_json::Value;

fn run(dir: &std::path::Path, args: &[&str]) -> (i32, Value) {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run the binary");
    let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is JSON ({err}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    (output.status.code().unwrap_or(-1), value)
}

#[test]
fn explain_gives_the_rule_in_json() {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let (code, rule) = run(
        dir.path(),
        &["explain", "unused_dependency", "--format", "json"],
    );
    assert_eq!(code, 0, "{rule}");
    let keys: Vec<&str> = rule
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "rule", "check", "severity", "meaning", "example", "fix", "docs"
        ]
    );
    assert_eq!(rule["rule"], "unused-dependency");
    assert_eq!(rule["check"], "deps");
    assert_eq!(rule["severity"], "error");
}

#[test]
fn unknown_rule_exits_2_and_lists_the_rules() {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let (code, error) = run(dir.path(), &["explain", "nothing", "--format", "json"]);
    assert_eq!(code, 2);
    assert_eq!(error["code"], "CARGO_FALLOW_INVALID_ARGUMENT");
    let help = error["help"].as_str().unwrap_or_default();
    assert!(
        help.contains("unused-file") && help.contains("dead_public"),
        "{help}"
    );
}

#[test]
fn explain_flag_adds_the_rules_of_the_findings() {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let write = |path: &str, text: &str| {
        let full = dir.path().join(path);
        fs::create_dir_all(full.parent().expect("a parent")).expect("create a folder");
        fs::write(full, text).expect("write a file");
    };
    write(
        "Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    );
    write("src/lib.rs", "");
    write("src/orphan.rs", "");
    let (_, report) = run(
        dir.path(),
        &["files", "--format", "json", "--quiet", "--explain"],
    );
    assert_eq!(
        report["_meta"]["rules"]["unused-file"]["check"], "files",
        "{report}"
    );
    let (_, check) = run(
        dir.path(),
        &[
            "check",
            "--format",
            "json",
            "--quiet",
            "--only",
            "files",
            "--explain",
        ],
    );
    assert_eq!(
        check["_meta"]["rules"]["unused-file"]["fix"].is_string(),
        true,
        "{check}"
    );
    let (_, plain) = run(dir.path(), &["files", "--format", "json", "--quiet"]);
    assert!(plain.get("_meta").is_none());
}
