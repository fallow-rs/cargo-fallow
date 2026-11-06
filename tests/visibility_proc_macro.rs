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
/// The `vec!` call and the `#depth` variable are code of the proc-macro, not
/// its output.
const MACRO: &str = "use proc_macro::TokenStream;\n\
    macro_rules! quote { ($($t:tt)*) => { stringify!($($t)*) }; }\n\
    struct Opts {\n    width: i32,\n}\n\
    #[proc_macro_attribute]\n\
    pub fn bench(_args: TokenStream, item: TokenStream) -> TokenStream {\n\
    \x20   let opts = Opts { width: 1 };\n\
    \x20   let all = vec![opts.width, 2];\n\
    \x20   let depth = all.len();\n\
    \x20   let mut out = item.to_string();\n\
    \x20   out.push_str(quote! { fn __depth() -> usize { #depth } });\n\
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
         pub struct Size {\n    pub width: i32,\n}\n\
         pub fn depth() -> i32 { 8 }\n\
         pub fn size() -> i32 { depth() + Size { width: 9 }.width }\n\
         impl Gadget {\n    pub fn build() -> i32 { 7 }\n}\n\
         impl Thing {\n    pub fn make() -> Self { Self }\n    pub fn go(&self) -> i32 { 3 }\n}\n",
    ),
    (
        "app/src/main.rs",
        "#[facade::bench]\nfn work() {}\nfn main() { work(); let _ = a::size(); }\n",
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
/// expression of the output uses the field `Config::level`. The field name
/// in `vec!` and the `#depth` variable are not in the output, so `Size`,
/// `Size::width` and `depth` can be private.
#[test]
fn proc_macro_tokens_use_items_of_the_crates_that_call_the_macro() {
    let dir = workspace();
    let found = findings(&dir);
    let expected: BTreeSet<String> = [
        "dead-public unused",
        "dead-public solo",
        "dead-public Poll",
        "unnecessary-public Size",
        "unnecessary-public Size::width",
        "unnecessary-public depth",
    ]
    .iter()
    .map(|row| (*row).to_owned())
    .collect();
    assert_eq!(found, expected);
}

/// A workspace for the scope of the macro output:
///
/// - `pm` is a proc-macro crate;
/// - `facade` re-exports the macro of `pm`, and `relay` re-exports it again
///   from `facade`;
/// - `user` calls the macro through `relay`;
/// - `far` depends on `user` and on `other`, but `far` cannot name the macro;
/// - `items` holds the items that the output names or defines;
/// - `plain` re-exports a struct of `facade`, but not the macro, so `tool`
///   cannot name the macro through `plain`. `tool` also uses `lone`.
const SCOPE_MANIFESTS: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"pm\", \"facade\", \"relay\", \"user\", \"far\", \"other\", \
         \"items\", \"plain\", \"tool\", \"lone\"]\nresolver = \"3\"\n",
    ),
    (
        "pm/Cargo.toml",
        "[package]\nname = \"pm\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [lib]\nproc-macro = true\n",
    ),
    (
        "facade/Cargo.toml",
        "[package]\nname = \"facade\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\npm = { path = \"../pm\" }\n",
    ),
    (
        "relay/Cargo.toml",
        "[package]\nname = \"relay\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\nfacade = { path = \"../facade\" }\n",
    ),
    (
        "user/Cargo.toml",
        "[package]\nname = \"user\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\nrelay = { path = \"../relay\" }\nitems = { path = \"../items\" }\n",
    ),
    (
        "far/Cargo.toml",
        "[package]\nname = \"far\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\nuser = { path = \"../user\" }\nother = { path = \"../other\" }\n",
    ),
    (
        "other/Cargo.toml",
        "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    ),
    (
        "items/Cargo.toml",
        "[package]\nname = \"items\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    ),
    (
        "plain/Cargo.toml",
        "[package]\nname = \"plain\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\nfacade = { path = \"../facade\" }\n",
    ),
    (
        "tool/Cargo.toml",
        "[package]\nname = \"tool\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\nplain = { path = \"../plain\" }\nlone = { path = \"../lone\" }\n",
    ),
    (
        "lone/Cargo.toml",
        "[package]\nname = \"lone\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    ),
];

/// The proc-macro. The output calls `items::chained` and a method
/// `member_name`, defines names, implements a trait, and names a function
/// after an interpolated prefix. The test module quotes `items::from_test`.
const SCOPE_MACRO: &str = "use proc_macro::TokenStream;\n\
    macro_rules! quote { ($($t:tt)*) => { stringify!($($t)*) }; }\n\
    #[proc_macro_attribute]\n\
    pub fn bench(_args: TokenStream, item: TokenStream) -> TokenStream {\n\
    \x20   let krate = quote!(items);\n\
    \x20   let mut out = item.to_string();\n\
    \x20   out.push_str(quote! {\n\
    \x20       fn __bench(v: u8) { items::chained(); v.member_name(); #krate::via_prefix(); }\n\
    \x20       fn defined_only() {}\n\
    \x20       fn take(cursor: u8) -> u8 { cursor + 1 }\n\
    \x20       struct Shape { level: u8 }\n\
    \x20       enum Kind { Alpha, Beta { depth: u8 } }\n\
    \x20       impl items::Marker for Shape {}\n\
    \x20       const LIMIT: u8 = 1;\n\
    \x20       use items::{tint, Marker};\n\
    \x20       fn __glow(v: u8) { v.glow(); v.lone_call(); }\n\
    \x20   });\n\
    \x20   let _ = krate;\n\
    \x20   out.parse().expect(\"valid tokens\")\n\
    }\n\
    #[cfg(test)]\n\
    mod tests {\n\
    \x20   #[test]\n\
    \x20   fn quoted() {\n\
    \x20       let _ = quote! { items::from_test() };\n\
    \x20   }\n\
    }\n";

const SCOPE_FILES: &[(&str, &str)] = &[
    ("pm/src/lib.rs", SCOPE_MACRO),
    (
        "facade/src/lib.rs",
        "pub use pm::bench;\npub struct Helper;\n",
    ),
    ("relay/src/lib.rs", "pub use facade::bench;\n"),
    ("plain/src/lib.rs", "pub use facade::Helper;\n"),
    (
        "tool/src/main.rs",
        "fn main() { let _ = plain::Helper; lone::run(); }\n",
    ),
    (
        "lone/src/lib.rs",
        "pub struct Lone;\n\
         impl Lone {\n    pub fn lone_call(&self) {}\n}\n\
         pub fn run() { Lone.lone_call(); }\n",
    ),
    (
        "user/src/lib.rs",
        "#[relay::bench]\nfn work() {}\npub fn run() { work(); let _ = items::own(); }\n",
    ),
    (
        "far/src/main.rs",
        "fn main() { user::run(); other::run(); }\n",
    ),
    (
        "other/src/lib.rs",
        "pub struct Unrelated;\n\
         impl Unrelated {\n    pub fn member_name(&self) {}\n}\n\
         pub fn run() { let _ = Unrelated; }\n",
    ),
    (
        "items/src/lib.rs",
        "pub trait Marker {}\n\
         pub fn chained() {}\n\
         pub fn via_prefix() {}\n\
         pub fn from_test() {}\n\
         pub fn defined_only() {}\n\
         pub fn Alpha() {}\n\
         pub const LIMIT: u8 = 2;\n\
         pub struct State {\n    pub cursor: u8,\n    pub level: u8,\n    pub depth: u8,\n\
         \x20   pub tint: u8,\n    pub glow: u8,\n}\n\
         pub fn own() -> u8 {\n\
         \x20   let s = State { cursor: 1, level: 2, depth: 3, tint: 4, glow: 5 };\n\
         \x20   s.cursor + s.level + s.depth + s.tint + s.glow\n}\n",
    ),
];

/// The output of `#[relay::bench]` in `user` calls `items::chained` and
/// implements `items::Marker`, so both stay `pub`. `relay` re-exports the
/// macro of `facade`, and `facade` re-exports the macro of `pm`. So `user`
/// can call the macro. `far` cannot name the macro, so `v.member_name()` does
/// not use `other::Unrelated::member_name`, and only `other` uses
/// `Unrelated`. The output only defines
/// `defined_only`, `Alpha`, `LIMIT`, the parameter `cursor` and the fields
/// `level` and `depth`, so these names are no uses. `#krate::via_prefix`
/// uses `via_prefix`. The `quote!` of the test module is not output, so
/// `from_test` has no use. The name `tint` in `use items::{tint, ..}` is no
/// field, and `v.glow()` calls a method, so the fields `tint` and `glow` have
/// no use outside `items`. `tool` cannot name the macro, so `v.lone_call()`
/// does not use `lone::Lone::lone_call`.
#[test]
fn proc_macro_output_counts_only_in_crates_that_can_name_the_macro() {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    for (path, text) in SCOPE_MANIFESTS.iter().chain(SCOPE_FILES) {
        write(dir.path(), path, text);
    }
    let found = findings(&dir);
    let expected: BTreeSet<String> = [
        "unnecessary-public Unrelated",
        "dead-public Unrelated::member_name",
        "dead-public defined_only",
        "dead-public from_test",
        "dead-public Alpha",
        "dead-public LIMIT",
        "unnecessary-public State",
        "unnecessary-public State::cursor",
        "unnecessary-public State::level",
        "unnecessary-public State::depth",
        "unnecessary-public State::tint",
        "unnecessary-public State::glow",
        "unnecessary-public Lone",
        "unnecessary-public Lone::lone_call",
    ]
    .iter()
    .map(|row| (*row).to_owned())
    .collect();
    assert_eq!(found, expected);
}

/// A facade that imports the macro in a private module and re-exports it with
/// a path in the crate: `pub use crate::inner::bench;`.
const INNER_FILES: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"pm\", \"facade\", \"items\", \"user\"]\nresolver = \"3\"\n",
    ),
    (
        "pm/Cargo.toml",
        "[package]\nname = \"pm\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [lib]\nproc-macro = true\n",
    ),
    (
        "facade/Cargo.toml",
        "[package]\nname = \"facade\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\npm = { path = \"../pm\" }\n",
    ),
    (
        "items/Cargo.toml",
        "[package]\nname = \"items\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    ),
    (
        "user/Cargo.toml",
        "[package]\nname = \"user\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\nfacade = { path = \"../facade\" }\nitems = { path = \"../items\" }\n",
    ),
    (
        "pm/src/lib.rs",
        "use proc_macro::TokenStream;\n\
         macro_rules! quote { ($($t:tt)*) => { stringify!($($t)*) }; }\n\
         #[proc_macro_attribute]\n\
         pub fn bench(_args: TokenStream, item: TokenStream) -> TokenStream {\n\
         \x20   let mut out = item.to_string();\n\
         \x20   out.push_str(quote! { fn __bench() { items::only_output(); } });\n\
         \x20   out.parse().expect(\"valid tokens\")\n\
         }\n",
    ),
    (
        "facade/src/lib.rs",
        "mod inner {\n    pub(crate) use pm::bench;\n}\npub use crate::inner::bench;\n",
    ),
    ("items/src/lib.rs", "pub fn only_output() {}\n"),
    (
        "user/src/main.rs",
        "#[facade::bench]\nfn work() {}\nfn main() { work(); }\n",
    ),
];

/// `facade` re-exports the macro of `pm` through a private import, so `user`
/// can call the macro, and the output uses `items::only_output`. The second
/// facade re-exports a private alias of the macro.
#[test]
fn a_reexport_through_a_path_in_the_crate_counts() {
    let facades = [
        "mod inner {\n    pub(crate) use pm::bench;\n}\npub use crate::inner::bench;\n",
        "use pm::bench as imported;\npub use imported as bench;\n",
    ];
    for facade in facades {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        for (path, text) in INNER_FILES {
            write(dir.path(), path, text);
        }
        write(dir.path(), "facade/src/lib.rs", facade);
        assert_eq!(findings(&dir), BTreeSet::new(), "facade: {facade}");
    }
}

/// A workspace with `pm`, `facade`, `glue`, `other` and `downstream`.
/// `downstream` depends on `glue` and `other`. It is a caller only when
/// `glue` re-exports the macro.
const REEXPORT_FILES: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"pm\", \"facade\", \"glue\", \"other\", \"downstream\"]\n\
         resolver = \"3\"\n",
    ),
    (
        "pm/Cargo.toml",
        "[package]\nname = \"pm\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [lib]\nproc-macro = true\n",
    ),
    (
        "facade/Cargo.toml",
        "[package]\nname = \"facade\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\npm = { path = \"../pm\" }\n",
    ),
    (
        "glue/Cargo.toml",
        "[package]\nname = \"glue\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\nfacade = { path = \"../facade\" }\n",
    ),
    (
        "other/Cargo.toml",
        "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    ),
    (
        "downstream/Cargo.toml",
        "[package]\nname = \"downstream\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
         publish = false\n\n[dependencies]\nglue = { path = \"../glue\" }\n\
         other = { path = \"../other\" }\n",
    ),
    (
        "pm/src/lib.rs",
        "use proc_macro::TokenStream;\n\
         macro_rules! quote { ($($t:tt)*) => { stringify!($($t)*) }; }\n\
         #[proc_macro_attribute]\n\
         pub fn bench(_args: TokenStream, item: TokenStream) -> TokenStream {\n\
         \x20   let mut out = item.to_string();\n\
         \x20   out.push_str(quote! { fn __bench(v: u8) { v.member_name(); } });\n\
         \x20   out.parse().expect(\"valid tokens\")\n\
         }\n",
    ),
    (
        "other/src/lib.rs",
        "pub struct Unrelated;\n\
         impl Unrelated {\n    pub fn member_name(&self) {}\n}\n\
         pub fn run() { let _ = Unrelated; }\n",
    ),
    ("downstream/src/main.rs", "fn main() { other::run(); }\n"),
];

/// A crate is a caller only when it depends on a crate whose re-export leads
/// to the macro. A glob or a named import of a module without the macro, or
/// of a struct next to the macro, is no re-export of the macro.
#[test]
fn only_a_reexport_that_leads_to_the_macro_makes_callers() {
    let root = "pub use pm::bench;\npub mod colors {\n    pub struct Red;\n}\n";
    let nested = "pub mod colors {\n    pub use pm::bench;\n    pub struct Red;\n}\n";
    let cases = [
        (root, "pub use facade::colors::*;\n", true),
        (nested, "pub use facade::colors::Red;\n", true),
        (nested, "pub use facade::colors::*;\n", false),
        (nested, "pub use facade::colors;\n", false),
        (nested, "pub use facade::colors::bench;\n", false),
        (root, "pub use facade::*;\n", false),
    ];
    for (facade, glue, reported) in cases {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        for (path, text) in REEXPORT_FILES {
            write(dir.path(), path, text);
        }
        write(dir.path(), "facade/src/lib.rs", facade);
        write(dir.path(), "glue/src/lib.rs", glue);
        let found = findings(&dir).contains("dead-public Unrelated::member_name");
        assert_eq!(found, reported, "facade: {facade} glue: {glue}");
    }
}

/// The macro output calls an associated function after an interpolated type:
/// `#ty::interp_method()`. It also defines a struct with a where clause and
/// items with an interpolated name.
const FORMS_FILES: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"pm\", \"items\", \"user\"]\nresolver = \"3\"\n",
    ),
    (
        "pm/Cargo.toml",
        "[package]\nname = \"pm\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [lib]\nproc-macro = true\n",
    ),
    (
        "items/Cargo.toml",
        "[package]\nname = \"items\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    ),
    (
        "user/Cargo.toml",
        "[package]\nname = \"user\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\npm = { path = \"../pm\" }\nitems = { path = \"../items\" }\n",
    ),
    (
        "pm/src/lib.rs",
        "use proc_macro::TokenStream;\n\
         macro_rules! quote { ($($t:tt)*) => { stringify!($($t)*) }; }\n\
         #[proc_macro_attribute]\n\
         pub fn bench(_args: TokenStream, item: TokenStream) -> TokenStream {\n\
         \x20   let ty = quote!(items::Thing);\n\
         \x20   let name = quote!(generated);\n\
         \x20   let mut out = item.to_string();\n\
         \x20   out.push_str(quote! {\n\
         \x20       fn __bench() { let _ = #ty::interp_method(); }\n\
         \x20       struct W<F> where F: Fn(u8) { where_field: F }\n\
         \x20       fn #name(param_name: u8) {}\n\
         \x20       struct #name { field_name: u8 }\n\
         \x20   });\n\
         \x20   let _ = (ty, name);\n\
         \x20   out.parse().expect(\"valid tokens\")\n\
         }\n",
    ),
    (
        "items/src/lib.rs",
        "pub struct Thing;\n\
         impl Thing {\n    pub fn interp_method() -> Self {\n        Thing\n    }\n}\n\
         pub struct State {\n    pub where_field: u8,\n    pub param_name: u8,\n\
         \x20   pub field_name: u8,\n}\n\
         pub fn own() -> u8 {\n\
         \x20   let s = State { where_field: 1, param_name: 2, field_name: 3 };\n\
         \x20   s.where_field + s.param_name + s.field_name\n}\n",
    ),
    (
        "user/src/main.rs",
        "#[pm::bench]\nfn work() {}\nfn main() { work(); let _ = items::own(); }\n",
    ),
];

/// `#ty::interp_method()` uses the associated function `interp_method`. The
/// output only defines `where_field`, `param_name` and `field_name`, so these
/// fields have no use outside `items`.
#[test]
fn interpolations_and_where_clauses_in_output() {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    for (path, text) in FORMS_FILES {
        write(dir.path(), path, text);
    }
    let expected: BTreeSet<String> = [
        "unnecessary-public State",
        "unnecessary-public State::where_field",
        "unnecessary-public State::param_name",
        "unnecessary-public State::field_name",
    ]
    .iter()
    .map(|row| (*row).to_owned())
    .collect();
    assert_eq!(findings(&dir), expected);
}
