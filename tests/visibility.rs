//! Black-box tests of the experimental `visibility` command.
//!
//! Each test writes a small workspace to a temporary directory and checks the
//! set of findings. A finding is `kind item`, for example
//! `unnecessary_public util::helper`.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
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

#[test]
fn public_items_get_the_finding_that_their_uses_allow() {
    let dir = workspace(&[
        ("app/src/main.rs", "fn main() { mid::shared(); }\n"),
        (
            "mid/src/lib.rs",
            "pub fn shared() { local(); }\npub fn local() {}\npub fn unused() {}\n\
             pub mod inner {\n    pub(crate) fn only_here() {}\n    pub(crate) fn wide() {}\n\
             fn call() { only_here(); }\n}\nmod other { fn f() { crate::inner::wide(); } }\n",
        ),
    ]);
    let (found, _) = findings(&dir, &[]);
    assert_eq!(
        found,
        set(&[
            "dead_public inner",
            "dead_public unused",
            "unnecessary_public local",
            "unnecessary_restricted_visibility inner::only_here",
        ])
    );
}

#[test]
fn method_call_gives_credit_through_a_transitive_reexport() {
    let dir = workspace(&[
        (
            "app/src/main.rs",
            "fn main() { let value = mid::base::Thing::new(); value.used(); }\n",
        ),
        ("mid/src/lib.rs", "pub use base;\n"),
        (
            "base/src/lib.rs",
            "pub struct Thing;\nimpl Thing {\n    pub fn new() -> Self { Self }\n    pub fn used(&self) {}\n}\n",
        ),
    ]);
    let (found, undecided) = findings(&dir, &[]);
    assert_eq!(found, set(&[]));
    assert_eq!(undecided, set(&["dead_public Thing::used"]));
}

#[test]
fn only_a_local_named_reexport_is_a_candidate() {
    let dir = workspace(&[
        ("app/src/main.rs", "fn main() { mid::run(); }\n"),
        (
            "mid/src/lib.rs",
            "mod inner { pub fn run() { crate::helper(); } pub fn helper() {} }\n\
             pub use base::Outside;\npub use inner::{\n    run,\n    helper,\n};\npub use inner as moved;\n",
        ),
        ("base/src/lib.rs", "pub struct Outside;\n"),
    ]);
    let (found, _) = findings(&dir, &[]);
    assert_eq!(found, set(&["unnecessary_public helper"]));
}

