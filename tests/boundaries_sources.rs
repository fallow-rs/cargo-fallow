//! Black-box tests of the `allow` and `scope` keys of a
//! `boundaries.sources` rule.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// One call of the denied pattern.
const CALL: &str = "    let _ = Parser::new(1);\n";

/// A function with `count` calls.
fn calls(name: &str, count: usize) -> String {
    format!("pub fn {name}() {{\n{}}}\n", CALL.repeat(count))
}

/// Writes a crate with a source rule. `rule` holds the extra keys of the
/// rule. `src/parse.rs` has `parse` calls, `src/other.rs` has one call, and
/// test code has one call in each of these places: a `#[cfg(test)]` module in
/// `src/lib.rs`, the `#[cfg(test)]` file module `src/helpers.rs`, a `#[test]`
/// function in `src/other.rs` and the test target `tests/it.rs`.
fn workspace(rule: &str, parse: usize) -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[workspace]\n\n[[workspace.metadata.fallow.boundaries.sources]]\nrule = \"parse-through-one-place\"\npaths = [\"src\", \"tests\"]\ndeny = [\"Parser::new(\"]\n{rule}"
        ),
    );
    write(
        root,
        "src/lib.rs",
        &format!(
            "pub mod other;\npub mod parse;\n#[cfg(test)]\nmod helpers;\npub struct Parser;\nimpl Parser {{\n    pub fn new(_: u8) -> Self {{\n        Self\n    }}\n}}\n#[cfg(test)]\nmod tests {{\n    use super::*;\n    {}}}\n",
            calls("in_tests", 1)
        ),
    );
    write(
        root,
        "src/parse.rs",
        &format!("use crate::Parser;\n{}", calls("parse", parse)),
    );
    write(
        root,
        "src/other.rs",
        &format!(
            "use crate::Parser;\n{}#[test]\nfn check() {{\n{CALL}}}\n",
            calls("other", 1)
        ),
    );
    write(
        root,
        "src/helpers.rs",
        &format!("use crate::Parser;\n{}", calls("helper", 1)),
    );
    write(
        root,
        "tests/it.rs",
        &format!("use app::Parser;\n{}", calls("it", 1)),
    );
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("a file path has a parent")).expect("create a folder");
    fs::write(full, text).expect("write a file");
}

