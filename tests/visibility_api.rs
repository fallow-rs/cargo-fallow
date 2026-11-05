//! Black-box tests of the `visibility` command: the libraries whose
//! exported items are an external API.
//!
//! Each test writes a small workspace to a temporary directory and checks the
//! set of findings. A finding is `kind item`, for example
//! `unnecessary_public util::helper`.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// The manifests of a workspace with three members: the binary `app`, the
/// library `mid` that `app` uses, and the library `base` that `mid` uses.
const MANIFESTS: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\", \"mid\", \"base\", \"testing\"]\nresolver = \"3\"\n",
    ),
    (
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\nmid = { path = \"../mid\" }\n",
    ),
    (
        "mid/Cargo.toml",
        "[package]\nname = \"mid\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\nbase = { path = \"../base\" }\n",
    ),
    (
        "base/Cargo.toml",
        "[package]\nname = \"base\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dev-dependencies]\ntesting = { path = \"../testing\" }\n",
    ),
    (
        "testing/Cargo.toml",
        "[package]\nname = \"testing\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    ),
];

/// Writes the workspace with `files` added to the manifests.
fn workspace(files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    for (path, text) in MANIFESTS.iter().chain(files) {
        write(dir.path(), path, text);
    }
    for default in [
        "app/src/main.rs",
        "mid/src/lib.rs",
        "base/src/lib.rs",
        "testing/src/lib.rs",
    ] {
        if !dir.path().join(default).exists() {
            write(dir.path(), default, "");
        }
    }
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create a folder");
    }
    fs::write(path, text).expect("write a file");
}

/// The findings of the run, as `kind item` strings, and the undecided list.
fn findings(dir: &TempDir, extra: &[&str]) -> (BTreeSet<String>, BTreeSet<String>) {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("find the binary")
        .args([
            "visibility",
            "--format",
            "json",
            "--quiet",
            "--manifest-path",
        ])
        .arg(dir.path().join("Cargo.toml"))
        .args(extra)
        .output()
        .expect("run the binary");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("the output is JSON");
    let rows = |key: &str| -> BTreeSet<String> {
        report[key]
            .as_array()
            .expect("an array of findings")
            .iter()
            .map(|row| {
                format!(
                    "{} {}",
                    row["kind"].as_str().unwrap_or_default(),
                    row["item"].as_str().unwrap_or_default()
                )
            })
            .collect()
    };
    (rows("findings"), rows("undecided"))
}

fn set(rows: &[&str]) -> BTreeSet<String> {
    rows.iter().map(|row| (*row).to_owned()).collect()
}

/// `mid` and `base` can go to a registry: neither has `publish = false`.
const PUBLISHED: &[(&str, &str)] = &[
    (
        "mid/Cargo.toml",
        "[package]\nname = \"mid\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
         [dependencies]\nbase = { path = \"../base\" }\n",
    ),
    (
        "base/Cargo.toml",
        "[package]\nname = \"base\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    ),
    ("app/src/main.rs", "fn main() { mid::run(); }\n"),
    (
        "mid/src/lib.rs",
        "pub fn run() { base::go(); }\npub fn unused_mid() {}\n",
    ),
    (
        "base/src/lib.rs",
        "pub fn go() {}\npub fn unused_base() {}\n",
    ),
];

fn published(root: &str) -> TempDir {
    let mut files: Vec<(&str, &str)> = PUBLISHED.to_vec();
    files.push(("Cargo.toml", root));
    workspace(&files)
}

const ROOT: &str =
    "[workspace]\nmembers = [\"app\", \"mid\", \"base\", \"testing\"]\nresolver = \"3\"\n";

#[test]
fn a_published_library_is_an_external_api_by_default() {
    let dir = published(ROOT);
    let (found, _) = findings(&dir, &[]);
    assert_eq!(found, set(&[]));
}

/// With `public-crates`, only the named libraries are an external API. Each
/// other library gets each rule, also when a registry can publish it.
#[test]
fn public_crates_mark_each_other_library_as_internal() {
    let root =
        format!("{ROOT}\n[workspace.metadata.fallow.visibility]\npublic-crates = [\"base\"]\n");
    let dir = published(&root);
    let (found, _) = findings(&dir, &[]);
    assert_eq!(found, set(&["dead-public unused_mid"]));
}

#[test]
fn the_public_crate_flag_and_a_glob_work_as_the_key() {
    let dir = published(ROOT);
    let (found, _) = findings(&dir, &["--public-crate", "bas*"]);
    assert_eq!(found, set(&["dead-public unused_mid"]));
}

/// An empty list makes each library internal.
#[test]
fn an_empty_public_crates_list_makes_each_library_internal() {
    let root = format!("{ROOT}\n[workspace.metadata.fallow.visibility]\npublic-crates = []\n");
    let dir = published(&root);
    let (found, _) = findings(&dir, &[]);
    assert_eq!(
        found,
        set(&["dead-public unused_base", "dead-public unused_mid"])
    );
}

/// The flag replaces the list of the key.
#[test]
fn the_public_crate_flag_replaces_the_key() {
    let root =
        format!("{ROOT}\n[workspace.metadata.fallow.visibility]\npublic-crates = [\"mid\"]\n");
    let dir = published(&root);
    let (found, _) = findings(&dir, &["--public-crate", "base"]);
    assert_eq!(found, set(&["dead-public unused_mid"]));
}

/// A pattern that matches no library is an error, so a typo does not make
/// each library internal without notice.
#[test]
fn a_public_crate_that_matches_no_library_is_an_error() {
    let dir = published(ROOT);
    let output = Command::cargo_bin("cargo-fallow")
        .expect("find the binary")
        .args([
            "visibility",
            "--format",
            "json",
            "--quiet",
            "--public-crate",
            "bsae",
            "--manifest-path",
        ])
        .arg(dir.path().join("Cargo.toml"))
        .output()
        .expect("run the binary");
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stdout).expect("the output is JSON");
    assert_eq!(error["code"], "CARGO_FALLOW_INVALID_CONFIG", "{error}");
}
