//! Black-box tests of the schema stage of the `mutation` command.
//!
//! Each test writes a small crate and runs real cargo, real rustc and real
//! nextest. The `--no-run` flag stops the command after the removal loop and the
//! baseline, so no test in this file runs a mutant.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

const MANIFEST: &str = "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\npublish = false\n\n[workspace]\n";

fn write_crate(dir: &Path, files: &[(&str, &str)]) {
    fs::write(dir.join("Cargo.toml"), MANIFEST).expect("write the manifest");
    for (path, text) in files {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().expect("a file has a parent")).expect("create the parent");
        fs::write(path, text).expect("write a file");
    }
}

struct Stage {
    /// The `schema` part of the report, with the full report in `full`.
    report: Value,
    full: Value,
    _source: TempDir,
    work: TempDir,
}

fn stage(files: &[(&str, &str)]) -> Stage {
    let source = tempfile::tempdir().expect("create a temporary directory");
    let work = tempfile::tempdir().expect("create a temporary directory");
    write_crate(source.path(), files);
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .env("MBX_DISABLE", "1")
        .args(ARGS)
        .arg(source.path().join("Cargo.toml"))
        .args(["--package", "fixture", "--work-dir"])
        .arg(work.path().join("run"))
        .output()
        .expect("the binary runs");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let full: Value = serde_json::from_slice(&output.stdout).expect("the report is JSON");
    Stage {
        report: full["schema"].clone(),
        full,
        _source: source,
        work,
    }
}

/// The command up to the baseline, for the schema tier. The memory guard has no
/// probe on Windows, and `--no-run` runs no mutant.
const ARGS: [&str; 8] = [
    "mutation",
    "--no-run",
    "--tier",
    "schema",
    "--format",
    "json",
    if cfg!(windows) {
        "--no-memory-guard"
    } else {
        "--quiet"
    },
    "--manifest-path",
];

fn number(report: &Value, key: &str) -> i64 {
    report[key]
        .as_i64()
        .unwrap_or_else(|| panic!("{key}: {report}"))
}

const SIMPLE: &str = r"
pub fn check(a: u8, b: u8) -> bool {
    if a == b || a < 3 {
        return true;
    }
    false
}

const LIMIT: bool = 1 == 2;

pub fn limit() -> bool {
    assert!(LIMIT == false);
    LIMIT
}

#[cfg(test)]
mod tests {
    #[test]
    fn check_works() {
        assert!(super::check(1, 1));
        assert!(1 == 1);
    }
}
";

#[test]
fn simple_crate_compiles_in_one_round_and_keeps_each_mutant() {
    let stage = stage(&[("src/lib.rs", SIMPLE)]);
    let report = &stage.report;
    assert_eq!(report["status"], "compiled", "{report}");
    assert_eq!(number(report, "rounds"), 1);
    // `if` negated, `||`, `==`, and two for `<`.
    assert_eq!(number(report, "candidates"), 5);
    assert_eq!(number(report, "kept"), 5);
    assert_eq!(report["removed"].as_array().map(Vec::len), Some(0));
    assert_eq!(stage.full["baseline"]["tests_pass_with_schema"], true);
    assert_eq!(stage.full["baseline"]["schema"]["nextest"]["pass"], 1);
    assert_eq!(stage.full["baseline"]["clean"]["nextest"]["pass"], 1);
    let schema = stage.work.path().join("run/schema/src/lib.rs");
    let text = fs::read_to_string(schema).expect("the schema copy exists");
    assert!(text.contains("__fallow_schema::active()"));
    assert!(text.contains("const LIMIT: bool = 1 == 2;"));
}

#[test]
fn source_tree_stays_untouched() {
    let source = tempfile::tempdir().expect("create a temporary directory");
    let work = tempfile::tempdir().expect("create a temporary directory");
    write_crate(source.path(), &[("src/lib.rs", SIMPLE)]);
    Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .env("MBX_DISABLE", "1")
        .args(ARGS)
        .arg(source.path().join("Cargo.toml"))
        .args(["--package", "fixture", "--work-dir"])
        .arg(work.path().join("run"))
        .assert()
        .success();
    let names: Vec<String> = fs::read_dir(source.path())
        .expect("read the source")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name != "src" && name != "Cargo.toml")
        .collect();
    assert_eq!(names, Vec::<String>::new());
    let text = fs::read_to_string(source.path().join("src/lib.rs")).expect("read the source");
    assert_eq!(text, SIMPLE);
}

