//! Black-box tests of the `boundaries.cfg` rules: a path from code that a
//! build compiles to an item that the same build does not compile.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// The rule of the tests: a test build, and a test build under Miri.
const RULE: &str = "[[workspace.metadata.fallow.boundaries.cfg]]\nrule = \"miri-build\"\nbuilds = [[\"test\"], [\"test\", \"miri\"]]\n";

/// A helper module that the Miri build does not compile.
const HELPERS: &str = "#[cfg(all(test, not(miri)))]\nmod tests {\n    pub fn parse_ts() {}\n}\n";

/// Writes a one-crate workspace with `rule` and the files.
fn workspace(rule: &str, files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    write(
        dir.path(),
        "Cargo.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[workspace]\n\n[features]\nx = []\n\n{rule}"
        ),
    );
    for (path, text) in files {
        write(dir.path(), path, text);
    }
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("a file path has a parent")).expect("create a folder");
    fs::write(full, text).expect("write a file");
}

fn boundaries(dir: &TempDir) -> (i32, Value) {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args(["fallow", "boundaries", "--format", "json", "--quiet"])
        .output()
        .expect("run the binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report = serde_json::from_str(&stdout)
        .unwrap_or_else(|err| panic!("stdout is JSON ({err}): {stdout}"));
    (output.status.code().unwrap_or(-1), report)
}

/// The `path:line` of each finding.
fn places(report: &Value) -> Vec<String> {
    report["findings"]
        .as_array()
        .expect("the report has findings")
        .iter()
        .map(|finding| {
            format!(
                "{}:{}",
                finding["path"].as_str().unwrap_or_default(),
                finding["line"]
            )
        })
        .collect()
}

/// The one-based line of the first line of `text` that holds `needle`.
fn line_of(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|line| line.contains(needle))
        .expect("the text holds the needle")
        + 1
}

#[test]
fn a_path_to_an_item_that_the_build_leaves_out_is_a_finding() {
    let lib = format!(
        "{HELPERS}#[cfg(test)]\nmod checks {{\n    #[test]\n    fn uses() {{\n        crate::tests::parse_ts();\n    }}\n}}\n"
    );
    let dir = workspace(RULE, &[("src/lib.rs", &lib)]);
    let (code, report) = boundaries(&dir);
    assert_eq!(code, 0, "{report}");
    let line = line_of(&lib, "crate::tests::parse_ts");
    assert_eq!(
        places(&report),
        [format!("src/lib.rs:{line}")],
        "{report:#}"
    );
    let finding = &report["findings"][0];
    assert_eq!(finding["rule"], "miri-build");
    assert_eq!(finding["kind"], "cfg-path");
    let message = finding["message"].as_str().unwrap_or_default();
    assert!(message.contains("parse_ts"), "{message}");
    assert!(message.contains("test, miri"), "{message}");
}

#[test]
fn an_import_that_the_build_compiles_is_a_finding_too() {
    let lib = format!("{HELPERS}#[cfg(test)]\nuse crate::tests::parse_ts;\n");
    let dir = workspace(RULE, &[("src/lib.rs", &lib)]);
    let (_, report) = boundaries(&dir);
    // The place of an import is the first line of its `use` item: here the
    // line of its attribute.
    let line = line_of(&lib, "use crate::tests") - 1;
    assert_eq!(
        places(&report),
        [format!("src/lib.rs:{line}")],
        "{report:#}"
    );
}

#[test]
fn a_file_module_carries_the_cfg_of_its_declaration() {
    let lib = "#[cfg(all(test, not(miri)))]\nmod helpers;\n#[cfg(test)]\nmod checks;\n";
    let checks = "#[test]\nfn uses() {\n    crate::helpers::parse_ts();\n}\n";
    let dir = workspace(
        RULE,
        &[
            ("src/lib.rs", lib),
            ("src/helpers.rs", "pub fn parse_ts() {}\n"),
            ("src/checks.rs", checks),
        ],
    );
    let (_, report) = boundaries(&dir);
    assert_eq!(places(&report), ["src/checks.rs:3"], "{report:#}");
}

#[test]
fn an_item_with_a_variant_for_the_build_gives_no_finding() {
    let lib = format!(
        "{HELPERS}#[cfg(all(test, miri))]\nmod tests {{\n    pub fn parse_ts() {{}}\n}}\n#[cfg(test)]\nfn uses() {{\n    crate::tests::parse_ts();\n}}\n"
    );
    let (_, report) = boundaries(&workspace(RULE, &[("src/lib.rs", &lib)]));
    assert_eq!(places(&report), Vec::<String>::new(), "{report:#}");
}

