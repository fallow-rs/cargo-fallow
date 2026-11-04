//! Black-box tests of `fix --only` and `fix --skip`.
//!
//! The workspace has one unused dependency and one item that only its own
//! crate uses, so each check of `fix` has one edit. An unused optional
//! dependency is a warning that `fix` does not change. A fake `cargo` runs the
//! real `cargo metadata`, passes each `cargo check` and logs it.
#![cfg(unix)]
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

const CORE: &str = "pub fn used() { helper(); }\npub fn helper() {}\n";
const APP: &str = "fn main() { core_lib::used(); }\n";
/// The file that the fake cargo writes a line to for each `cargo check`.
const CHECK_LOG: &str = "checks.log";

/// The number of `cargo check` calls of the run.
fn checks(dir: &TempDir) -> usize {
    fs::read_to_string(dir.path().join(CHECK_LOG)).map_or(0, |text| text.lines().count())
}

/// Writes the workspace: `app` uses `core-lib`, and `core-lib` has an unused
/// dependency on `extra` and an unused optional dependency on `spare`.
fn workspace() -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"core\", \"app\", \"extra\", \"spare\"]\nresolver = \"3\"\n",
    );
    write(
        root,
        "core/Cargo.toml",
        "[package]\nname = \"core-lib\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[lib]\nname = \"core_lib\"\n\n[dependencies]\nextra = { path = \"../extra\" }\nspare = { path = \"../spare\", optional = true }\n",
    );
    write(root, "core/src/lib.rs", CORE);
    write(
        root,
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[dependencies]\ncore-lib = { path = \"../core\" }\n",
    );
    write(root, "app/src/main.rs", APP);
    write(
        root,
        "extra/Cargo.toml",
        "[package]\nname = \"extra\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    );
    write(root, "extra/src/lib.rs", "");
    write(
        root,
        "spare/Cargo.toml",
        "[package]\nname = \"spare\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    );
    write(root, "spare/src/lib.rs", "");
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("a file path has a parent")).expect("create a folder");
    fs::write(full, text).expect("write a file");
}

/// A fake cargo: `check` passes and writes a line to [`CHECK_LOG`], each other
/// command runs the real cargo.
fn fake_cargo(dir: &TempDir) -> String {
    use std::os::unix::fs::PermissionsExt as _;

    let real = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let script = dir.path().join("fake-cargo");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = check ]; then echo check >> '{log}'; exit 0; fi\nexec '{real}' \"$@\"\n",
            log = dir.path().join(CHECK_LOG).display()
        ),
    )
    .expect("write the script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("make it executable");
    script.to_str().expect("the path is UTF-8").to_owned()
}

fn fix(dir: &TempDir, args: &[&str]) -> (i32, Value, String) {
    let cargo = fake_cargo(dir);
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .env("CARGO", cargo)
        .arg("fix")
        .args(args)
        .args(["--format", "json", "--quiet"])
        .output()
        .expect("run the binary");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let report = serde_json::from_str(&stdout).unwrap_or(Value::Null);
    (
        output.status.code().unwrap_or(-1),
        report,
        format!("{}{stdout}", String::from_utf8_lossy(&output.stderr)),
    )
}

/// The `type` of each planned edit.
fn types(report: &Value) -> Vec<String> {
    report["fixes"]
        .as_array()
        .expect("the report has a fix list")
        .iter()
        .map(|fix| fix["type"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn without_a_filter_both_checks_plan_an_edit() {
    let dir = workspace();
    let (code, report, output) = fix(&dir, &["--dry-run"]);
    assert_eq!(code, 0, "{output}");
    assert_eq!(
        types(&report),
        ["remove-dependency", "change-visibility"],
        "{report}"
    );
    assert_eq!(report["skipped"], 1, "{report}");
}

#[test]
fn only_visibility_plans_no_dependency_edit() {
    let dir = workspace();
    let (code, report, output) = fix(&dir, &["--dry-run", "--only", "visibility"]);
    assert_eq!(code, 0, "{output}");
    assert_eq!(types(&report), ["change-visibility"], "{report}");
    assert_eq!(report["skipped"], 0, "{report}");
}

#[test]
fn skip_visibility_plans_only_the_dependency_edit() {
    let dir = workspace();
    let (code, report, output) = fix(&dir, &["--dry-run", "--skip", "visibility"]);
    assert_eq!(code, 0, "{output}");
    assert_eq!(types(&report), ["remove-dependency"], "{report}");
    assert_eq!(report["skipped"], 1, "{report}");
}

#[test]
fn skip_deps_counts_no_dependency_warning() {
    let dir = workspace();
    let (code, report, output) = fix(&dir, &["--dry-run", "--skip", "deps"]);
    assert_eq!(code, 0, "{output}");
    assert_eq!(types(&report), ["change-visibility"], "{report}");
    assert_eq!(report["skipped"], 0, "{report}");
}

#[test]
fn skip_of_each_check_exits_2() {
    let dir = workspace();
    let (code, _, output) = fix(&dir, &["--dry-run", "--skip", "deps,visibility"]);
    assert_eq!(code, 2, "{output}");
    assert!(output.contains("CARGO_FALLOW_INVALID_ARGUMENT"), "{output}");
}

#[test]
fn yes_with_only_visibility_checks_the_build_and_keeps_the_manifest() {
    let dir = workspace();
    let (code, report, output) = fix(&dir, &["--yes", "--only", "visibility"]);
    assert_eq!(code, 0, "{output}");
    assert_eq!(report["total_fixed"], 1, "{report}");
    assert_eq!(checks(&dir), 2, "one check before and one after the edits");
    let manifest = fs::read_to_string(dir.path().join("core/Cargo.toml")).expect("read a file");
    assert!(manifest.contains("extra"), "{manifest}");
}

#[test]
fn only_takes_a_list_and_can_repeat() {
    let dir = workspace();
    let (code, report, output) = fix(&dir, &["--dry-run", "--only", "deps,visibility"]);
    assert_eq!(code, 0, "{output}");
    assert_eq!(types(&report).len(), 2, "{report}");
    let (code, report, output) = fix(
        &dir,
        &["--dry-run", "--only", "deps", "--only", "visibility"],
    );
    assert_eq!(code, 0, "{output}");
    assert_eq!(types(&report).len(), 2, "{report}");
}

#[test]
fn yes_with_only_deps_keeps_the_visibility_keyword() {
    let dir = workspace();
    let (code, report, output) = fix(&dir, &["--yes", "--only", "deps"]);
    assert_eq!(code, 0, "{output}");
    assert_eq!(report["total_fixed"], 1, "{report}");
    let core = fs::read_to_string(dir.path().join("core/src/lib.rs")).expect("read a file");
    assert_eq!(core, CORE);
    let manifest = fs::read_to_string(dir.path().join("core/Cargo.toml")).expect("read a file");
    assert!(!manifest.contains("extra"), "{manifest}");
    assert_eq!(checks(&dir), 0, "a dependency edit needs no `cargo check`");
}

#[test]
fn only_and_skip_together_exit_2() {
    let dir = workspace();
    let (code, _, output) = fix(
        &dir,
        &["--dry-run", "--only", "deps", "--skip", "visibility"],
    );
    assert_eq!(code, 2, "{output}");
}

#[test]
fn a_check_without_edits_is_not_a_value() {
    let dir = workspace();
    let (code, _, output) = fix(&dir, &["--dry-run", "--only", "dupes"]);
    assert_eq!(code, 2, "{output}");
    assert!(output.contains("CARGO_FALLOW_INVALID_ARGUMENT"), "{output}");
    assert!(output.contains("dupes"), "{output}");
}
