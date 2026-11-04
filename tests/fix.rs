//! Black-box tests of the `fix` command for the `deps` findings.
//!
//! Each test writes a small workspace to a temporary directory. Each
//! dependency is a path dependency, so `cargo metadata` needs no network.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// The crates of the fixture are an external API for the visibility check,
/// so these tests see only the `Cargo.toml` edits.
const VISIBILITY_API: &str = "[workspace.metadata.fallow.visibility]\nexclude-crates = [\"app\", \"alpha\", \"beta\", \"gamma\", \"delta\"]\n";

/// The dependency crates that the tests can use.
const LIBS: &[&str] = &["alpha", "beta", "gamma", "delta"];

/// Writes a workspace with the member `app` and the libraries in [`LIBS`].
/// `app_manifest` is the text after the `[package]` table of `app/Cargo.toml`.
fn workspace(root_extra: &str, app_manifest: &str, files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let mut members = vec!["\"app\"".to_owned()];
    members.extend(LIBS.iter().map(|lib| format!("\"libs/{lib}\"")));
    write(
        dir.path(),
        "Cargo.toml",
        &format!(
            "[workspace]\nmembers = [{}]\nresolver = \"3\"\n{root_extra}\n{VISIBILITY_API}",
            members.join(", ")
        ),
    );
    write(
        dir.path(),
        "app/Cargo.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n{app_manifest}"
        ),
    );
    for lib in LIBS {
        write(
            dir.path(),
            &format!("libs/{lib}/Cargo.toml"),
            &format!(
                "[package]\nname = \"{lib}\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[features]\nextra = []\n"
            ),
        );
        write(
            dir.path(),
            &format!("libs/{lib}/src/lib.rs"),
            "pub fn f() {}\n",
        );
    }
    for (path, text) in files {
        write(dir.path(), path, text);
    }
    if !dir.path().join("app/src/lib.rs").exists() {
        write(dir.path(), "app/src/lib.rs", "pub fn g() { alpha::f(); }\n");
    }
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

