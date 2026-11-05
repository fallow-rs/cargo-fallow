//! Black-box tests of `consumers`: a separate workspace, for example
//! `fuzz/`, whose code counts as a use of the items of the workspace.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// `fuzzed` has a use in its own crate and a use in the fuzz target.
/// `helper` has only a use in its own crate.
const CORE: &str =
    "pub fn used() {\n    fuzzed();\n    helper();\n}\npub fn fuzzed() {}\npub fn helper() {}\n";

/// A workspace with the library `core-lib` and the binary `app`, and a
/// separate workspace at `consumer` with a binary that calls
/// `core_lib::fuzzed`. `table` is the text of the visibility table.
fn workspace(consumer: &str, table: &str) -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        &format!(
            "[workspace]\nmembers = [\"core\", \"app\"]\nresolver = \"3\"\n\n[workspace.metadata.fallow.visibility]\n{table}"
        ),
    );
    write(
        root,
        "core/Cargo.toml",
        "[package]\nname = \"core-lib\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[lib]\nname = \"core_lib\"\n",
    );
    write(root, "core/src/lib.rs", CORE);
    write(
        root,
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[dependencies]\ncore-lib = { path = \"../core\" }\n",
    );
    write(
        root,
        "app/src/main.rs",
        "fn main() {\n    core_lib::used();\n}\n",
    );
    let up = "../".repeat(consumer.split('/').count());
    write(
        root,
        &format!("{consumer}/Cargo.toml"),
        &format!(
            "[package]\nname = \"core-fuzz\"\nversion = \"0.0.0\"\nedition = \"2024\"\npublish = false\n\n[workspace]\n\n[dependencies]\ncore-lib = {{ path = \"{up}core\" }}\n\n[[bin]]\nname = \"target_one\"\npath = \"fuzz_targets/target_one.rs\"\n"
        ),
    );
    write(
        root,
        &format!("{consumer}/fuzz_targets/target_one.rs"),
        "pub fn unused_in_fuzz() {}\nfn main() {\n    core_lib::fuzzed();\n}\n",
    );
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("a file path has a parent")).expect("create a folder");
    fs::write(full, text).expect("write a file");
}

