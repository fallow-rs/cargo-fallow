//! Black-box tests of the CRAP score: `complexity --coverage <LCOV>` and
//! `check --coverage <LCOV>`.
//!
//! Each test writes a small workspace and an LCOV file to a temporary
//! directory. The tests set the line counts of the LCOV file, so each
//! coverage value is known.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// The body of a function with cyclomatic complexity 6 and cognitive
/// complexity 5. With the signature and the closing brace, it has 8 lines.
const SIX_IFS: &str = "    if a == 1 { return 1; }\n    if a == 2 { return 2; }\n    if a == 3 { return 3; }\n    if a == 4 { return 4; }\n    if a == 5 { return 5; }\n    0\n";

/// A function with cyclomatic complexity 6.
fn six(name: &str) -> String {
    format!("pub fn {name}(a: u8) -> u8 {{\n{SIX_IFS}}}\n")
}

/// The workspace: one package `demo`.
struct Demo {
    dir: TempDir,
    /// The text of `src/lib.rs`.
    lib: String,
}

impl Demo {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::create_dir_all(root.join("tests")).expect("create tests");
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace]\n",
        )
        .expect("write the manifest");
        let mut lib = String::from("//! Demo.\n");
        for name in ["risky", "covered", "nodata", "partial"] {
            lib.push_str(&six(name));
        }
        // The closure lines are covered and the lines of `outer` are not.
        lib.push_str("pub fn outer(v: &[u8]) -> usize {\n    let pick = |x: &u8| {\n        let a = *x;\n        a + 1\n    };\n    let a = v.len() as u8;\n");
        lib.push_str(SIX_IFS);
        lib.push_str("    + v.iter().map(pick).count()\n}\n");
        lib.push_str("pub fn generic<T: Into<u8>>(t: T) -> u8 {\n    let a: u8 = t.into();\n");
        lib.push_str(SIX_IFS);
        lib.push_str("}\n");
        lib.push_str("#[cfg(test)]\nmod helpers;\n#[cfg(test)]\nmod tests {\n");
        lib.push_str(&six("in_test_module"));
        lib.push_str("}\n#[test]\n");
        lib.push_str(&six("test_fn").replace("pub fn test_fn(a: u8) -> u8", "fn test_fn()"));
        fs::write(root.join("src/lib.rs"), &lib).expect("write lib.rs");
        fs::write(root.join("src/helpers.rs"), six("helper")).expect("write helpers.rs");
        fs::write(root.join("tests/it.rs"), six("integration")).expect("write it.rs");
        Self { dir, lib }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// The 1-based lines of the function `name` in `src/lib.rs`: from the
    /// signature to the closing brace.
    fn lines_of(&self, name: &str) -> (u32, u32) {
        let lines: Vec<&str> = self.lib.lines().collect();
        let start = lines
            .iter()
            .position(|line| line.contains(&format!("fn {name}")))
            .expect("find the function");
        let end = (start..lines.len())
            .find(|&index| lines[index] == "}")
            .expect("find the end");
        (index_line(start), index_line(end))
    }

    /// Writes `coverage.info` and returns its path.
    fn lcov(&self, text: &str) -> String {
        let path = self.root().join("coverage.info");
        fs::write(&path, text).expect("write the LCOV file");
        path.to_string_lossy().into_owned()
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let output = Command::cargo_bin("cargo-fallow")
            .expect("find the binary")
            .current_dir(self.root())
            .arg("fallow")
            .args(args)
            .arg("--manifest-path")
            .arg(self.root().join("Cargo.toml"))
            .arg("--quiet")
            .output()
            .expect("run cargo-fallow");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }

    fn json(&self, args: &[&str]) -> (i32, Value, String) {
        let mut all = args.to_vec();
        all.extend(["--format", "json"]);
        let (code, stdout, stderr) = self.run(&all);
        let value = serde_json::from_str(&stdout).expect("the output is JSON");
        (code, value, stderr)
    }
}

