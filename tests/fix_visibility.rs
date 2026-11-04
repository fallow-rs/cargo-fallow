//! Black-box tests of the `fix` command for the `visibility` findings.
//!
//! A fake `cargo` runs the real `cargo metadata` and gives a fixed result for
//! `cargo check`, so most tests compile nothing. One test runs the real check.
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
/// uses `core`.
fn workspace(core: &str, app: &str) -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    write(
        dir.path(),
        "Cargo.toml",
        "[workspace]\nmembers = [\"core\", \"app\"]\nresolver = \"3\"\n",
    );
    write(
        dir.path(),
        "core/Cargo.toml",
        "[package]\nname = \"core-lib\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[lib]\nname = \"core_lib\"\n",
    );
    write(dir.path(), "core/src/lib.rs", core);
    write(
        dir.path(),
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[dependencies]\ncore-lib = { path = \"../core\" }\n",
    );
    write(dir.path(), "app/src/main.rs", app);
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("a file path has a parent")).expect("create a folder");
    fs::write(full, text).expect("write a file");
}

fn read(dir: &TempDir, path: &str) -> String {
    fs::read_to_string(dir.path().join(path)).expect("read a file")
}

/// A fake cargo: `check` exits with `check_code`, each other command runs the
/// real cargo. Returns the path of the script.
#[cfg(unix)]
fn fake_cargo(dir: &TempDir, check_code: i32) -> String {
    use std::os::unix::fs::PermissionsExt as _;

    let real = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let script = dir.path().join("fake-cargo");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = check ]; then\n  if [ {check_code} -ne 0 ]; then echo 'error[E0603]: function `f` is private' >&2; fi\n  exit {check_code}\nfi\nexec '{real}' \"$@\"\n"
        ),
    )
    .expect("write the script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("make it executable");
    script.to_str().expect("the path is UTF-8").to_owned()
}

fn fix(dir: &TempDir, args: &[&str], cargo: Option<&str>) -> (i32, String, String) {
    let mut command = Command::cargo_bin("cargo-fallow").expect("the binary exists");
    command.current_dir(dir.path()).arg("fix").args(args);
    if let Some(cargo) = cargo {
        command.env("CARGO", cargo);
    }
    let output = command.output().expect("run the binary");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|err| panic!("stdout is JSON ({err}): {text}"))
}

/// A library with one item that only its own crate uses, behind a raw string
/// with wide text on the same line.
const CORE: &str =
    "pub fn used() { helper(); }\nconst S: &str = r#\"é \"#; pub fn helper() { let _ = S; }\n";
const APP: &str = "fn main() { core_lib::used(); }\n";

#[cfg(unix)]
#[test]
fn dry_run_lists_the_keyword_edit_and_changes_nothing() {
    let dir = workspace(CORE, APP);
    let cargo = fake_cargo(&dir, 0);
    let (code, stdout, stderr) = fix(
        &dir,
        &["--dry-run", "--format", "json", "--quiet"],
        Some(&cargo),
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(read(&dir, "core/src/lib.rs"), CORE);
    let report = json(&stdout);
    let fix = &report["fixes"][0];
    assert_eq!(fix["type"], "change-visibility", "{report}");
    assert_eq!(fix["item"], "helper");
    assert_eq!(fix["from"], "pub");
    assert_eq!(fix["to"], "pub(crate)");
    assert_eq!(fix["applied"], false);
}

#[cfg(unix)]
#[test]
fn yes_narrows_the_keyword_at_its_character_column() {
    let dir = workspace(CORE, APP);
    let cargo = fake_cargo(&dir, 0);
    let (code, stdout, stderr) = fix(
        &dir,
        &["--yes", "--format", "json", "--quiet"],
        Some(&cargo),
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(json(&stdout)["total_fixed"], 1, "{stdout}");
    assert_eq!(
        read(&dir, "core/src/lib.rs"),
        "pub fn used() { helper(); }\nconst S: &str = r#\"é \"#; pub(crate) fn helper() { let _ = S; }\n"
    );
}

#[cfg(unix)]
#[test]
fn failed_check_after_the_edit_writes_back_each_file() {
    let dir = workspace(CORE, APP);
    // The check before the edits passes and the check after them fails: the
    // script counts its `check` calls in a file.
    use std::os::unix::fs::PermissionsExt as _;
    let real = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let counter = dir.path().join("checks");
    let script = dir.path().join("fake-cargo");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = check ]; then\n  if [ -f '{c}' ]; then echo 'error[E0603]: function `helper` is private' >&2; exit 101; fi\n  touch '{c}'; exit 0\nfi\nexec '{real}' \"$@\"\n",
            c = counter.display()
        ),
    )
    .expect("write the script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("make it executable");
    let script = script.to_str().expect("the path is UTF-8").to_owned();
    let (code, stdout, _) = fix(
        &dir,
        &["--yes", "--format", "json", "--quiet"],
        Some(&script),
    );
    assert_eq!(code, 2, "{stdout}");
    let error = json(&stdout);
    assert_eq!(error["code"], "CARGO_FALLOW_FIX_FAILED", "{error}");
    let message = error["message"].as_str().unwrap_or_default();
    assert!(message.contains("E0603"), "{message}");
    assert_eq!(read(&dir, "core/src/lib.rs"), CORE);
}

