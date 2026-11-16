//! Black-box tests of a `dupes` baseline after an edit inside a saved clone.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

const BASELINE: &str = "base.json";

/// A function body that is long enough to be a clone. `extra` goes into the
/// middle of the body.
fn body(name: &str, extra: &str) -> String {
    format!(
        "pub fn {name}(items: &[u32]) -> u32 {{\n    let mut total = 0;\n    for item in items {{\n        if *item > 10 {{\n            total += item * 2;\n        }} else {{\n            total += item + 1;\n        }}\n    }}\n{extra}    let extra = items.iter().filter(|x| **x % 3 == 0).count() as u32;\n    total + extra * 7\n}}\n"
    )
}

/// Another body that is long enough to be a clone, with no shared tokens of
/// `body`.
fn other_body(name: &str) -> String {
    format!(
        "pub fn {name}(text: &str) -> Vec<String> {{\n    let words: Vec<&str> = text.split_whitespace().collect();\n    let mut out = Vec::new();\n    for word in words {{\n        match word.len() {{\n            0 => continue,\n            1 | 2 => out.push(word.to_uppercase()),\n            _ => out.push(word.chars().rev().collect()),\n        }}\n    }}\n    out.sort();\n    out.dedup();\n    out\n}}\n"
    )
}

/// A crate with the same function in `src/a.rs` and `src/b.rs`, and a saved
/// baseline.
fn workspace() -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"copies\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[workspace]\n\n[workspace.metadata.fallow.dupes]\nmin-tokens = 30\nmin-lines = 3\n",
    );
    write(
        root,
        "src/lib.rs",
        "pub mod a;\npub mod b;\npub mod c;\npub mod d;\npub mod e;\n",
    );
    write(root, "src/a.rs", &format!("// a\n\n{}", body("first", "")));
    write(root, "src/b.rs", &format!("{}\n", body("second", "")));
    write(root, "src/c.rs", "");
    write(root, "src/d.rs", "");
    write(root, "src/e.rs", "");
    let (code, report) = dupes(&dir, &["--save-baseline", BASELINE]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(groups(&dir, &[]), 1, "the setup has one clone group");
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("a file path has a parent")).expect("create a folder");
    fs::write(full, text).expect("write a file");
}

fn dupes(dir: &TempDir, args: &[&str]) -> (i32, Value) {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args(["fallow", "dupes", "--format", "json", "--quiet"])
        .args(args)
        .output()
        .expect("run the binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report = serde_json::from_str(&stdout)
        .unwrap_or_else(|err| panic!("stdout is JSON ({err}): {stdout}"));
    (output.status.code().unwrap_or(-1), report)
}

fn groups(dir: &TempDir, args: &[&str]) -> usize {
    let (_, report) = dupes(dir, args);
    report["findings"].as_array().map_or(0, Vec::len)
}

/// Runs with the baseline and `--fail-on-issues`.
fn gated(dir: &TempDir) -> (i32, Value) {
    dupes(
        dir,
        &[
            "--baseline",
            BASELINE,
            "--fail-on-issues",
            "--fail-on-stale-baseline",
        ],
    )
}

const ADDED: &str = "    total += items.len() as u32;\n    total -= 1;\n";

#[test]
fn an_edit_inside_both_copies_keeps_the_saved_clone() {
    let dir = workspace();
    write(
        dir.path(),
        "src/a.rs",
        &format!("// a\n\n{}", body("first", ADDED)),
    );
    write(
        dir.path(),
        "src/b.rs",
        &format!("{}\n", body("second", ADDED)),
    );
    let (code, report) = gated(&dir);
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["findings"], Value::Array(Vec::new()), "{report}");
    assert_eq!(report["baseline"]["matched"], 1, "{report}");
    assert_eq!(report["baseline"]["stale"], Value::Array(Vec::new()));
}