fn index_line(index: usize) -> u32 {
    u32::try_from(index + 1).expect("a small line number")
}

/// One LCOV record for `path`, with a `DA` line for each line and its count.
fn record(path: &str, lines: &[(u32, u64)]) -> String {
    let mut text = format!("SF:{path}\n");
    for (line, count) in lines {
        let _ = writeln!(text, "DA:{line},{count}");
    }
    text.push_str("end_of_record\n");
    text
}

/// A `DA` entry for each line from `start` to `end`, with `count`.
fn span(start: u32, end: u32, count: u64) -> Vec<(u32, u64)> {
    (start..=end).map(|line| (line, count)).collect()
}

/// The coverage data of the standard case, for the `SF` prefix `prefix`.
fn standard(demo: &Demo, prefix: &str) -> String {
    let mut lines = Vec::new();
    let (start, end) = demo.lines_of("risky");
    lines.extend(span(start, end, 0));
    let (start, end) = demo.lines_of("covered");
    lines.extend(span(start, end, 3));
    // `nodata` has no `DA` line.
    // `partial` has one covered line of 8: the coverage is 12.5 percent.
    let (start, end) = demo.lines_of("partial");
    lines.push((start, 1));
    lines.extend(span(start + 1, end, 0));
    // `outer`: only the lines inside the braces of the closure are covered.
    // The closure owns them, so they do not count for `outer`.
    let (start, end) = demo.lines_of("outer");
    lines.extend(span(start, start + 1, 0));
    lines.extend(span(start + 2, start + 3, 5));
    lines.extend(span(start + 4, end, 0));
    let (start, end) = demo.lines_of("generic");
    lines.extend(span(start, end, 0));
    let mut text = record(&format!("{prefix}/src/lib.rs"), &lines);
    text.push_str(&record(&format!("{prefix}/src/helpers.rs"), &span(1, 8, 0)));
    text.push_str(&record(&format!("{prefix}/tests/it.rs"), &span(1, 8, 0)));
    text
}

fn root_prefix(demo: &Demo) -> String {
    demo.root().to_string_lossy().into_owned()
}

fn names(report: &Value) -> Vec<String> {
    let mut names: Vec<String> = report["findings"]
        .as_array()
        .expect("a findings list")
        .iter()
        .map(|finding| finding["name"].as_str().unwrap_or_default().to_owned())
        .collect();
    names.sort();
    names
}

fn by_name<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["findings"]
        .as_array()
        .expect("a findings list")
        .iter()
        .find(|finding| finding["name"] == name)
        .unwrap_or_else(|| panic!("no finding `{name}` in {report:#}"))
}

#[test]
fn complexity_coverage_reports_each_function_at_or_over_the_crap_limit() {
    let demo = Demo::new();
    let lcov = demo.lcov(&standard(&demo, &root_prefix(&demo)));
    let (code, report, stderr) = demo.json(&["complexity", "--coverage", &lcov]);
    assert_eq!(code, 0, "{report:#}\n{stderr}");
    // `covered`, `nodata` and the test units give no finding. The closure of
    // `outer` is covered, and `outer` is not: the closure lines do not count
    // for `outer`.
    assert_eq!(
        names(&report),
        ["generic", "outer", "partial", "risky"],
        "{report:#}"
    );
    let risky = by_name(&report, "risky");
    assert_eq!(risky["exceeded"], "crap");
    assert_eq!(risky["crap"], 42.0);
    assert_eq!(risky["coverage_pct"], 0.0);
    assert_eq!(risky["coverage_source"], "lcov");
    assert_eq!(risky["actions"][0]["type"], "add-tests");
    let partial = by_name(&report, "partial");
    assert_eq!(partial["crap"], 30.1);
    assert_eq!(partial["coverage_pct"], 12.5);
    assert_eq!(partial["actions"][0]["type"], "increase-coverage");
    assert_eq!(by_name(&report, "outer")["coverage_pct"], 0.0);
    let summary = &report["summary"];
    assert_eq!(summary["max_crap"], 30.0);
    assert_eq!(summary["functions_without_coverage"], 1);
    assert_eq!(summary["stale_files"], serde_json::json!([]));
    let gate = &report["gate_outcomes"]["health-findings"];
    assert_eq!(
        (gate["status"].as_str(), gate["enforced"].as_bool()),
        (Some("fail"), Some(false))
    );

    let (code, _, _) = demo.json(&["complexity", "--coverage", &lcov, "--fail-on-issues"]);
    assert_eq!(code, 1);
}