#[cfg(unix)]
#[test]
fn workspace_that_does_not_build_before_the_edits_changes_nothing() {
    let dir = workspace(CORE, APP);
    let cargo = fake_cargo(&dir, 101);
    let (code, stdout, _) = fix(
        &dir,
        &["--yes", "--format", "json", "--quiet"],
        Some(&cargo),
    );
    assert_eq!(code, 2, "{stdout}");
    let error = json(&stdout);
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|message| message.contains("before the edits")),
        "{error}"
    );
    assert_eq!(read(&dir, "core/src/lib.rs"), CORE);
}

#[test]
fn visibility_report_marks_the_keyword_change_as_auto_fixable() {
    let dir = workspace(CORE, APP);
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args(["visibility", "--format", "json", "--quiet"])
        .output()
        .expect("run the binary");
    let report = json(&String::from_utf8_lossy(&output.stdout));
    let action = &report["findings"][0]["actions"][0];
    assert_eq!(action["type"], "change-visibility", "{report}");
    assert_eq!(action["auto_fixable"], true, "{report}");
}

#[test]
fn real_check_passes_after_a_re_export_and_its_target_change() {
    // `inner::f` has a `pub use` that only the crate uses. The analysis
    // reports the re-export and holds the target, so the edits compile.
    let core =
        "mod inner {\n    pub fn f() {}\n}\npub use inner::f;\npub fn used() {\n    f();\n}\n";
    let dir = workspace(core, APP);
    let (code, stdout, stderr) = fix(
        &dir,
        &["--yes", "--format", "json", "--quiet", "--offline"],
        None,
    );
    assert_eq!(code, 0, "{stderr} {stdout}");
    let report = json(&stdout);
    assert!(
        report["total_fixed"]
            .as_u64()
            .is_some_and(|count| count >= 1),
        "{report}"
    );
    let after = read(&dir, "core/src/lib.rs");
    assert!(after.contains("pub(crate) use inner::f;"), "{after}");
}

#[cfg(unix)]
#[test]
fn relative_manifest_path_reaches_the_check_from_the_current_folder() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("create a temporary directory");
    let inner = workspace(CORE, APP);
    let sub = dir.path().join("sub");
    fs::rename(inner.path(), &sub).expect("move the workspace");
    // A fake check that fails when its `--manifest-path` names no file.
    let real = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let script = dir.path().join("fake-cargo");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = check ]; then\n  while [ $# -gt 0 ]; do\n    if [ \"$1\" = --manifest-path ]; then shift; [ -f \"$1\" ] || {{ echo \"error: no manifest $1\" >&2; exit 101; }}; fi\n    shift\n  done\n  exit 0\nfi\nexec '{real}' \"$@\"\n"
        ),
    )
    .expect("write the script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("make it executable");
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .env("CARGO", &script)
        .args([
            "fix",
            "--yes",
            "--format",
            "json",
            "--quiet",
            "--manifest-path",
            "sub/Cargo.toml",
        ])
        .output()
        .expect("run the binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert_eq!(json(&stdout)["total_fixed"], 1, "{stdout}");
}

#[cfg(unix)]
#[test]
fn edit_that_the_compiler_names_is_left_out_and_the_rest_stays() {
    use std::os::unix::fs::PermissionsExt as _;

    let core = "pub fn used() { helper(); other(); }\npub fn helper() {}\npub fn other() {}\n";
    let dir = workspace(core, APP);
    // The check fails while `helper` is `pub(crate)`, and names it.
    let real = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let lib = dir.path().join("core/src/lib.rs");
    let script = dir.path().join("fake-cargo");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = check ]; then\n  if grep -q 'pub(crate) fn helper' '{lib}'; then echo 'core/src/lib.rs:2:1: error[E0446]: crate-private type `helper` in public interface' >&2; exit 101; fi\n  exit 0\nfi\nexec '{real}' \"$@\"\n",
            lib = lib.display()
        ),
    )
    .expect("write the script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("make it executable");
    let script = script.to_str().expect("the path is UTF-8").to_owned();
    let (code, stdout, stderr) = fix(
        &dir,
        &["--yes", "--format", "json", "--quiet"],
        Some(&script),
    );
    assert_eq!(code, 0, "{stderr} {stdout}");
    let report = json(&stdout);
    assert_eq!(report["total_fixed"], 1, "{report}");
    assert_eq!(
        read(&dir, "core/src/lib.rs"),
        "pub fn used() { helper(); other(); }\npub fn helper() {}\npub(crate) fn other() {}\n"
    );
}
