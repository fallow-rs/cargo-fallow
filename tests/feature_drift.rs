//! Black-box tests of the `feature-unification-drift` rule of the `deps`
//! command. The rule runs `cargo tree`, so each workspace uses path
//! dependencies only and the runs stay offline.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("a file path has a parent")).expect("create a folder");
    fs::write(full, text).expect("write a file");
}

fn manifest(name: &str, rest: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n{rest}"
    )
}

/// A workspace where `app` uses `base` with no feature and `other` turns on
/// the feature `fast` of `base`. `cargo build -p app` then builds `base`
/// without `fast`, and `cargo build --workspace` builds it with `fast`.
fn workspace(root_extra: &str, app_extra: &str) -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        &format!(
            "[workspace]\nmembers = [\"app\", \"other\", \"base\"]\nresolver = \"3\"\n{root_extra}"
        ),
    );
    write(
        root,
        "app/Cargo.toml",
        &manifest(
            "app",
            &format!("[dependencies]\nbase = {{ path = \"../base\"{app_extra} }}\n"),
        ),
    );
    write(root, "app/src/main.rs", "fn main() { base::f(); }\n");
    write(
        root,
        "other/Cargo.toml",
        &manifest(
            "other",
            "[dependencies]\nbase = { path = \"../base\", features = [\"fast\"] }\n",
        ),
    );
    write(root, "other/src/lib.rs", "pub fn g() { base::f(); }\n");
    write(
        root,
        "base/Cargo.toml",
        &manifest("base", "[features]\nfast = []\nslow = []\n"),
    );
    write(
        root,
        "base/src/lib.rs",
        "#[cfg(feature = \"fast\")]\npub fn quick() {}\n#[cfg(feature = \"slow\")]\npub fn late() {}\npub fn f() {}\n",
    );
    dir
}

fn run(dir: &TempDir) -> (Option<i32>, Value) {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args(["deps", "--format", "json", "--quiet", "--offline"])
        .output()
        .expect("run the binary");
    let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "the output is JSON: {err}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code(), report)
}

fn drift(report: &Value) -> Vec<&Value> {
    report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter(|finding| finding["rule"] == "feature-unification-drift")
        .collect()
}

const UNIFY: &str = "\n[workspace.metadata.fallow.deps]\nunify-features = [\"app\"]\n";

#[test]
fn a_feature_that_only_the_workspace_build_turns_on_is_drift() {
    let dir = workspace(UNIFY, "");
    let (code, report) = run(&dir);
    assert_eq!(code, Some(0), "{report:#}");
    let found = drift(&report);
    assert_eq!(found.len(), 1, "{report:#}");
    assert_eq!(found[0]["package"], "app");
    assert_eq!(found[0]["dependency"], "base");
    assert_eq!(found[0]["version"], "0.1.0");
    assert_eq!(
        found[0]["workspace_only_features"],
        serde_json::json!(["fast"])
    );
    assert_eq!(found[0]["manifest_path"], "app/Cargo.toml");
    // `app` declares `base` on line 8, so a comment can name this crate.
    assert_eq!(found[0]["line"], 8, "{report:#}");
    assert_eq!(found[0]["severity"], "error");
    assert_eq!(found[0]["actions"][0]["type"], "pin-features", "{report:#}");
}

/// A feature pin in the manifest of the member makes both builds equal.
#[test]
fn a_feature_pin_removes_the_drift() {
    let dir = workspace(UNIFY, ", features = [\"fast\"]");
    let (_, report) = run(&dir);
    assert!(drift(&report).is_empty(), "{report:#}");
}

/// The rule is opt-in: without `unify-features` it does not run.
#[test]
fn the_rule_runs_only_for_the_named_members() {
    let dir = workspace("", "");
    let (_, report) = run(&dir);
    assert!(drift(&report).is_empty(), "{report:#}");
}

/// A name that is no workspace member is an error.
#[test]
fn an_unknown_member_is_an_error() {
    let dir = workspace(
        "\n[workspace.metadata.fallow.deps]\nunify-features = [\"apq\"]\n",
        "",
    );
    let (code, report) = run(&dir);
    assert_eq!(code, Some(2), "{report:#}");
    assert_eq!(report["code"], "CARGO_FALLOW_INVALID_CONFIG", "{report:#}");
}

/// The member itself is no dependency that it can pin, also when another
/// member turns on one of its features.
#[test]
fn the_member_itself_gets_no_finding() {
    let dir = workspace(UNIFY, ", features = [\"fast\"]");
    let manifest = fs::read_to_string(dir.path().join("other/Cargo.toml")).expect("read");
    write(
        dir.path(),
        "other/Cargo.toml",
        &manifest.replace(
            "[dependencies]\n",
            "[dependencies]\napp = { path = \"../app\", features = [\"extra\"] }\n",
        ),
    );
    let app = fs::read_to_string(dir.path().join("app/Cargo.toml")).expect("read");
    write(
        dir.path(),
        "app/Cargo.toml",
        &format!("{app}\n[features]\nextra = []\n"),
    );
    write(dir.path(), "app/src/lib.rs", "pub fn h() {}\n");
    let (_, report) = run(&dir);
    assert!(drift(&report).is_empty(), "{report:#}");
}