#[test]
fn a_cfg_on_the_statement_of_the_path_counts() {
    let lib = format!(
        "{HELPERS}#[cfg(test)]\nfn uses() {{\n    #[cfg(not(miri))]\n    crate::tests::parse_ts();\n}}\n"
    );
    let (_, report) = boundaries(&workspace(RULE, &[("src/lib.rs", &lib)]));
    assert_eq!(places(&report), Vec::<String>::new(), "{report:#}");
}

#[test]
fn a_cfg_that_the_builds_do_not_set_gives_credit() {
    // `feature = "x"` and `unix` are not in a build, so the syntax cannot
    // decide, and no finding comes.
    let lib = "#[cfg(feature = \"x\")]\nmod feat {\n    pub fn f() {}\n}\n#[cfg(unix)]\nmod os {\n    pub fn g() {}\n}\n#[cfg(test)]\nfn uses() {\n    crate::feat::f();\n    crate::os::g();\n}\n#[cfg_attr(test, cfg(miri))]\nfn maybe() {\n    crate::tests::parse_ts();\n}\n";
    let lib = format!("{HELPERS}{lib}");
    let (_, report) = boundaries(&workspace(RULE, &[("src/lib.rs", &lib)]));
    assert_eq!(places(&report), Vec::<String>::new(), "{report:#}");
}

#[test]
fn without_a_cfg_rule_the_check_reports_nothing_new() {
    let lib = format!("{HELPERS}#[cfg(test)]\nfn uses() {{\n    crate::tests::parse_ts();\n}}\n");
    let (code, report) = boundaries(&workspace("", &[("src/lib.rs", &lib)]));
    assert_eq!(code, 0, "{report}");
    assert_eq!(places(&report), Vec::<String>::new(), "{report:#}");
}

#[test]
fn a_rule_without_a_build_is_a_config_error() {
    let rule = "[[workspace.metadata.fallow.boundaries.cfg]]\nrule = \"none\"\nbuilds = []\n";
    let (code, report) = boundaries(&workspace(rule, &[("src/lib.rs", "")]));
    assert_eq!(code, 2, "{report}");
    assert_eq!(report["code"], "CARGO_FALLOW_INVALID_CONFIG", "{report}");
}

/// A rule with a test build and a build without tests.
const TWO_BUILDS: &str = "[[workspace.metadata.fallow.boundaries.cfg]]\nrule = \"two\"\nbuilds = [[\"test\"], [\"miri\"]]\n";

#[test]
fn an_item_of_another_crate_gives_credit() {
    // An integration test compiles the library without `cfg(test)`.
    let lib = "#[cfg(not(test))]\npub fn real() {}\n";
    let it = "use app::real;\n#[test]\nfn uses() {\n    real();\n    ::app::real();\n}\n";
    let dir = workspace(RULE, &[("src/lib.rs", lib), ("tests/it.rs", it)]);
    let (_, report) = boundaries(&dir);
    assert_eq!(places(&report), Vec::<String>::new(), "{report:#}");
}

#[test]
fn a_test_attribute_is_a_test_condition() {
    let lib = "#[cfg(test)]\nmod h {\n    pub fn helper() {}\n}\n#[test]\nfn t() {\n    crate::h::helper();\n}\n";
    let (_, report) = boundaries(&workspace(TWO_BUILDS, &[("src/lib.rs", lib)]));
    assert_eq!(places(&report), Vec::<String>::new(), "{report:#}");
}

#[test]
fn a_path_that_can_also_resolve_outside_the_workspace_gives_credit() {
    let lib = "#[cfg(test)]\nmod fake {\n    #[derive(Default)]\n    pub struct Client;\n}\n#[cfg(test)]\nuse fake::Client;\n#[cfg(not(test))]\nuse std::string::String as Client;\npub fn make() {\n    let _ = self::Client::default();\n}\n";
    let (_, report) = boundaries(&workspace(TWO_BUILDS, &[("src/lib.rs", lib)]));
    assert_eq!(places(&report), Vec::<String>::new(), "{report:#}");
}

#[test]
fn a_bare_name_that_a_local_item_can_shadow_gives_credit() {
    let lib =
        "#[cfg(test)]\nfn helper() {}\npub fn run() {\n    fn helper() {}\n    helper();\n}\n";
    let (_, report) = boundaries(&workspace(TWO_BUILDS, &[("src/lib.rs", lib)]));
    assert_eq!(places(&report), Vec::<String>::new(), "{report:#}");
}

#[test]
fn a_module_with_a_macro_call_at_item_level_gives_credit() {
    let lib = "macro_rules! other_gen {\n    () => {\n        #[cfg(not(test))]\n        mod gen {\n            pub fn x() {}\n        }\n    };\n}\nother_gen!();\n#[cfg(test)]\nmod gen {\n    pub fn x() {}\n}\npub fn run() {\n    crate::gen::x();\n}\n";
    let (_, report) = boundaries(&workspace(TWO_BUILDS, &[("src/lib.rs", lib)]));
    assert_eq!(places(&report), Vec::<String>::new(), "{report:#}");
}

