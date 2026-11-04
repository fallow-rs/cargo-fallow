//! Black-box tests of the `visibility` command: the forms of documentation
//! and macro output that hold the visibility of an item.
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

/// A `Self::` path in a doc link names the self type of the `impl` block, or
/// the type that the comment documents.
#[test]
fn a_self_path_in_a_doc_link_names_the_self_type() {
    let dir = workspace(&[
        (
            "app/src/main.rs",
            "fn main() { let t = mid::T::make(); t.doc(); }\n",
        ),
        (
            "mid/src/lib.rs",
            "/// Build it with [`Self::make`] or [`Self::other`].\npub struct T;\n\
             impl T {\n    /// See [`Self::helper`] and [`Self::K`].\n    pub fn doc(&self) {}\n\
             pub fn helper(&self) {}\n    pub const K: u8 = 1;\n    pub fn make() -> Self { Self }\n\
             pub fn other() {}\n    pub fn unlinked() {}\n}\n\
             fn local(t: &T) { t.helper(); let _ = T::K; T::other(); T::unlinked(); }\n",
        ),
    ]);
    let (found, _) = findings(&dir, &[]);
    assert_eq!(found, set(&["dead-public T::unlinked"]));
}

/// rustdoc resolves a turbofish link, and an indented paragraph in a list
/// item is text, not code.
#[test]
fn a_turbofish_link_and_an_indented_paragraph_hold_their_targets() {
    let dir = workspace(&[
        ("app/src/main.rs", "fn main() { mid::run(); }\n"),
        (
            "mid/src/lib.rs",
            "/// Calls [`Gen::<u8>::new`].\n///\n/// - A list item.\n///\n\
             ///     The item goes on with [`Nested`].\npub fn run() { let _ = (Gen::<u8>::new(), Nested); }\n\
             pub struct Gen<T>(Option<T>);\nimpl<T> Gen<T> { pub fn new() -> Self { Self(None) } }\n\
             pub struct Nested;\n",
        ),
    ]);
    let (found, _) = findings(&dir, &[]);
    assert_eq!(found, set(&[]));
}

/// `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))` reads a
/// file relative to the package.
#[test]
fn a_readme_from_the_manifest_folder_holds_its_link_targets() {
    let dir = workspace(&[
        ("app/src/main.rs", "fn main() { mid::run(); }\n"),
        (
            "mid/src/lib.rs",
            "#![doc = include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/README.md\"))]\n\
             pub fn run() { let _ = Readme; }\npub struct Readme;\n",
        ),
        ("mid/README.md", "# mid\n\nSee [`crate::Readme`].\n"),
    ]);
    let (found, _) = findings(&dir, &[]);
    assert_eq!(found, set(&[]));
}

/// A `pub` item that a `macro_rules!` body writes keeps `pub`, so each type
/// that the body names keeps `pub` too.
#[test]
fn a_type_in_a_macro_that_writes_a_public_item_gets_no_finding() {
    let dir = workspace(&[
        (
            "app/src/main.rs",
            "fn main() { mid::run(); mid::via_macro(); }\n",
        ),
        (
            "mid/src/lib.rs",
            "macro_rules! mk { ($n:ident) => { pub fn $n() -> Made { Made } }; }\n\
             mk!(via_macro);\npub struct Made;\npub struct Plain;\n\
             macro_rules! private { () => { fn hidden() -> Plain { Plain } }; }\n\
             private!();\npub fn run() {}\n",
        ),
    ]);
    let (found, _) = findings(&dir, &[]);
    assert_eq!(found, set(&["unnecessary-public Plain"]));
}
