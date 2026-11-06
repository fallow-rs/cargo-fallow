//! Black-box tests of the `visibility` command: names in the tokens of a
//! proc-macro crate.
//!
//! A proc-macro writes its tokens into the crate that calls the macro. A name
//! in those tokens is a use in that crate, also when the proc-macro crate
//! does not depend on the crate of the item.
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

/// The members of the workspace:
///
/// - `m` is a proc-macro crate with no workspace dependency;
/// - `facade` re-exports the macro of `m`;
/// - `a` holds the items that only the output of the macro names;
/// - `app` calls the macro through `facade` and depends on `a`;
/// - `lonely` is a library that only `tool` uses, and `tool` cannot call
///   the macro.
const MANIFESTS: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"m\", \"facade\", \"a\", \"app\", \"lonely\", \"tool\"]\n\
         resolver = \"3\"\n",
    ),
    (
        "m/Cargo.toml",
        "[package]\nname = \"m\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [lib]\nproc-macro = true\n",
    ),
    (
        "facade/Cargo.toml",
        "[package]\nname = \"facade\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\nm = { path = \"../m\" }\n",
    ),
    (
        "a/Cargo.toml",
        "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    ),
    (
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\nfacade = { path = \"../facade\" }\na = { path = \"../a\" }\n",
    ),
    (
        "lonely/Cargo.toml",
        "[package]\nname = \"lonely\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    ),
    (
        "tool/Cargo.toml",
        "[package]\nname = \"tool\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\nlonely = { path = \"../lonely\" }\n",
    ),
];

/// The proc-macro. A local `quote!` keeps the test free of registry crates.
/// The tokens name items of `a` and of `lonely`, and `m` depends on neither.
const MACRO: &str = "use proc_macro::TokenStream;\n\
    macro_rules! quote { ($($t:tt)*) => { stringify!($($t)*) }; }\n\
    #[proc_macro_attribute]\n\
    pub fn bench(_args: TokenStream, item: TokenStream) -> TokenStream {\n\
    \x20   let mut out = item.to_string();\n\
    \x20   out.push_str(quote! {\n\
    \x20       fn __bench() -> i32 { a::helper() + a::Thing::make().go() + lonely::solo() }\n\
    \x20       fn __poll() { let _ = core::task::Poll::Ready(()); }\n\
    \x20       fn __more() -> i32 { single() + Gadget::build() }\n\
    \x20       fn __config() -> a::Config { a::Config { level: 1 } }\n\
    \x20   });\n\
    \x20   out.parse().expect(\"valid tokens\")\n\
    }\n";

const FILES: &[(&str, &str)] = &[
    ("m/src/lib.rs", MACRO),
    ("facade/src/lib.rs", "pub use m::bench;\n"),
    (
        "a/src/lib.rs",
        "pub fn helper() -> i32 { 1 }\n\
         pub fn unused() -> i32 { 2 }\n\
         pub struct Thing;\n\
         pub struct Poll;\n\
         pub fn single() -> i32 { 6 }\n\
         pub struct Gadget;\n\
         pub struct Config {\n    pub level: i32,\n}\n\
         impl Gadget {\n    pub fn build() -> i32 { 7 }\n}\n\
         impl Thing {\n    pub fn make() -> Self { Self }\n    pub fn go(&self) -> i32 { 3 }\n}\n",
    ),
    (
        "app/src/main.rs",
        "#[facade::bench]\nfn work() {}\nfn main() { work(); }\n",
    ),
    (
        "lonely/src/lib.rs",
        "pub fn solo() -> i32 { 4 }\npub fn used() -> i32 { 5 }\n",
    ),
    ("tool/src/main.rs", "fn main() { lonely::used(); }\n"),
];

fn workspace() -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    for (path, text) in MANIFESTS.iter().chain(FILES) {
        write(dir.path(), path, text);
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

/// The findings of the run, as `kind item` strings.
fn findings(dir: &TempDir) -> BTreeSet<String> {
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
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("the output is JSON");
    report["findings"]
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
}

/// The output of `#[facade::bench]` in `app` calls `a::helper`, `Thing::make`
/// and `Thing::go`, so they stay `pub`. `a::unused` has no use. `lonely::solo`
/// is not visible from a crate that can call the macro, so the tokens do not
/// use it. `core::task::Poll` resolves outside the workspace in each caller,
/// so it does not use `a::Poll`. The output can import `single` and
/// `Gadget` in its own module, so the bare name and the path that does not
/// resolve from the root of `app` use the items of `a`. The struct
/// expression of the output uses the field `Config::level`.
#[test]
fn proc_macro_tokens_use_items_of_the_crates_that_call_the_macro() {
    let dir = workspace();
    let found = findings(&dir);
    let expected: BTreeSet<String> = ["dead-public unused", "dead-public solo", "dead-public Poll"]
        .iter()
        .map(|row| (*row).to_owned())
        .collect();
    assert_eq!(found, expected);
}
