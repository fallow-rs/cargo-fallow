//! Black-box tests of the suppression comments across the checks.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// Writes a workspace with the library `core` and the binary `app`, which
/// calls `core::used`. `root_extra` goes after the `[workspace]` table.
fn workspace(root_extra: &str, files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    write(
        dir.path(),
        "Cargo.toml",
        &format!("[workspace]\nmembers = [\"core\", \"app\"]\nresolver = \"3\"\n{root_extra}"),
    );
    write(
        dir.path(),
        "core/Cargo.toml",
        "[package]\nname = \"core-lib\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[lib]\nname = \"core_lib\"\n",
    );
    write(
        dir.path(),
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[dependencies]\ncore-lib = { path = \"../core\" }\n",
    );
    write(
        dir.path(),
        "app/src/main.rs",
        "fn main() { core_lib::used(); }\n",
    );
    for (path, text) in files {
        write(dir.path(), path, text);
    }
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("a file path has a parent")).expect("create a folder");
    fs::write(full, text).expect("write a file");
}

/// Runs the command with `--format json --quiet` and returns the report and
/// the stderr text.
fn run(dir: &TempDir, args: &[&str]) -> (Value, String) {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args(args)
        .args(["--format", "json", "--quiet"])
        .output()
        .expect("run the binary");
    let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is JSON ({err}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    (report, String::from_utf8_lossy(&output.stderr).into_owned())
}

/// The `rule path` of each finding.
fn findings(report: &Value) -> Vec<String> {
    report["findings"]
        .as_array()
        .expect("findings is an array")
        .iter()
        .map(|finding| {
            let path = finding["file"]
                .as_str()
                .or_else(|| finding["path"].as_str())
                .or_else(|| finding["location"]["path"].as_str())
                .unwrap_or("?");
            let rule = finding["kind"]
                .as_str()
                .or_else(|| finding["rule"].as_str())
                .unwrap_or("?");
            format!("{rule} {path}")
        })
        .collect()
}

const CORE: &str = "pub fn used() { helper(); }\n\
                    // fallow-ignore-next-line visibility -- kept for a plugin\n\
                    #[inline]\n\
                    pub fn helper() {}\n\
                    pub fn other() {}\n";

#[test]
fn next_line_comment_hides_a_visibility_finding_and_counts_it() {
    let dir = workspace("", &[("core/src/lib.rs", CORE)]);
    let (report, _) = run(&dir, &["visibility"]);
    assert_eq!(
        findings(&report),
        ["dead_public core/src/lib.rs"],
        "{report}"
    );
    assert_eq!(report["findings"][0]["item"], "other");
    assert_eq!(report["suppressed"], 1);
}

#[test]
fn file_comment_hides_each_finding_of_the_kind_in_the_file() {
    let core = format!("/* fallow-ignore-file dead_public */\n{CORE}");
    let dir = workspace("", &[("core/src/lib.rs", &core)]);
    let (report, _) = run(&dir, &["visibility"]);
    assert_eq!(findings(&report), Vec::<String>::new(), "{report}");
}

#[test]
fn comment_that_hides_nothing_is_a_stale_suppression() {
    let core = "pub fn used() {}\n// fallow-ignore-next-line visibility\nfn private() {}\n";
    let dir = workspace("", &[("core/src/lib.rs", core)]);
    let (report, _) = run(&dir, &["visibility"]);
    let stale = &report["stale_suppressions"][0];
    assert_eq!(stale["path"], "core/src/lib.rs", "{report}");
    assert_eq!(stale["line"], 2);
    // In `check`, the warning is a finding in the shared shape.
    let (check, _) = run(&dir, &["check", "--only", "visibility"]);
    let rules: Vec<&str> = check["findings"]
        .as_array()
        .expect("findings is an array")
        .iter()
        .filter_map(|finding| finding["rule"].as_str())
        .collect();
    assert_eq!(rules, ["stale-suppression"], "{check}");
    assert_eq!(check["findings"][0]["severity"], "warn");
}

#[test]
fn unknown_kind_gives_a_warning() {
    let core = "pub fn used() {}\n// fallow-ignore-next-line no-such-rule\nfn private() {}\n";
    let dir = workspace("", &[("core/src/lib.rs", core)]);
    let (_, stderr) = run(&dir, &["visibility"]);
    assert!(stderr.contains("`no-such-rule`"), "{stderr}");
}

#[test]
fn manifest_comment_hides_a_dependency_finding() {
    let dir = workspace("", &[("core/src/lib.rs", "pub fn used() {}\n")]);
    let manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[dependencies]\n# fallow-ignore-next-line unused-dependency\ncore-lib = { path = \"../core\" }\n";
    write(dir.path(), "app/Cargo.toml", manifest);
    write(dir.path(), "app/src/main.rs", "fn main() {}\n");
    let (report, _) = run(&dir, &["deps"]);
    assert_eq!(findings(&report), Vec::<String>::new(), "{report}");
    assert_eq!(report["suppressed"], 1);
}

#[test]
fn file_comment_hides_an_unused_file() {
    let dir = workspace(
        "",
        &[
            ("core/src/lib.rs", "pub fn used() {}\n"),
            ("core/src/kept.rs", "// fallow-ignore-file unused-file\n"),
            ("core/src/orphan.rs", ""),
        ],
    );
    let (report, _) = run(&dir, &["files"]);
    assert_eq!(
        findings(&report),
        ["unused-file core/src/orphan.rs"],
        "{report}"
    );
}

#[test]
fn next_line_comment_hides_a_boundary_violation() {
    let rules = "\n[workspace.metadata.fallow.boundaries]\nsources = [{ rule = \"no-exit\", paths = [\"core/src\"], deny = [\"std::process::exit\"], message = \"no exit\" }]\n";
    let core = "mod other;\npub fn used() {\n    // fallow-ignore-next-line no-exit\n    std::process::exit(0);\n}\n";
    let other = "pub fn stop() {\n    std::process::exit(1);\n}\n";
    let dir = workspace(
        rules,
        &[("core/src/lib.rs", core), ("core/src/other.rs", other)],
    );
    let (report, stderr) = run(&dir, &["boundaries"]);
    let places: Vec<String> = report["findings"]
        .as_array()
        .expect("findings is an array")
        .iter()
        .map(|finding| {
            format!(
                "{}:{}",
                finding["path"].as_str().unwrap_or("?"),
                finding["line"]
            )
        })
        .collect();
    assert_eq!(places, ["core/src/other.rs:2"], "{report} {stderr}");
    assert_eq!(report["suppressed"], 1);
    assert!(!stderr.contains("names no check"), "{stderr}");
}