fn boundaries(dir: &TempDir) -> (i32, Value) {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args(["fallow", "boundaries", "--format", "json", "--quiet"])
        .output()
        .expect("run the binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report = serde_json::from_str(&stdout)
        .unwrap_or_else(|err| panic!("stdout is JSON ({err}): {stdout}"));
    (output.status.code().unwrap_or(-1), report)
}

/// The `path:line` of each finding.
fn places(report: &Value) -> Vec<String> {
    report["findings"]
        .as_array()
        .expect("the report has findings")
        .iter()
        .map(|finding| {
            format!(
                "{}:{}",
                finding["path"].as_str().unwrap_or_default(),
                finding["line"]
            )
        })
        .collect()
}

#[test]
fn without_the_new_keys_each_file_reports_its_first_match() {
    let (code, report) = boundaries(&workspace("", 2));
    assert_eq!(code, 0, "{report}");
    assert_eq!(
        places(&report),
        [
            "src/helpers.rs:3",
            "src/lib.rs:15",
            "src/other.rs:3",
            "src/parse.rs:3",
            "tests/it.rs:3"
        ],
        "{report:#}"
    );
}

#[test]
fn production_scope_skips_test_targets_test_modules_and_test_items() {
    let (code, report) = boundaries(&workspace("scope = \"production\"\n", 1));
    assert_eq!(code, 0, "{report}");
    assert_eq!(
        places(&report),
        ["src/other.rs:3", "src/parse.rs:3"],
        "{report:#}"
    );
}

#[test]
fn an_allowed_file_passes_up_to_its_count() {
    let rule = "scope = \"production\"\nallow = [{ path = \"src/parse.rs\", max = 2 }]\n";
    let (_, report) = boundaries(&workspace(rule, 2));
    assert_eq!(places(&report), ["src/other.rs:3"], "{report:#}");
}

#[test]
fn each_match_over_the_count_is_a_finding() {
    let rule = "scope = \"production\"\nallow = [{ path = \"src/parse.rs\", max = 2 }]\n";
    let (_, report) = boundaries(&workspace(rule, 4));
    assert_eq!(
        places(&report),
        ["src/other.rs:3", "src/parse.rs:5", "src/parse.rs:6"],
        "{report:#}"
    );
    let finding = &report["findings"][1];
    let message = finding["message"].as_str().unwrap_or_default();
    assert!(message.contains("over the allowed 2"), "{message}");
}

#[test]
fn an_allowed_path_that_does_not_exist_is_a_config_error() {
    let rule = "allow = [{ path = \"src/missing.rs\", max = 1 }]\n";
    let (code, report) = boundaries(&workspace(rule, 1));
    assert_eq!(code, 2, "{report}");
    assert_eq!(report["code"], "CARGO_FALLOW_INVALID_CONFIG", "{report}");
}

#[test]
fn an_unknown_scope_is_a_config_error() {
    let (code, report) = boundaries(&workspace("scope = \"tests\"\n", 1));
    assert_eq!(code, 2, "{report}");
    assert_eq!(report["code"], "CARGO_FALLOW_INVALID_CONFIG", "{report}");
}

#[test]
fn text_matching_in_production_scope_skips_test_items() {
    let (_, report) = boundaries(&workspace("scope = \"production\"\nmatch = \"text\"\n", 1));
    assert_eq!(
        places(&report),
        ["src/other.rs:3", "src/parse.rs:3"],
        "{report:#}"
    );
}

#[test]
fn a_crlf_file_in_production_scope_keeps_its_lines() {
    let dir = workspace("scope = \"production\"\n", 1);
    let other = fs::read_to_string(dir.path().join("src/other.rs")).expect("read a file");
    write(dir.path(), "src/other.rs", &other.replace('\n', "\r\n"));
    let (_, report) = boundaries(&dir);
    assert_eq!(
        places(&report),
        ["src/other.rs:3", "src/parse.rs:3"],
        "{report:#}"
    );
}

#[test]
fn a_file_that_does_not_parse_in_production_scope_is_not_judged() {
    let dir = workspace("scope = \"production\"\n", 1);
    write(
        dir.path(),
        "src/parse.rs",
        "fn parse( {\n    Parser::new(1);\n",
    );
    let (code, report) = boundaries(&dir);
    assert_eq!(code, 0, "{report}");
    assert_eq!(places(&report), ["src/other.rs:3"], "{report:#}");
    assert_eq!(report["summary"]["source_files"], 2, "{report:#}");
}

#[test]
fn max_zero_reports_each_place_and_two_patterns_at_one_place_count_once() {
    let rule = "deny = [\"Parser::new(\", \"Parser::new(1\"]\nallow = [{ path = \"src/parse.rs\", max = 0 }]\n";
    let dir = workspace("", 2);
    let manifest = fs::read_to_string(dir.path().join("Cargo.toml")).expect("read a file");
    write(
        dir.path(),
        "Cargo.toml",
        &format!(
            "{}{rule}",
            manifest.replace("deny = [\"Parser::new(\"]\n", "")
        ),
    );
    let (_, report) = boundaries(&dir);
    let parse: Vec<String> = places(&report)
        .into_iter()
        .filter(|place| place.starts_with("src/parse.rs"))
        .collect();
    assert_eq!(parse, ["src/parse.rs:3", "src/parse.rs:4"], "{report:#}");
}

#[test]
fn an_allowed_file_must_be_read_by_the_rule_and_listed_one_time() {
    for rule in [
        "allow = [{ path = \"Cargo.toml\", max = 1 }]\n",
        "exclude = [\"src/parse.rs\"]\nallow = [{ path = \"src/parse.rs\", max = 1 }]\n",
        "allow = [{ path = \"src/parse.rs\", max = 1 }, { path = \"./src/parse.rs\", max = 2 }]\n",
    ] {
        let (code, report) = boundaries(&workspace(rule, 1));
        assert_eq!(code, 2, "{rule}: {report}");
        assert_eq!(report["code"], "CARGO_FALLOW_INVALID_CONFIG", "{report}");
    }
    let rule = "allow = [{ path = \"./src/parse.rs\", max = 1 }]\n";
    let (code, report) = boundaries(&workspace(rule, 1));
    assert_eq!(code, 0, "{report}");
    assert!(
        !places(&report).contains(&"src/parse.rs:3".to_owned()),
        "{report:#}"
    );
}