#[test]
fn a_reexport_finding_is_on_the_line_of_its_name() {
    let dir = workspace(&[(
        "mid/src/lib.rs",
        "mod inner { pub fn first() {} pub fn second() {} }\npub use inner::{\n    first,\n    second,\n};\n",
    )]);
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
        .output()
        .expect("run the binary");
    let report: Value = serde_json::from_slice(&output.stdout).expect("the output is JSON");
    let lines: BTreeSet<(String, u64)> = report["findings"]
        .as_array()
        .expect("an array of findings")
        .iter()
        .filter(|row| row["item_kind"] == "reexport")
        .map(|row| {
            (
                row["item"].as_str().unwrap_or_default().to_owned(),
                row["line"].as_u64().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        lines,
        BTreeSet::from([("first".to_owned(), 3), ("second".to_owned(), 4)])
    );
}

#[test]
fn members_of_a_reexported_type_are_candidates() {
    let dir = workspace(&[
        (
            "app/src/main.rs",
            "fn main() { let made = mid::Made::new(); made.show(); }\n",
        ),
        (
            "mid/src/lib.rs",
            "mod hidden {\n    pub struct Made { pub size: u8 }\n    impl Made {\n        \
             pub fn new() -> Self { Self { size: 0 }.grow() }\n        pub fn grow(self) -> Self { self }\n        \
             pub fn show(&self) {}\n        pub fn never(&self) {}\n    }\n}\npub use hidden::Made;\n",
        ),
    ]);
    let (found, undecided) = findings(&dir, &[]);
    assert_eq!(
        found,
        set(&[
            "dead_public hidden::Made::never",
            "unnecessary_public hidden::Made::grow",
            "unnecessary_public hidden::Made::size",
        ])
    );
    assert_eq!(
        undecided,
        set(&[
            "dead_public hidden::Made::grow",
            "dead_public hidden::Made::show"
        ])
    );
}

#[test]
fn an_excluded_crate_gets_no_findings() {
    let dir = workspace(&[(
        "mid/src/lib.rs",
        "pub fn unused() {}\nmod inner { pub(crate) fn here() {} fn call() { here(); } }\n",
    )]);
    let (found, _) = findings(&dir, &[]);
    assert_eq!(
        found,
        set(&[
            "dead_public unused",
            "unnecessary_restricted_visibility inner::here",
        ])
    );
    let (found, _) = findings(&dir, &["--exclude-crate", "mid"]);
    assert_eq!(found, set(&[]));
}

#[test]
fn a_restricted_type_in_a_wider_signature_keeps_its_visibility() {
    let dir = workspace(&[(
        "mid/src/lib.rs",
        "mod outer {\n    mod cache {\n        pub(super) struct Span { pub(super) line: u32 }\n        \
         pub(super) struct Snapshot { pub(super) spans: Vec<Span> }\n        \
         pub(super) fn make() -> Snapshot { Snapshot { spans: vec![Span { line: 1 }] } }\n    }\n    \
         fn use_it() -> u32 { cache::make().spans.len() as u32 }\n}\n",
    )]);
    let (found, _) = findings(&dir, &[]);
    assert!(
        !found.contains("unnecessary_restricted_visibility outer::cache::Span"),
        "{found:?}"
    );
}

#[test]
fn a_doc_block_credit_does_not_make_a_finding_undecided() {
    let dir = workspace(&[(
        "mid/src/lib.rs",
        "/// ```\n/// let thing = mid::Thing;\n/// thing.show();\n/// ```\npub struct Thing;\n\
         impl Thing {\n    pub fn show(&self) {}\n}\n",
    )]);
    let (found, undecided) = findings(&dir, &[]);
    assert_eq!(found, set(&[]));
    assert_eq!(undecided, set(&[]));
}

#[test]
fn a_reexport_of_a_required_item_stays() {
    let dir = workspace(&[
        (
            "app/src/main.rs",
            "fn main() { let _ = mid::graph::Graph.closure(); }\n",
        ),
        (
            "mid/src/lib.rs",
            "pub mod graph {\n    mod load {\n        pub struct Closure;\n        \
             impl super::Graph {\n            pub fn closure(&self) -> Closure { Closure }\n        }\n    }\n    \
             pub struct Graph;\n    pub use load::Closure;\n}\n",
        ),
    ]);
    let (found, _) = findings(&dir, &[]);
    assert!(!found.contains("dead_public graph::Closure"), "{found:?}");
}

#[test]
fn a_bare_name_in_tokens_does_not_credit_a_field_or_a_method() {
    let dir = workspace(&[
        (
            "app/src/main.rs",
            "fn main() { let text = mid::label(); println!(\"{}\", text); }\n",
        ),
        (
            "mid/src/lib.rs",
            "/// ```\n/// let text = mid::label();\n/// ```\npub fn label() -> String { Link { text: String::new() }.text }\n\
             pub struct Link {\n    pub text: String,\n}\n",
        ),
    ]);
    let (found, _) = findings(&dir, &[]);
    assert_eq!(
        found,
        set(&["unnecessary_public Link", "unnecessary_public Link::text"])
    );
}

#[test]
fn a_struct_literal_in_macro_tokens_credits_only_its_own_fields() {
    let dir = workspace(&[
        (
            "app/src/main.rs",
            "fn main() { let all = vec![mid::Parser { text: None }]; mid::run(&all); }\n",
        ),
        (
            "mid/src/lib.rs",
            "pub struct Parser {\n    pub text: Option<String>,\n}\npub struct Link {\n    pub text: String,\n}\n\
             pub fn run(all: &[Parser]) -> usize { all.len() + make().text.len() }\n\
             fn make() -> Link { Link { text: String::new() } }\n",
        ),
    ]);
    let (found, _) = findings(&dir, &[]);
    assert_eq!(
        found,
        set(&["unnecessary_public Link", "unnecessary_public Link::text"])
    );
}

#[test]
fn a_path_from_outside_the_workspace_credits_no_workspace_item() {
    let dir = workspace(&[
        (
            "app/src/main.rs",
            "fn main() { let text = String::new(); mid::run(&text); }\n",
        ),
        (
            "mid/src/lib.rs",
            "pub struct Thing;\nimpl Thing {\n    pub fn new() -> Self { Thing }\n}\n\
             pub fn run(_: &str) { let _ = Thing::new(); }\n",
        ),
    ]);
    let (found, _) = findings(&dir, &[]);
    assert_eq!(
        found,
        set(&["unnecessary_public Thing", "unnecessary_public Thing::new"])
    );
}

#[test]
fn a_struct_literal_of_an_outside_type_credits_no_workspace_field() {
    let dir = workspace(&[
        (
            "app/src/main.rs",
            "use std::ops::Range;\nfn main() { let span = Range { start: 0, end: mid::frame().line }; drop(span); }\n",
        ),
        (
            "mid/src/lib.rs",
            "pub struct Frame {\n    pub line: u32,\n    pub start: u32,\n}\n\
             pub fn frame() -> Frame { let made = Frame { line: 1, start: 0 }; let _ = made.start; made }\n",
        ),
    ]);
    let (found, _) = findings(&dir, &[]);
    assert!(
        found.contains("unnecessary_public Frame::start"),
        "{found:?}"
    );
}

#[test]
fn a_local_variable_does_not_credit_a_field_with_its_name() {
    let dir = workspace(&[
        (
            "app/src/main.rs",
            "fn main() { let column = 3; mid::show(column); }\n",
        ),
        (
            "mid/src/lib.rs",
            "pub struct Frame {\n    pub column: u32,\n}\n\
             pub fn show(value: u32) -> u32 { Frame { column: value }.column }\n",
        ),
    ]);
    let (found, _) = findings(&dir, &[]);
    assert!(
        found.contains("unnecessary_public Frame::column"),
        "{found:?}"
    );
}

#[test]
fn unit_tests_of_a_library_can_use_its_dev_dependencies() {
    let dir = workspace(&[
        ("testing/src/lib.rs", "pub fn fixture() -> u8 { 1 }\n"),
        (
            "base/src/lib.rs",
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn uses() { assert_eq!(testing::fixture(), 1); }\n}\n",
        ),
    ]);
    let (found, _) = findings(&dir, &[]);
    assert!(!found.contains("dead_public fixture"), "{found:?}");
}

#[test]
fn an_item_with_an_attribute_macro_gets_credit() {
    let dir = workspace(&[(
        "mid/src/lib.rs",
        "#[some_macro::export]\npub fn exported() {}\npub fn plain() {}\n",
    )]);
    let (found, _) = findings(&dir, &[]);
    assert_eq!(found, set(&["dead_public plain"]));
}

/// Layers of glob re-exports form a graph with many paths to each module. A name
/// that no module defines must not walk each path, and a name at the end must
/// still resolve.
#[test]
fn a_layered_glob_chain_resolves_without_a_walk_of_each_path() {
    const LAYERS: usize = 14;
    const WIDTH: usize = 4;
    let mut lib = String::new();
    for layer in 0..LAYERS {
        for slot in 0..WIDTH {
            writeln!(lib, "pub mod l{layer}_{slot} {{").expect("write to a string");
            if layer + 1 < LAYERS {
                for next in 0..WIDTH {
                    writeln!(lib, "    pub use super::l{}_{next}::*;", layer + 1)
                        .expect("write to a string");
                }
            } else if slot == 0 {
                lib.push_str("    pub fn leaf() {}\n");
            }
            lib.push_str("}\n");
        }
    }
    lib.push_str("pub fn probe() {\n    l0_0::Nope::go();\n}\n");
    let dir = workspace(&[
        ("mid/src/lib.rs", lib.as_str()),
        (
            "app/src/main.rs",
            "fn main() {\n    mid::probe();\n    mid::l0_0::leaf();\n}\n",
        ),
    ]);
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
        .timeout(std::time::Duration::from_secs(20))
        .output()
        .expect("run the binary");
    assert_eq!(
        output.status.code(),
        Some(0),
        "the run did not finish in time"
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("the output is JSON");
    let text = report["findings"].to_string();
    assert!(!text.contains("dead_public\",\"item\":\"mid::l"), "{text}");
    assert!(!text.contains("leaf"), "{text}");
}

/// Two modules that glob-import each other form a cycle. A lookup of a name that
/// neither defines must end, and a name in the cycle must resolve.
#[test]
fn a_glob_cycle_ends_and_resolves() {
    let lib = "pub mod a {\n    pub use super::b::*;\n    pub fn in_a() {}\n}\n\
               pub mod b {\n    pub use super::a::*;\n}\n\
               pub fn probe() {\n    b::Nope::go();\n    b::in_a();\n}\n";
    let dir = workspace(&[
        ("mid/src/lib.rs", lib),
        ("app/src/main.rs", "fn main() {\n    mid::probe();\n}\n"),
    ]);
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
        .timeout(std::time::Duration::from_secs(20))
        .output()
        .expect("run the binary");
    assert_eq!(
        output.status.code(),
        Some(0),
        "the run did not finish in time"
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("the output is JSON");
    let text = report["findings"].to_string();
    assert!(
        !text.contains("dead_public\",\"item\":\"mid::a::in_a"),
        "{text}"
    );
}