#[test]
fn no_std_crate_with_deny_warnings_compiles_in_one_round() {
    let lib = r"//! Docs.
#![no_std]
#![deny(warnings)]
#![deny(missing_docs)]

/// A check. The mutant `x >= 0` starts the lint for a useless comparison.
pub fn positive(x: u8) -> bool {
    x > 0
}

#[cfg(test)]
mod tests {
    #[test]
    fn positive_works() {
        assert!(super::positive(1));
    }
}
";
    let stage = stage(&[("src/lib.rs", lib)]);
    let report = &stage.report;
    assert_eq!(report["status"], "compiled", "{report}");
    assert_eq!(number(report, "rounds"), 1, "{report}");
    assert_eq!(number(report, "kept"), 2);
    assert_eq!(stage.full["baseline"]["tests_pass_with_schema"], true);
}

#[test]
fn modules_are_followed_in_each_layout_and_unreached_files_are_counted() {
    let files = [
        (
            "src/lib.rs",
            "mod a;\nmod b;\n#[path = \"other/c.rs\"]\nmod c;\n#[cfg(test)]\nmod t;\n",
        ),
        (
            "src/a.rs",
            "pub mod nested;\npub fn f(x: u8) -> bool { x == 1 }\n",
        ),
        ("src/a/nested.rs", "pub fn f(x: u8) -> bool { x == 2 }\n"),
        ("src/b/mod.rs", "pub mod inner;\n"),
        ("src/b/inner.rs", "pub fn f(x: u8) -> bool { x == 3 }\n"),
        ("src/other/c.rs", "pub fn f(x: u8) -> bool { x == 4 }\n"),
        ("src/t.rs", "pub fn f(x: u8) -> bool { x == 5 }\n"),
        ("src/unused.rs", "pub fn f(x: u8) -> bool { x == 6 }\n"),
    ];
    let stage = stage(&files);
    let report = &stage.report;
    assert_eq!(report["status"], "compiled", "{report}");
    assert_eq!(number(report, "candidates"), 4);
    assert_eq!(number(report, "kept"), 4);
    // `src/t.rs` is test-only, and `src/unused.rs` has no `mod` declaration.
    let gaps = &stage.full["gaps"];
    assert_eq!(number(gaps, "unreached_src_files"), 2);
    assert_eq!(number(gaps, "files"), 6);
}

#[test]
fn error_inside_a_switch_removes_only_that_site() {
    // The `&'static` borrow needs constant promotion, and a switch stops it.
    let lib = r"
pub fn promoted() -> &'static bool {
    &(1 == 2)
}

pub fn plain(x: u8) -> bool {
    x == 1
}
";
    let stage = stage(&[("src/lib.rs", lib)]);
    let report = &stage.report;
    assert_eq!(report["status"], "compiled", "{report}");
    assert_eq!(number(report, "candidates"), 2);
    assert_eq!(number(report, "kept"), 1);
    assert!(number(report, "rounds") >= 2);
    let removed = &report["removed"][0];
    assert_eq!(removed["line"], 3, "{report}");
    assert_eq!(removed["code"], "E0515");
    let kept: Vec<&Value> = stage.full["mutants"]
        .as_array()
        .expect("mutants")
        .iter()
        .filter(|mutant| mutant["status"] == "pending")
        .collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0]["line"], 7);
}

#[test]
fn error_outside_each_site_uses_the_bisection() {
    // The compiler cannot see through a switch that `x` is set in the body.
    let lib = r"
pub fn a(y: u8) -> bool { y == 1 }
pub fn b(y: u8) -> bool { y == 2 }
pub fn c(y: u8) -> bool { y == 3 }

pub fn uninit(y: bool) -> u8 {
    let x;
    if y && { x = 1; true } {
        x
    } else {
        0
    }
}

pub fn d(y: u8) -> bool { y == 4 }
pub fn e(y: u8) -> bool { y == 5 }
";
    let stage = stage(&[("src/lib.rs", lib)]);
    let report = &stage.report;
    assert_eq!(report["status"], "compiled", "{report}");
    // `==` five times, the `&&`, and the negated `if`.
    assert_eq!(number(report, "candidates"), 7);
    assert_eq!(number(report, "kept"), 5, "{report}");
    let removed = report["removed"].as_array().expect("removed");
    assert_eq!(removed.len(), 2);
    assert!(
        removed.iter().all(|item| item["how"] == "bisection"),
        "{report}"
    );
    assert!(removed.iter().all(|item| item["line"] == 8), "{report}");
    assert!(number(report, "rounds") > 2);
}