#[test]
fn a_build_option_must_be_a_name_or_a_name_with_a_value() {
    let rule = "[[workspace.metadata.fallow.boundaries.cfg]]\nrule = \"bad\"\nbuilds = [[\"not(miri)\"]]\n";
    let (code, report) = boundaries(&workspace(rule, &[("src/lib.rs", "")]));
    assert_eq!(code, 2, "{report}");
    assert_eq!(report["code"], "CARGO_FALLOW_INVALID_CONFIG", "{report}");
}

/// A rule with two platforms.
const PLATFORMS: &str = "[[workspace.metadata.fallow.boundaries.cfg]]\nrule = \"platforms\"\nbuilds = [[\"unix\"], [\"windows\"]]\n";

#[test]
fn two_declarations_of_one_module_give_credit() {
    let lib = "#[cfg(unix)]\nmod sys;\n#[cfg(windows)]\n#[path = \"sys_win.rs\"]\nmod sys;\npub fn run() {\n    crate::sys::f();\n}\n";
    let files = [
        ("src/lib.rs", lib),
        ("src/sys.rs", "pub fn f() {}\n"),
        ("src/sys_win.rs", "pub fn f() {}\n"),
    ];
    let (_, report) = boundaries(&workspace(PLATFORMS, &files));
    assert_eq!(places(&report), Vec::<String>::new(), "{report:#}");
}

#[test]
fn a_glob_import_in_the_module_of_the_target_gives_credit() {
    // Without `test`, the glob import gives `m`.
    let lib = "#[cfg(test)]\nmod m {\n    pub fn f() {}\n}\npub use crate::fallback::*;\nmod fallback {\n    pub mod m {\n        pub fn f() {}\n    }\n}\npub fn run() {\n    crate::m::f();\n}\n";
    let (_, report) = boundaries(&workspace(TWO_BUILDS, &[("src/lib.rs", lib)]));
    assert_eq!(places(&report), Vec::<String>::new(), "{report:#}");
}

#[test]
fn a_macro_call_in_a_module_on_the_path_gives_credit() {
    let lib = "mod m {\n    #[cfg(test)]\n    pub fn f() {}\n}\nmacro_rules! other_f {\n    () => {\n        #[cfg(not(test))]\n        pub fn f() {}\n    };\n}\nmod api {\n    #[cfg(test)]\n    pub use crate::m::f;\n    other_f!();\n}\npub fn run() {\n    crate::api::f();\n}\n";
    let (_, report) = boundaries(&workspace(TWO_BUILDS, &[("src/lib.rs", lib)]));
    assert_eq!(places(&report), Vec::<String>::new(), "{report:#}");
}

#[test]
fn an_import_that_a_local_item_can_shadow_gives_credit() {
    let lib = "#[cfg(test)]\nenum Mode {\n    A,\n}\npub fn run() {\n    enum Mode {\n        A,\n    }\n    use Mode::A;\n    let _ = A;\n}\n";
    let (_, report) = boundaries(&workspace(TWO_BUILDS, &[("src/lib.rs", lib)]));
    assert_eq!(places(&report), Vec::<String>::new(), "{report:#}");
}

#[test]
fn another_test_attribute_and_a_test_cfg_attr_give_credit() {
    let lib = "#[cfg(test)]\nmod h {\n    pub fn helper() {}\n}\n#[test_case(1)]\nfn check(_: u8) {}\npub fn run() {\n    crate::check(1);\n}\n#[cfg_attr(miri, test)]\nfn t() {\n    crate::h::helper();\n}\n";
    let (_, report) = boundaries(&workspace(TWO_BUILDS, &[("src/lib.rs", lib)]));
    assert_eq!(places(&report), Vec::<String>::new(), "{report:#}");
}

#[test]
fn only_the_built_in_bench_attribute_is_a_test_condition() {
    let lib = "#[cfg(test)]\nmod h {\n    pub fn helper() {}\n}\n#[m::bench]\nfn work() {\n    crate::h::helper();\n}\n#[bench]\nfn b() {\n    crate::h::helper();\n}\n#[test::bench]\nfn c() {\n    crate::h::helper();\n}\n#[::test::bench]\nfn d() {\n    crate::h::helper();\n}\n";
    let (_, report) = boundaries(&workspace(TWO_BUILDS, &[("src/lib.rs", lib)]));
    let line = line_of(lib, "#[m::bench]") + 2;
    assert_eq!(
        places(&report),
        [format!("src/lib.rs:{line}")],
        "{report:#}"
    );
}