#[test]
fn an_edit_inside_one_copy_keeps_the_saved_clone() {
    let dir = workspace();
    write(
        dir.path(),
        "src/a.rs",
        &format!("// a\n\n{}", body("first", "    total -= 1;\n")),
    );
    let (code, report) = gated(&dir);
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["findings"], Value::Array(Vec::new()), "{report}");
}

#[test]
fn a_new_clone_in_other_files_still_fails() {
    let dir = workspace();
    write(dir.path(), "src/c.rs", &other_body("third"));
    write(dir.path(), "src/d.rs", &other_body("fourth"));
    let (code, report) = gated(&dir);
    assert_eq!(code, 1, "{report}");
    assert_eq!(report["findings"].as_array().map(Vec::len), Some(1));
}

#[test]
fn a_new_clone_beside_a_saved_clone_in_the_same_files_still_fails() {
    let dir = workspace();
    write(
        dir.path(),
        "src/a.rs",
        &format!("// a\n\n{}\n{}", body("first", ""), other_body("third")),
    );
    write(
        dir.path(),
        "src/b.rs",
        &format!("{}\n{}", body("second", ""), other_body("fourth")),
    );
    let (code, report) = gated(&dir);
    assert_eq!(code, 1, "{report}");
}

#[test]
fn a_third_copy_of_an_edited_clone_still_fails() {
    let dir = workspace();
    write(
        dir.path(),
        "src/a.rs",
        &format!("// a\n\n{}", body("first", ADDED)),
    );
    write(
        dir.path(),
        "src/b.rs",
        &format!("{}\n", body("second", ADDED)),
    );
    write(dir.path(), "src/e.rs", &body("third", ADDED));
    let (code, report) = gated(&dir);
    assert_eq!(code, 1, "{report}");
}

#[test]
fn an_edit_above_and_inside_both_copies_keeps_the_saved_clone() {
    let dir = workspace();
    let header = format!(
        "// a\n\n{}\n{}",
        other_body("helper"),
        "// pad\n".repeat(40)
    );
    write(
        dir.path(),
        "src/a.rs",
        &format!("{header}{}", body("first", ADDED)),
    );
    write(
        dir.path(),
        "src/b.rs",
        &format!("{}{}\n", "// pad\n".repeat(60), body("second", ADDED)),
    );
    let (code, report) = gated(&dir);
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["findings"], Value::Array(Vec::new()), "{report}");
    assert_eq!(report["baseline"]["stale"], Value::Array(Vec::new()));
}

#[test]
fn an_unchanged_third_copy_still_fails() {
    let dir = workspace();
    write(dir.path(), "src/e.rs", &body("third", ""));
    let (code, report) = gated(&dir);
    assert_eq!(code, 1, "{report}");
}

#[test]
fn a_new_clone_added_inside_the_functions_of_an_edited_clone_still_fails() {
    let dir = workspace();
    let block = "    let words: Vec<&str> = vec![\"a\", \"bb\", \"ccc\"];\n    let mut out = Vec::new();\n    for word in words {\n        match word.len() {\n            0 => continue,\n            1 | 2 => out.push(word.to_uppercase()),\n            _ => out.push(word.chars().rev().collect::<String>()),\n        }\n    }\n    out.sort();\n    out.dedup();\n    total += out.len() as u32;\n";
    // Different code before the new block in each copy keeps the new block
    // apart from the saved clone.
    let first = format!("{ADDED}    if items.is_empty() {{ return 0; }}\n{block}");
    let second = format!("{ADDED}    let unused = Some(3).map_or(0, |x| x + 1);\n{block}");
    write(
        dir.path(),
        "src/a.rs",
        &format!("// a\n\n{}", body("first", &first)),
    );
    write(
        dir.path(),
        "src/b.rs",
        &format!("{}\n", body("second", &second)),
    );
    assert!(
        groups(&dir, &[]) >= 2,
        "the run finds the old and the new clone"
    );
    let (code, report) = gated(&dir);
    assert_eq!(code, 1, "{report}");
}