/// Runs `fix` with the arguments and returns the exit code, stdout and stderr.
fn fix(dir: &TempDir, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let mut command = Command::cargo_bin("cargo-fallow").expect("the binary exists");
    command.current_dir(dir.path()).arg("fix").args(args);
    for (key, value) in env {
        command.env(key, value);
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

const APP_DEPS: &str = "[dependencies]\nalpha = { path = \"../libs/alpha\" }\n# beta is old\nbeta = { path = \"../libs/beta\" }\n# keep this comment\ngamma = { path = \"../libs/gamma\", optional = true }\n\n[features]\nfull = [\"dep:gamma\", \"gamma/extra\"]\n";

#[test]
fn dry_run_changes_no_byte_and_lists_the_edits() {
    let dir = workspace("", APP_DEPS, &[]);
    let before = read(&dir, "app/Cargo.toml");
    let (code, stdout, stderr) = fix(&dir, &["--dry-run", "--format", "json", "--quiet"], &[]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(read(&dir, "app/Cargo.toml"), before);
    let report = json(&stdout);
    assert_eq!(report["kind"], "fix");
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["total_fixed"], 0);
    let fixes = report["fixes"].as_array().expect("fixes is an array");
    assert_eq!(fixes.len(), 1, "{report}");
    assert_eq!(fixes[0]["type"], "remove-dependency");
    assert_eq!(fixes[0]["rule"], "unused-dependency");
    assert_eq!(fixes[0]["dependency"], "beta");
    assert_eq!(fixes[0]["manifest_path"], "app/Cargo.toml");
    assert_eq!(fixes[0]["applied"], false);
}

#[test]
fn yes_removes_the_entry_and_keeps_the_next_comment() {
    let dir = workspace("", APP_DEPS, &[]);
    let (code, stdout, stderr) = fix(&dir, &["--yes", "--format", "json", "--quiet"], &[]);
    assert_eq!(code, 0, "{stderr}");
    let report = json(&stdout);
    assert_eq!(report["total_fixed"], 1, "{report}");
    assert_eq!(report["fixes"][0]["applied"], true);
    let after = read(&dir, "app/Cargo.toml");
    assert!(!after.contains("beta"), "{after}");
    assert!(after.contains("# keep this comment\ngamma"), "{after}");
    assert!(after.contains("alpha = { path"), "{after}");
    // The optional dependency is a warning, so it and its feature stay.
    assert!(after.contains("\"gamma/extra\""), "{after}");
}

#[test]
fn without_yes_in_a_non_interactive_process_the_command_exits_2() {
    let dir = workspace("", APP_DEPS, &[]);
    let before = read(&dir, "app/Cargo.toml");
    let (code, stdout, _) = fix(&dir, &["--format", "json", "--quiet"], &[]);
    assert_eq!(code, 2);
    assert_eq!(json(&stdout)["error"], true);
    assert_eq!(read(&dir, "app/Cargo.toml"), before);
}

#[test]
fn renamed_target_and_dotted_entries_are_removed() {
    let manifest = "[dependencies]\nalpha = { path = \"../libs/alpha\" }\nrenamed = { package = \"beta\", path = \"../libs/beta\" }\n\n[target.'cfg(unix)'.dependencies]\ngamma = { path = \"../libs/gamma\" }\n\n[dependencies.delta]\npath = \"../libs/delta\"\n";
    let dir = workspace("", manifest, &[]);
    let (code, _, stderr) = fix(&dir, &["--yes", "--quiet"], &[]);
    assert_eq!(code, 0, "{stderr}");
    let after = read(&dir, "app/Cargo.toml");
    for gone in ["renamed", "gamma", "delta"] {
        assert!(!after.contains(gone), "{gone} stays: {after}");
    }
    assert!(after.contains("alpha"), "{after}");
}

#[test]
fn unused_workspace_entry_and_feature_values_are_removed() {
    let root = "\n[workspace.dependencies]\nalpha = { path = \"libs/alpha\" }\n# unused\nbeta = { path = \"libs/beta\" }\n";
    let manifest = "[dependencies]\nalpha = { workspace = true }\ngamma = { path = \"../libs/gamma\" }\n\n[dev-dependencies]\ndelta = { path = \"../libs/delta\" }\n";
    let dir = workspace(root, manifest, &[]);
    let (code, stdout, stderr) = fix(&dir, &["--yes", "--format", "json", "--quiet"], &[]);
    assert_eq!(code, 0, "{stderr}");
    let report = json(&stdout);
    let rules: Vec<&str> = report["fixes"]
        .as_array()
        .expect("fixes is an array")
        .iter()
        .filter_map(|fix| fix["rule"].as_str())
        .collect();
    assert_eq!(
        rules,
        [
            "unused-workspace-dependency",
            "unused-dependency",
            "unused-dev-dependency"
        ]
    );
    let root_after = read(&dir, "Cargo.toml");
    assert!(!root_after.contains("beta = "), "{root_after}");
    assert!(root_after.contains("alpha = { path"), "{root_after}");
    let after = read(&dir, "app/Cargo.toml");
    assert!(
        !after.contains("gamma") && !after.contains("delta"),
        "{after}"
    );
}

#[test]
fn warnings_stay_and_their_feature_values_stay() {
    // `gamma` is a `feature-only-dependency` warning: a feature sets its
    // features. The fix does not change a warning.
    let manifest = "[dependencies]\nalpha = { path = \"../libs/alpha\" }\ngamma = { path = \"../libs/gamma\" }\n\n[features]\nfull = [\"gamma/extra\"]\n";
    let dir = workspace("", manifest, &[]);
    let before = read(&dir, "app/Cargo.toml");
    let (code, stdout, stderr) = fix(&dir, &["--yes", "--format", "json", "--quiet"], &[]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(json(&stdout)["total_fixed"], 0);
    assert_eq!(read(&dir, "app/Cargo.toml"), before);
}

#[test]
fn misplaced_dependency_moves_to_dev_dependencies() {
    let manifest = "[dependencies]\nalpha = { path = \"../libs/alpha\" }\nbeta = { path = \"../libs/beta\" }\n";
    let dir = workspace(
        "",
        manifest,
        &[("app/tests/it.rs", "#[test]\nfn t() { beta::f(); }\n")],
    );
    let (code, stdout, stderr) = fix(&dir, &["--yes", "--format", "json", "--quiet"], &[]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        json(&stdout)["fixes"][0]["type"],
        "move-to-dev-dependencies"
    );
    let after = read(&dir, "app/Cargo.toml");
    let dev = after
        .split_once("[dev-dependencies]")
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    assert!(dev.contains("beta = { path"), "{after}");
    let (_, normal) = after.split_once("[dependencies]").expect("the table stays");
    let normal = normal
        .split("[dev-dependencies]")
        .next()
        .unwrap_or_default();
    assert!(!normal.contains("beta"), "{after}");
}

#[test]
fn finding_of_a_fixable_rule_is_auto_fixable_in_the_deps_report() {
    let dir = workspace("", APP_DEPS, &[]);
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args(["deps", "--format", "json", "--quiet"])
        .output()
        .expect("run the binary");
    let report = json(&String::from_utf8_lossy(&output.stdout));
    for finding in report["findings"].as_array().expect("findings is an array") {
        let fixable = finding["rule"] == "unused-dependency";
        assert_eq!(finding["actions"][0]["auto_fixable"], fixable, "{finding}");
    }
}

#[cfg(unix)]
#[test]
fn failed_check_after_the_edit_writes_back_the_old_bytes() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = workspace("", APP_DEPS, &[]);
    let before = read(&dir, "app/Cargo.toml");
    // A cargo that works before the edit and fails after it: the check after
    // the edit sees a manifest without `beta`.
    let real = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let manifest = dir.path().join("app/Cargo.toml");
    let script = dir.path().join("fake-cargo");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nif ! grep -q beta '{manifest}'; then echo 'error: broken manifest' >&2; exit 101; fi\nexec '{real}' \"$@\"\n",
            manifest = manifest.display()
        ),
    )
    .expect("write the script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("make it executable");
    let script = script.to_str().expect("the path is UTF-8").to_owned();
    let (code, stdout, _) = fix(
        &dir,
        &["--yes", "--format", "json", "--quiet"],
        &[("CARGO", &script)],
    );
    assert_eq!(code, 2, "{stdout}");
    let error = json(&stdout);
    assert_eq!(error["code"], "CARGO_FALLOW_FIX_FAILED", "{error}");
    assert_eq!(read(&dir, "app/Cargo.toml"), before);
}

#[test]
fn entry_that_turns_on_features_stays_for_a_person() {
    // Cargo unifies the features of one package, so an unused entry can turn
    // on a feature that another dependency needs.
    let manifest = "[dependencies]\nalpha = { path = \"../libs/alpha\" }\nbeta = { path = \"../libs/beta\", features = [\"extra\"] }\n";
    let dir = workspace("", manifest, &[]);
    let before = read(&dir, "app/Cargo.toml");
    let (code, stdout, stderr) = fix(&dir, &["--yes", "--format", "json", "--quiet"], &[]);
    assert_eq!(code, 0, "{stderr}");
    let report = json(&stdout);
    assert_eq!(report["total_fixed"], 0, "{report}");
    assert_eq!(report["skipped"], 1, "{report}");
    assert_eq!(read(&dir, "app/Cargo.toml"), before);
}

#[test]
fn workspace_entry_that_a_kept_member_entry_inherits_stays() {
    let root = "\n[workspace.dependencies]\nbeta = { path = \"libs/beta\" }\n";
    let manifest = "[dependencies]\nalpha = { path = \"../libs/alpha\" }\nbeta = { workspace = true, features = [\"extra\"] }\n";
    let dir = workspace(root, manifest, &[]);
    let before = read(&dir, "Cargo.toml");
    let (code, stdout, stderr) = fix(&dir, &["--yes", "--format", "json", "--quiet"], &[]);
    assert_eq!(code, 0, "{stderr} {stdout}");
    let report = json(&stdout);
    assert_eq!(report["total_fixed"], 0, "{report}");
    assert_eq!(read(&dir, "Cargo.toml"), before);
}

#[test]
fn misplaced_entry_that_a_feature_names_stays() {
    let manifest = "[dependencies]\nalpha = { path = \"../libs/alpha\" }\nbeta = { path = \"../libs/beta\" }\n\n[features]\nfull = [\"beta/extra\"]\n";
    let dir = workspace(
        "",
        manifest,
        &[("app/tests/it.rs", "#[test]\nfn t() { beta::f(); }\n")],
    );
    let before = read(&dir, "app/Cargo.toml");
    let (code, stdout, stderr) = fix(&dir, &["--yes", "--format", "json", "--quiet"], &[]);
    assert_eq!(code, 0, "{stderr} {stdout}");
    assert_eq!(json(&stdout)["total_fixed"], 0, "{stdout}");
    assert_eq!(read(&dir, "app/Cargo.toml"), before);
}

#[test]
fn misplaced_entry_moves_when_the_dev_entry_is_for_another_platform() {
    let dev = "[target.'cfg(windows)'.dev-dependencies]\nbeta = { path = \"../libs/beta\" }\n";
    {
        let manifest = format!(
            "[dependencies]\nalpha = {{ path = \"../libs/alpha\" }}\nbeta = {{ path = \"../libs/beta\" }}\n\n{dev}"
        );
        let dir = workspace(
            "",
            &manifest,
            &[("app/tests/it.rs", "#[test]\nfn t() { beta::f(); }\n")],
        );
        let (code, stdout, stderr) = fix(&dir, &["--yes", "--format", "json", "--quiet"], &[]);
        assert_eq!(code, 0, "{stderr} {stdout}");
        let report = json(&stdout);
        let types: Vec<&str> = report["fixes"]
            .as_array()
            .expect("fixes is an array")
            .iter()
            .filter(|fix| fix["dependency"] == "beta")
            .filter_map(|fix| fix["type"].as_str())
            .collect();
        assert_eq!(types, ["move-to-dev-dependencies"], "{dev}: {report}");
    }
}
