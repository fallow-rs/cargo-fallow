//! Black-box tests of `--trace` on `visibility`, `deps` and `dupes`.
//!
//! Each test writes a small workspace to a temporary directory, runs the
//! command with `--trace` and `--format json`, and checks the trace document.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// The library `base`, the library `mid` that uses `base`, and the binary
/// `app` that uses `mid`.
const FILES: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\", \"mid\", \"base\"]\nresolver = \"3\"\n",
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
        "[package]\nname = \"base\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    ),
    (
        "app/src/main.rs",
        "fn main() {\n    mid::run(&base_thing());\n}\n\nfn base_thing() -> mid::Thing {\n    mid::make()\n}\n",
    ),
    (
        "mid/src/lib.rs",
        "pub use base::Thing;\n\
         pub fn run(thing: &Thing) {\n    \
             base::direct();\n    \
             thing.method();\n    \
             let _ = stringify!(base::by_macro);\n\
         }\n\
         pub fn make() -> Thing {\n    \
             Thing\n\
         }\n\
         #[cfg(test)]\n\
         mod tests {\n    \
             use base::direct;\n\
         }\n",
    ),
    (
        "base/src/lib.rs",
        "pub fn direct() {}\n\
         pub struct Thing;\n\
         impl Thing {\n    \
             pub fn method(&self) {}\n\
         }\n\
         pub fn by_macro() {}\n\
         pub fn unused() {}\n",
    ),
];

