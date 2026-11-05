//! Black-box tests of the `visibility` command: name credits, which credit
//! each item with one name.
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

/// Code inside an item does not use the item from outside. A signature that
/// names its own function by a path that does not resolve does not expose it,
/// and its module is no use either.
#[test]
fn a_name_credit_from_inside_the_item_does_not_hold_it() {
    let dir = workspace(&[
        ("app/src/main.rs", "fn main() { mid::run(); }\n"),
        (
            "mid/src/lib.rs",
            "mod inner {\n    pub(crate) fn foo(_x: Missing::foo) {}\n    pub fn call() { foo(todo!()); }\n}\n\
             pub fn run() { inner::call(); }\n",
        ),
    ]);
    let (found, _) = findings(&dir, &[]);
    assert_eq!(
        found,
        set(&["unnecessary-restricted-visibility inner::foo"])
    );
}

/// A field access in another crate keeps the field public. A field that only
/// its own crate accesses can be `pub(crate)`.
#[test]
fn a_name_credit_from_another_crate_keeps_the_field_public() {
    let dir = workspace(&[
        (
            "app/src/main.rs",
            "fn main() { let s = mid::make(); let _ = s.wide; }\n",
        ),
        (
            "mid/src/lib.rs",
            "pub struct S { pub wide: u8, pub narrow: u8 }\npub fn make() -> S { S { wide: 0, narrow: 0 } }\n\
             fn local(s: &S) -> u8 { s.narrow + s.wide }\n",
        ),
    ]);
    let (found, _) = findings(&dir, &[]);
    assert_eq!(found, set(&["unnecessary-public S::narrow"]));
}

/// A doc test is another crate, so its name credit keeps the item public,
/// also when a credit from the own crate is in the same group.
#[test]
fn a_doc_test_name_credit_and_a_normal_one_share_a_group() {
    let dir = workspace(&[
        ("app/src/main.rs", "fn main() { mid::run(); }\n"),
        (
            "mid/src/lib.rs",
            "/// ```\n/// let v = mid::Thing::default();\n/// v.shown();\n/// ```\npub fn run() { Thing.shown(); }\n\
             #[derive(Default)]\npub struct Thing;\nimpl Thing { pub fn shown(&self) {} }\n",
        ),
    ]);
    let (found, _) = findings(&dir, &[]);
    assert_eq!(found, set(&[]));
}