fn visibility(dir: &TempDir) -> (i32, Value) {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args(["visibility", "--format", "json", "--quiet"])
        .output()
        .expect("run the binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value = serde_json::from_str(&stdout)
        .unwrap_or_else(|err| panic!("stdout is JSON ({err}): {stdout}"));
    (output.status.code().unwrap_or(-1), value)
}

/// The `kind item` of each finding.
fn items(report: &Value) -> Vec<String> {
    report["findings"]
        .as_array()
        .expect("findings is an array")
        .iter()
        .map(|finding| {
            format!(
                "{} {}",
                finding["kind"].as_str().unwrap_or_default(),
                finding["item"].as_str().unwrap_or_default()
            )
        })
        .collect()
}

#[test]
fn a_fuzz_workspace_counts_as_a_use_without_a_setting() {
    let dir = workspace("fuzz", "");
    let (code, report) = visibility(&dir);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(items(&report), ["unnecessary-public helper"], "{report:#}");
}

#[test]
fn an_empty_consumers_list_turns_off_the_fuzz_workspace() {
    let dir = workspace("fuzz", "consumers = []\n");
    let (_, report) = visibility(&dir);
    assert_eq!(
        items(&report),
        ["unnecessary-public fuzzed", "unnecessary-public helper"],
        "{report:#}"
    );
}

#[test]
fn consumers_names_a_workspace_at_another_place() {
    let dir = workspace(
        "tools/fuzzing",
        "consumers = [\"tools/fuzzing/Cargo.toml\"]\n",
    );
    let (code, report) = visibility(&dir);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(items(&report), ["unnecessary-public helper"], "{report:#}");
    // Without the key, a workspace outside `fuzz/` does not count.
    let dir = workspace("tools/fuzzing", "");
    let (_, report) = visibility(&dir);
    assert_eq!(
        items(&report),
        ["unnecessary-public fuzzed", "unnecessary-public helper"],
        "{report:#}"
    );
}

#[test]
fn a_consumer_manifest_that_does_not_exist_is_an_error() {
    let dir = workspace("fuzz", "consumers = [\"missing/Cargo.toml\"]\n");
    let (code, report) = visibility(&dir);
    assert_eq!(code, 2, "{report:#}");
    assert_eq!(report["code"], "CARGO_FALLOW_INVALID_CONFIG", "{report:#}");
}

/// The `fix` guard also checks each consumer workspace, before and after the
/// edits.
#[cfg(unix)]
#[test]
fn fix_checks_the_consumer_workspace_too() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = workspace("fuzz", "");
    let real = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let log = dir.path().join("checks.log");
    let script = dir.path().join("fake-cargo");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = check ] || [ \"$2\" = check ]; then\n  echo \"$@\" >> '{log}'\n  exit 0\nfi\nexec '{real}' \"$@\"\n",
            log = log.display()
        ),
    )
    .expect("write the script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("make it executable");
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .env("CARGO", &script)
        .args(["fix", "--yes", "--format", "json", "--quiet"])
        .output()
        .expect("run the binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    let report: Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    assert_eq!(report["total_fixed"], 1, "{report:#}");
    let calls = fs::read_to_string(&log).expect("read the log");
    let consumer = calls
        .lines()
        .filter(|line| line.contains("fuzz/Cargo.toml"))
        .count();
    assert_eq!(consumer, 2, "a check before and after the edits: {calls}");
}

/// Adds `only_fuzzed`, which only the fuzz target uses, with `target` as the
/// text of the fuzz target.
fn with_only_fuzzed(dir: &TempDir, target: &str) {
    write(
        dir.path(),
        "core/src/lib.rs",
        &format!("{CORE}pub fn only_fuzzed() {{}}\n"),
    );
    write(dir.path(), "fuzz/fuzz_targets/target_one.rs", target);
}

/// A consumer binary is a production root, also when `production` names
/// other binaries, so a use in it is no test use.
#[test]
fn a_consumer_binary_is_a_production_root() {
    let dir = workspace("fuzz", "production = [\"app:app\"]\n");
    with_only_fuzzed(
        &dir,
        "fn main() {\n    core_lib::fuzzed();\n    core_lib::only_fuzzed();\n}\n",
    );
    let (code, report) = visibility(&dir);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(items(&report), ["unnecessary-public helper"], "{report:#}");
}

/// A cargo-fuzz target calls the code inside `fuzz_target!`.
#[test]
fn a_use_inside_fuzz_target_counts() {
    let dir = workspace("fuzz", "");
    with_only_fuzzed(
        &dir,
        "#![no_main]\nlibfuzzer_sys::fuzz_target!(|data: &[u8]| {\n    let _ = data;\n    core_lib::fuzzed();\n    core_lib::only_fuzzed();\n});\n",
    );
    let (code, report) = visibility(&dir);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(items(&report), ["unnecessary-public helper"], "{report:#}");
}

/// A fake cargo for `fix`. A `check` of the consumer runs `consumer`, and a
/// `check` of the workspace runs `own`. Each other command runs the real
/// cargo.
#[cfg(unix)]
fn fake_cargo(dir: &TempDir, consumer: &str, own: &str) -> String {
    use std::os::unix::fs::PermissionsExt as _;

    let real = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let script = dir.path().join("fake-cargo");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = check ] || [ \"$2\" = check ]; then\n  case \"$*\" in\n    *fuzz/Cargo.toml*) {consumer} ;;\n    *) {own} ;;\n  esac\nfi\nexec '{real}' \"$@\"\n"
        ),
    )
    .expect("write the script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("make it executable");
    script.to_str().expect("the path is UTF-8").to_owned()
}

#[cfg(unix)]
fn fix(dir: &TempDir, cargo: &str) -> (Option<i32>, String, String) {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .env("CARGO", cargo)
        .args(["fix", "--yes", "--format", "json", "--quiet"])
        .output()
        .expect("run the binary");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// A consumer that does not build before the edits gets a warning, and
/// `fix` still applies the edits of the workspace.
#[cfg(unix)]
#[test]
fn a_consumer_that_does_not_build_gets_a_warning() {
    let dir = workspace("fuzz", "");
    let cargo = fake_cargo(
        &dir,
        "echo 'error: no C++ compiler' >&2; exit 101",
        "exit 0",
    );
    let (code, stdout, stderr) = fix(&dir, &cargo);
    assert_eq!(code, Some(0), "{stdout} {stderr}");
    assert!(
        stderr.contains("does not build before the edits"),
        "{stderr}"
    );
}

/// A failed run writes back the `Cargo.lock` of the consumer too: a lock
/// file that the check made goes again.
#[cfg(unix)]
#[test]
fn a_failed_run_removes_the_lock_file_that_the_consumer_check_made() {
    let dir = workspace("fuzz", "");
    let lock = dir.path().join("fuzz/Cargo.lock");
    let lib = dir.path().join("core/src/lib.rs");
    let cargo = fake_cargo(
        &dir,
        &format!("touch '{}'; exit 0", lock.display()),
        &format!(
            "if grep -q 'pub(crate)' '{}'; then echo 'error: could not compile' >&2; exit 101; fi; exit 0",
            lib.display()
        ),
    );
    let (code, stdout, stderr) = fix(&dir, &cargo);
    assert_eq!(code, Some(2), "{stdout} {stderr}");
    assert!(!lock.exists(), "the consumer lock file stays");
}