fn workspace(extra: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    for (path, text) in FILES.iter().chain(extra) {
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

/// Runs the command and returns the exit code and the JSON document.
fn run(dir: &TempDir, args: &[&str]) -> (Option<i32>, Value) {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("find the binary")
        .args(args)
        .args(["--format", "json", "--quiet", "--manifest-path"])
        .arg(dir.path().join("Cargo.toml"))
        .output()
        .expect("run the binary");
    let json = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "the output is JSON: {err}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code(), json)
}

/// The uses of the one traced item as `file:line credit` strings.
fn uses(trace: &Value) -> Vec<String> {
    let items = trace["items"].as_array().expect("the trace has items");
    assert_eq!(items.len(), 1, "{trace:#}");
    items[0]["uses"]
        .as_array()
        .expect("the item has uses")
        .iter()
        .map(|entry| {
            format!(
                "{}:{} {}",
                entry["file"].as_str().unwrap_or_default(),
                entry["line"],
                entry["credit"].as_str().unwrap_or_default()
            )
        })
        .collect()
}

#[test]
fn a_visibility_trace_labels_a_path_a_method_name_and_macro_tokens() {
    let dir = workspace(&[]);
    let (code, trace) = run(&dir, &["visibility", "--trace", "base::direct"]);
    assert_eq!(code, Some(0), "{trace:#}");
    assert_eq!(trace["kind"], "visibility-trace");
    assert_eq!(trace["trace"], "base::direct");
    assert_eq!(
        uses(&trace),
        ["mid/src/lib.rs:3 path", "mid/src/lib.rs:12 import"]
    );
    let item = &trace["items"][0];
    assert_eq!(item["path"], "direct");
    assert_eq!(item["crate"], "base");
    assert_eq!(item["file"], "base/src/lib.rs");
    assert_eq!(item["line"], 1);
    assert_eq!(item["is_used"], true);
    assert_eq!(item["uses"][1]["test_only"], true);

    let (_, trace) = run(&dir, &["visibility", "--trace", "base::Thing::method"]);
    assert_eq!(uses(&trace), ["mid/src/lib.rs:4 method-name"]);
    assert_eq!(trace["items"][0]["uses"][0]["class"], "untyped");
    assert_eq!(
        trace["items"][0]["undecided"].as_array().map(Vec::len),
        Some(1),
        "only an untyped credit keeps the method: {trace:#}"
    );

    let (_, trace) = run(&dir, &["visibility", "--trace", "base::by_macro"]);
    assert_eq!(uses(&trace), ["mid/src/lib.rs:5 macro-tokens"]);
}

#[test]
fn a_visibility_trace_gives_the_verdict_of_an_unused_item() {
    let dir = workspace(&[]);
    let (code, trace) = run(&dir, &["visibility", "--trace", "base::unused"]);
    assert_eq!(code, Some(0), "{trace:#}");
    let item = &trace["items"][0];
    assert_eq!(item["is_used"], false);
    assert_eq!(item["findings"][0], "dead_public");
    assert_eq!(uses(&trace), Vec::<String>::new());
}

#[test]
fn a_visibility_trace_names_a_glob_reexport_hold() {
    let dir = workspace(&[(
        "base/src/lib.rs",
        "mod inner {\n    pub fn deep() {}\n}\npub use inner::*;\n",
    )]);
    let (_, trace) = run(&dir, &["visibility", "--trace", "base::inner::deep"]);
    let holds = &trace["items"][0]["holds"];
    assert!(
        holds
            .as_array()
            .is_some_and(|holds| holds.iter().any(|hold| hold == "glob-reexport")),
        "{trace:#}"
    );
}

#[test]
fn a_trace_of_an_unknown_item_is_an_input_error() {
    let dir = workspace(&[]);
    let (code, error) = run(&dir, &["visibility", "--trace", "base::missing"]);
    assert_eq!(code, Some(2), "{error:#}");
    assert_eq!(error["error"], true, "{error:#}");
}

#[test]
fn a_deps_trace_lists_each_use_site_with_its_context() {
    let dir = workspace(&[]);
    let (code, trace) = run(&dir, &["deps", "--offline", "--trace", "base"]);
    assert_eq!(code, Some(0), "{trace:#}");
    assert_eq!(trace["kind"], "deps-trace");
    assert_eq!(trace["package_name"], "base");
    assert_eq!(trace["is_used"], true);
    let declarations = trace["declarations"].as_array().expect("declarations");
    assert_eq!(declarations.len(), 1, "{trace:#}");
    assert_eq!(declarations[0]["package"], "mid");
    assert_eq!(declarations[0]["section"], "dependencies");
    assert_eq!(declarations[0]["line"], 8);
    let sites: Vec<String> = trace["uses"]
        .as_array()
        .expect("uses")
        .iter()
        .map(|site| {
            format!(
                "{}:{} {} {}",
                site["file"].as_str().unwrap_or_default(),
                site["line"],
                site["context"].as_str().unwrap_or_default(),
                site["via"].as_str().unwrap_or_default()
            )
        })
        .collect();
    assert_eq!(
        sites,
        [
            "mid/src/lib.rs:1 lib import",
            "mid/src/lib.rs:3 lib path",
            "mid/src/lib.rs:5 lib macro-tokens",
            "mid/src/lib.rs:12 unit-test import",
        ]
    );
    assert_eq!(trace["findings"], Value::Array(Vec::new()));
}

#[test]
fn a_deps_trace_of_an_unused_dependency_shows_the_finding() {
    let dir = workspace(&[(
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\nmid = { path = \"../mid\" }\nbase = { path = \"../base\" }\n",
    )]);
    let (_, trace) = run(&dir, &["deps", "--offline", "--trace", "base"]);
    let rules: Vec<&str> = trace["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter_map(|finding| finding["rule"].as_str())
        .collect();
    assert_eq!(rules, ["unused-dependency"], "{trace:#}");
    assert_eq!(trace["is_used"], true, "mid uses it");
}

#[test]
fn a_deps_trace_of_an_unknown_dependency_is_an_input_error() {
    let dir = workspace(&[]);
    let (code, error) = run(&dir, &["deps", "--offline", "--trace", "missing"]);
    assert_eq!(code, Some(2), "{error:#}");
    assert_eq!(error["error"], true, "{error:#}");
}

#[test]
fn a_dupes_trace_lists_the_instances_of_one_group() {
    let body = "pub fn a(values: &[u32]) -> u32 {\n    let mut total = 0;\n    for value in values {\n        if *value > 10 {\n            total += value * 2;\n        } else {\n            total += value + 1;\n        }\n    }\n    total\n}\n";
    let second = body.replace("pub fn a", "pub fn b");
    let dir = workspace(&[("base/src/one.rs", body), ("base/src/two.rs", &second)]);
    write(
        dir.path(),
        "base/src/lib.rs",
        "pub mod one;\npub mod two;\n",
    );
    let (_, report) = run(&dir, &["dupes", "--min-tokens", "20", "--min-lines", "3"]);
    let fingerprint = report["findings"][0]["fingerprint"]
        .as_str()
        .unwrap_or_else(|| panic!("a clone group: {report:#}"))
        .to_owned();
    let (code, trace) = run(
        &dir,
        &[
            "dupes",
            "--min-tokens",
            "20",
            "--min-lines",
            "3",
            "--trace",
            &fingerprint,
        ],
    );
    assert_eq!(code, Some(0), "{trace:#}");
    assert_eq!(trace["kind"], "dupes-trace");
    assert_eq!(trace["fingerprint"], fingerprint.as_str());
    let files: Vec<&str> = trace["clone_groups"][0]["instances"]
        .as_array()
        .expect("instances")
        .iter()
        .filter_map(|instance| instance["file"].as_str())
        .collect();
    assert_eq!(files, ["base/src/one.rs", "base/src/two.rs"]);

    let (code, _) = run(&dir, &["dupes", "--trace", "dup:00000000"]);
    assert_eq!(code, Some(2));
}

#[test]
fn a_trace_refuses_a_format_without_a_trace_form() {
    let dir = workspace(&[]);
    let output = Command::cargo_bin("cargo-fallow")
        .expect("find the binary")
        .args(["visibility", "--trace", "base::direct", "--format", "sarif"])
        .arg("--manifest-path")
        .arg(dir.path().join("Cargo.toml"))
        .output()
        .expect("run the binary");
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn a_human_trace_names_each_use() {
    let dir = workspace(&[]);
    let output = Command::cargo_bin("cargo-fallow")
        .expect("find the binary")
        .args(["visibility", "--quiet", "--trace", "base::Thing::method"])
        .arg("--manifest-path")
        .arg(dir.path().join("Cargo.toml"))
        .output()
        .expect("run the binary");
    let text = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{text}");
    assert!(text.contains("mid/src/lib.rs:4"), "{text}");
    assert!(text.contains("method-name"), "{text}");
}

#[test]
fn a_reexport_trace_does_not_list_its_own_use_item() {
    let dir = workspace(&[]);
    let (_, trace) = run(&dir, &["visibility", "--trace", "mid::Thing"]);
    // The `pub use` item at mid/src/lib.rs:1 is the traced item itself.
    assert_eq!(
        uses(&trace),
        [
            "app/src/main.rs:5 path",
            "mid/src/lib.rs:2 path",
            "mid/src/lib.rs:7 path",
            "mid/src/lib.rs:8 path",
        ]
    );
}

#[test]
fn a_visibility_trace_gives_the_line_of_the_name_of_a_method_chain() {
    let dir = workspace(&[(
        "mid/src/lib.rs",
        "pub fn run(thing: &base::Thing) {\n    let _ = thing\n        .clone_ref()\n        .method();\n}\n",
    )]);
    let (_, trace) = run(&dir, &["visibility", "--trace", "base::Thing::method"]);
    assert_eq!(uses(&trace), ["mid/src/lib.rs:4 method-name"]);
}

#[test]
fn a_visibility_trace_gives_the_line_of_an_attribute_and_a_doc_code_block() {
    let dir = workspace(&[(
        "mid/src/lib.rs",
        "/// Runs.\n\
         ///\n\
         /// ```\n\
         /// base::direct();\n\
         /// ```\n\
         pub fn documented() {}\n\
         \n\
         /// Tagged.\n\
         #[tool::tag(base::by_macro)]\n\
         pub fn tagged() {}\n",
    )]);
    let (_, trace) = run(&dir, &["visibility", "--trace", "base::direct"]);
    assert_eq!(uses(&trace), ["mid/src/lib.rs:4 doc-code"]);
    let (_, trace) = run(&dir, &["visibility", "--trace", "base::by_macro"]);
    assert_eq!(uses(&trace), ["mid/src/lib.rs:9 attribute"]);
}

#[test]
fn a_visibility_trace_follows_the_kinds_of_the_configuration() {
    let dir = workspace(&[(
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\", \"mid\", \"base\"]\nresolver = \"3\"\n\n\
         [workspace.metadata.fallow.visibility]\nkinds = [\"unnecessary-public\"]\n",
    )]);
    let (_, trace) = run(&dir, &["visibility", "--trace", "base::unused"]);
    assert_eq!(
        trace["items"][0]["findings"],
        Value::Array(Vec::new()),
        "{trace:#}"
    );
}

#[test]
fn a_deps_trace_marks_which_declaration_the_code_uses() {
    let dir = workspace(&[(
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\nmid = { path = \"../mid\" }\nbase = { path = \"../base\" }\n",
    )]);
    let (_, trace) = run(&dir, &["deps", "--offline", "--trace", "base"]);
    let used: Vec<String> = trace["declarations"]
        .as_array()
        .expect("declarations")
        .iter()
        .map(|entry| {
            format!(
                "{} {}",
                entry["package"].as_str().unwrap_or_default(),
                entry["used"]
            )
        })
        .collect();
    assert_eq!(used, ["app false", "mid true"], "{trace:#}");
}