#[test]
fn a_function_over_the_complexity_limit_shows_its_crap_score_without_data() {
    let demo = Demo::new();
    let lcov = demo.lcov(&standard(&demo, &root_prefix(&demo)));
    let (code, report, _) =
        demo.json(&["complexity", "--coverage", &lcov, "--max-cyclomatic", "5"]);
    assert_eq!(code, 0);
    let risky = by_name(&report, "risky");
    assert_eq!(risky["exceeded"], "cyclomatic_crap");
    let covered = by_name(&report, "covered");
    assert_eq!(covered["exceeded"], "cyclomatic");
    assert_eq!(covered["crap"], 6.0);
    // A unit with no `DA` line has a null score and no CRAP part.
    let nodata = by_name(&report, "nodata");
    assert_eq!(nodata["exceeded"], "cyclomatic");
    assert!(nodata["crap"].is_null(), "{nodata:#}");
    assert!(nodata["coverage_pct"].is_null(), "{nodata:#}");
    assert!(nodata.get("crap").is_some(), "{nodata:#}");
}

#[test]
fn without_coverage_the_report_has_no_crap_field() {
    let demo = Demo::new();
    let (code, report, _) = demo.json(&["complexity", "--max-cyclomatic", "5"]);
    assert_eq!(code, 0);
    let risky = by_name(&report, "risky");
    assert_eq!(risky["exceeded"], "cyclomatic");
    for key in ["crap", "coverage_pct", "coverage_source"] {
        assert!(risky.get(key).is_none(), "{key} in {risky:#}");
    }
    for key in ["max_crap", "functions_without_coverage", "stale_files"] {
        assert!(report["summary"].get(key).is_none(), "{key}");
    }
}