#[test]
fn block_like_operand_stays_an_operand() {
    // In a match arm, `unsafe { x } == 1` ends after the block unless the arm
    // has parentheses. The same holds for each other block-like expression.
    let lib = r"
pub fn a(x: u8) -> bool {
    let r = unsafe { core::ptr::read(&x) } == 1;
    r
}

pub fn b(x: u8) -> bool {
    let r = { x } < 1;
    r
}

pub fn c(x: u8) -> bool {
    match x {
        y if match y { 0 => 1, _ => 2 } != 1 => true,
        _ => false,
    }
}
";
    let stage = stage(&[("src/lib.rs", lib)]);
    let report = &stage.report;
    assert_eq!(report["status"], "compiled", "{report}");
    assert_eq!(number(report, "rounds"), 1, "{report}");
    assert_eq!(number(report, "kept"), number(report, "candidates"));
}

#[test]
fn package_with_a_dependency_of_the_same_name_is_selected() {
    let manifest = "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                    [dependencies]\nold = { path = \"old\", package = \"fixture\" }\n\n[workspace]\nexclude = [\"old\"]\n";
    let source = tempfile::tempdir().expect("create a temporary directory");
    let work = tempfile::tempdir().expect("create a temporary directory");
    let root = source.path();
    fs::create_dir_all(root.join("src")).expect("create");
    fs::create_dir_all(root.join("old/src")).expect("create");
    fs::write(root.join("Cargo.toml"), manifest).expect("write");
    fs::write(
        root.join("src/lib.rs"),
        "pub fn f(x: u8) -> bool { old::g(x) && x == 1 }\n",
    )
    .expect("write");
    fs::write(
        root.join("old/Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    )
    .expect("write");
    fs::write(
        root.join("old/src/lib.rs"),
        "pub fn g(x: u8) -> bool { x == 2 }\n",
    )
    .expect("write");
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .env("MBX_DISABLE", "1")
        .args(ARGS)
        .arg(root.join("Cargo.toml"))
        .args(["--package", "fixture", "--work-dir"])
        .arg(work.path().join("run"))
        .output()
        .expect("the binary runs");
    let full: Value = serde_json::from_slice(&output.stdout).expect("the report is JSON");
    assert_eq!(full["schema"]["status"], "compiled", "{full}");
    assert_eq!(full["baseline"]["tests_pass_with_schema"], true, "{full}");
}

#[test]
fn kept_mutants_are_probed_so_code_outside_the_build_does_not_count() {
    let lib = r"
pub fn built(x: u8) -> bool { x == 1 }

#[cfg(any())]
pub fn off(x: u8) -> bool { x == 2 }

#[cfg(unix)]
pub fn on(x: u8) -> bool { x == 3 }

#[cfg(not(unix))]
pub fn on(x: u8) -> bool { x == 3 }
";
    let stage = stage(&[("src/lib.rs", lib)]);
    let report = &stage.report;
    assert_eq!(report["status"], "compiled", "{report}");
    assert_eq!(number(report, "kept"), 4);
    assert_eq!(number(report, "kept_under_cfg"), 3);
    assert_eq!(number(report, "kept_compile_checked"), 2, "{report}");
    // The probe does not stay in the schema and does not count as a round.
    assert_eq!(number(report, "rounds"), 1);
    let schema = stage.work.path().join("run/schema/src/lib.rs");
    let text = fs::read_to_string(schema).expect("the schema copy exists");
    assert!(!text.contains("__fallow_probe"));
    assert_eq!(stage.full["baseline"]["tests_pass_with_schema"], true);
}

#[test]
fn crlf_source_compiles_and_keeps_its_line_endings() {
    let lib = "pub fn f(x: u8) -> bool {\r\n    let s = \"a\r\nb\";\r\n    s.len() == 3 && x == 1\r\n}\r\n";
    let stage = stage(&[("src/lib.rs", lib)]);
    let report = &stage.report;
    assert_eq!(report["status"], "compiled", "{report}");
    assert_eq!(number(report, "rounds"), 1);
    assert_eq!(number(report, "kept"), 3);
    let schema = stage.work.path().join("run/schema/src/lib.rs");
    let text = fs::read_to_string(schema).expect("the schema copy exists");
    assert!(text.contains("let s = \"a\r\nb\";\r\n"));
}

#[test]
fn file_that_does_not_parse_is_reported_and_skipped() {
    let files = [
        (
            "src/lib.rs",
            "#[cfg(any())]\nmod broken;\npub fn f(x: u8) -> bool { x == 1 }\n",
        ),
        ("src/broken.rs", "fn f( {\n"),
    ];
    let stage = stage(&files);
    let report = &stage.report;
    assert_eq!(report["status"], "compiled", "{report}");
    assert_eq!(number(report, "kept"), 1);
    assert_eq!(
        stage.full["gaps"]["files_skipped"][0]["path"],
        "src/broken.rs"
    );
}

#[test]
fn crate_without_sites_is_reported() {
    let stage = stage(&[("src/lib.rs", "pub fn f() -> u8 { 1 }\n")]);
    assert_eq!(stage.report["status"], "no_sites", "{}", stage.report);
    assert_eq!(number(&stage.report, "candidates"), 0);
}

#[test]
fn doctests_run_separately_and_keep_their_names() {
    let lib = "/// Doc.\n///\n/// ```\n/// assert!(fixture::f(1));\n/// ```\npub fn f(\n    x: u8,\n) -> bool {\n    x\n    .max(1)\n    ==\n    1\n}\n\n/// Second.\n///\n/// ```\n/// assert!(!fixture::g(1));\n/// ```\npub fn g(x: u8) -> bool { x == 2 }\n";
    let stage = stage(&[("src/lib.rs", lib)]);
    let baseline = &stage.full["baseline"];
    assert_eq!(baseline["clean"]["doc"]["pass"], 2, "{baseline}");
    assert_eq!(baseline["schema"]["doc"]["pass"], 2, "{baseline}");
    assert_eq!(baseline["tests_pass_with_schema"], true, "{baseline}");
}

#[test]
fn test_that_breaks_with_the_schema_is_a_regression() {
    // The arm holds a copy of the multi-line operand. That moves the code under
    // it, so `line!()` differs.
    let lib = "pub fn f(x: u8) -> bool {\n    x\n    .max(1)\n    == 1\n}\n\npub fn line() -> u32 {\n    line!()\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn line_is_stable() {\n        assert_eq!(super::line(), 8);\n    }\n\n    #[test]\n    fn other() {}\n}\n";
    let stage = stage(&[("src/lib.rs", lib)]);
    let baseline = &stage.full["baseline"];
    assert_eq!(baseline["tests_pass_with_schema"], false, "{baseline}");
    let regressions = baseline["regressions"].as_array().expect("regressions");
    assert_eq!(regressions.len(), 1, "{baseline}");
    assert!(
        regressions[0]
            .as_str()
            .is_some_and(|name| name.ends_with("line_is_stable"))
    );
}

#[test]
fn test_that_fails_without_the_schema_is_not_a_regression() {
    let lib =
        "pub fn f(x: u8) -> bool { x == 1 }\n\n#[test]\nfn broken() { panic!(\"always\"); }\n";
    let stage = stage(&[("src/lib.rs", lib)]);
    let baseline = &stage.full["baseline"];
    assert_eq!(baseline["tests_pass_with_schema"], true, "{baseline}");
    assert_eq!(baseline["clean"]["nextest"]["fail"], 1, "{baseline}");
    assert_eq!(baseline["schema"]["nextest"]["fail"], 1, "{baseline}");
}

#[test]
fn package_flag_selects_one_workspace_member_and_path_dependencies_build() {
    let source = tempfile::tempdir().expect("create a temporary directory");
    let work = tempfile::tempdir().expect("create a temporary directory");
    let root = source.path();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"a\", \"b\"]\nresolver = \"2\"\n",
    )
    .expect("write");
    for (name, manifest, lib) in [
        (
            "a",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            "pub fn a(x: u8) -> bool { x == 1 }\n",
        ),
        (
            "b",
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\na = { path = \"../a\" }\n",
            "pub fn b(x: u8) -> bool { a::a(x) && x == 1 }\n",
        ),
    ] {
        fs::create_dir_all(root.join(name).join("src")).expect("create");
        fs::write(root.join(name).join("Cargo.toml"), manifest).expect("write");
        fs::write(root.join(name).join("src/lib.rs"), lib).expect("write");
    }
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .env("MBX_DISABLE", "1")
        .args(ARGS)
        .arg(root.join("Cargo.toml"))
        .args(["--package", "b", "--work-dir"])
        .arg(work.path().join("run"))
        .output()
        .expect("the binary runs");
    let full: Value = serde_json::from_slice(&output.stdout).expect("the report is JSON");
    let report = &full["schema"];
    assert_eq!(report["status"], "compiled", "{report}");
    assert_eq!(number(report, "candidates"), 2);
    let other = fs::read_to_string(work.path().join("run/schema/a/src/lib.rs")).expect("the copy");
    assert!(!other.contains("__fallow_schema"));
}