#[test]
fn coverage_root_remaps_the_paths_of_another_machine() {
    let demo = Demo::new();
    let lcov = demo.lcov(&standard(&demo, "/ci/work"));
    let (code, report, _) = demo.json(&["complexity", "--coverage", &lcov]);
    assert_eq!(code, 2, "{report:#}");
    assert_eq!(report["code"], "CARGO_FALLOW_INVALID_ARGUMENT");
    assert!(
        report["help"]
            .as_str()
            .is_some_and(|help| help.contains("--coverage-root")),
        "{report:#}"
    );
    let (code, report, _) = demo.json(&[
        "complexity",
        "--coverage",
        &lcov,
        "--coverage-root",
        "/ci/work",
    ]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(names(&report), ["generic", "outer", "partial", "risky"]);
}

#[test]
fn a_missing_coverage_file_is_an_error() {
    let demo = Demo::new();
    let (code, report, _) = demo.json(&["complexity", "--coverage", "nothing.info"]);
    assert_eq!(code, 2);
    assert_eq!(report["code"], "CARGO_FALLOW_INVALID_ARGUMENT");
}

#[test]
fn stale_coverage_data_gives_a_warning_and_a_summary_entry() {
    let demo = Demo::new();
    let prefix = root_prefix(&demo);
    let mut text = standard(&demo, &prefix);
    text.push_str(&record(&format!("{prefix}/src/gone.rs"), &[(1, 1)]));
    text.push_str(&record(&format!("{prefix}/src/helpers.rs"), &[(900, 1)]));
    let lcov = demo.lcov(&text);
    let (code, report, stderr) = demo.json(&["complexity", "--coverage", &lcov]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(
        report["summary"]["stale_files"],
        serde_json::json!(["src/gone.rs", "src/helpers.rs"])
    );
    assert!(stderr.contains("src/gone.rs"), "{stderr}");
    assert!(stderr.contains("src/helpers.rs"), "{stderr}");
}

#[test]
fn a_crap_limit_of_zero_turns_the_check_off() {
    let demo = Demo::new();
    let manifest = demo.root().join("Cargo.toml");
    let mut text = fs::read_to_string(&manifest).expect("read the manifest");
    text.push_str("\n[workspace.metadata.fallow.complexity]\nmax-crap = 0\n");
    fs::write(&manifest, text).expect("write the manifest");
    let lcov = demo.lcov(&standard(&demo, &root_prefix(&demo)));
    let (code, report, _) = demo.json(&["complexity", "--coverage", &lcov]);
    assert_eq!(code, 0, "{report:#}");
    assert!(names(&report).is_empty(), "{report:#}");
}

#[test]
fn a_crap_rule_set_to_warn_keeps_the_finding_out_of_the_gate() {
    let demo = Demo::new();
    let manifest = demo.root().join("Cargo.toml");
    let mut text = fs::read_to_string(&manifest).expect("read the manifest");
    text.push_str("\n[workspace.metadata.fallow.rules]\ncomplexity-crap = \"warn\"\n");
    fs::write(&manifest, text).expect("write the manifest");
    let lcov = demo.lcov(&standard(&demo, &root_prefix(&demo)));
    let (code, report, _) = demo.json(&["complexity", "--coverage", &lcov, "--fail-on-issues"]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(names(&report).len(), 4);
}

#[test]
fn the_suppression_comment_hides_a_crap_finding() {
    let demo = Demo::new();
    let lib = demo.root().join("src/lib.rs");
    let text = fs::read_to_string(&lib).expect("read lib.rs");
    // The comment replaces the first line, so the line numbers stay.
    let text = text.replacen(
        "//! Demo.\n",
        "// fallow-ignore-next-line complexity-crap\n",
        1,
    );
    fs::write(&lib, text).expect("write lib.rs");
    let lcov = demo.lcov(&standard(&demo, &root_prefix(&demo)));
    let (code, report, _) = demo.json(&["complexity", "--coverage", &lcov]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(names(&report), ["generic", "outer", "partial"]);
    assert_eq!(report["summary"]["suppressed"], 1);
}

#[test]
fn check_coverage_adds_the_crap_findings_to_the_complexity_section() {
    let demo = Demo::new();
    let lcov = demo.lcov(&standard(&demo, &root_prefix(&demo)));
    let (code, report, _) = demo.json(&["check", "--only", "complexity", "--coverage", &lcov]);
    assert_eq!(code, 0, "{report:#}");
    let section = &report["sections"]["complexity"];
    assert_eq!(names(section), ["generic", "outer", "partial", "risky"]);

    let (code, report, _) = demo.json(&["check", "--skip", "complexity", "--coverage", &lcov]);
    assert_eq!(code, 2, "{report:#}");
    assert_eq!(report["code"], "CARGO_FALLOW_INVALID_ARGUMENT");
}

#[test]
fn the_human_output_and_sarif_name_the_crap_score() {
    let demo = Demo::new();
    let lcov = demo.lcov(&standard(&demo, &root_prefix(&demo)));
    let (_, human, _) = demo.run(&["complexity", "--coverage", &lcov]);
    assert!(human.contains("crap 42.0"), "{human}");
    let (_, sarif, _) = demo.run(&["complexity", "--coverage", &lcov, "--format", "sarif"]);
    let sarif: Value = serde_json::from_str(&sarif).expect("SARIF is JSON");
    let rules: Vec<&str> = sarif["runs"][0]["results"]
        .as_array()
        .expect("results")
        .iter()
        .filter_map(|result| result["ruleId"].as_str())
        .collect();
    assert_eq!(rules.len(), 4, "{sarif:#}");
    assert!(
        rules.iter().all(|rule| *rule == "complexity-crap"),
        "{rules:?}"
    );
}
