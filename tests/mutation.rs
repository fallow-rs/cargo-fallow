//! Black-box tests of the `mutation` command: real cargo, rustc and nextest.
//!
//! Each test runs mutated binaries under the memory guard and a time limit. No
//! fixture can grow without limit in a way that a broken guard cannot stop: a
//! fixture that allocates sleeps between blocks, so the time limit bounds it.
//!
//! The memory guard has no probe on Windows, so the file is for Unix only.
#![cfg(unix)]
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;
use std::process::{Command as Process, Stdio};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

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

struct Run {
    report: Value,
    stderr: String,
    code: Option<i32>,
    source: TempDir,
    work: TempDir,
}

impl Run {
    fn mutants(&self) -> &Vec<Value> {
        self.report["mutants"].as_array().expect("the mutants")
    }

    /// The mutants of one operator text, as `(replacement, status)`.
    fn with_operator(&self, operator: &str) -> Vec<&Value> {
        self.mutants()
            .iter()
            .filter(|mutant| mutant["operator"] == operator)
            .collect()
    }

    fn find(&self, replacement: &str) -> &Value {
        self.mutants()
            .iter()
            .find(|mutant| mutant["replacement"] == replacement)
            .unwrap_or_else(|| panic!("no mutant `{replacement}`: {}", self.report))
    }
}

/// One run at a time: each run has up to two test processes, and that is the
/// limit for this machine. `.config/nextest.toml` gives the same limit to nextest.
static ONE_RUN: Mutex<()> = Mutex::new(());

fn run(files: &[(&str, &str)], flags: &[&str]) -> Run {
    let _one_run = ONE_RUN.lock().unwrap_or_else(PoisonError::into_inner);
    let source = tempfile::tempdir().expect("create a temporary directory");
    let work = tempfile::tempdir().expect("create a temporary directory");
    write_crate(source.path(), files);
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .env("MBX_DISABLE", "1")
        .args([
            "mutation",
            "--format",
            "json",
            "--jobs",
            "2",
            "--manifest-path",
        ])
        .arg(source.path().join("Cargo.toml"))
        .arg("--work-dir")
        .arg(work.path().join("run"))
        .args(flags)
        .timeout(Duration::from_secs(280))
        .output()
        .expect("the binary runs");
    let report = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    Run {
        report,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        code: output.status.code(),
        source,
        work,
    }
}

const CHECK: &str = r"
/// Has a test.
pub fn check(a: u8) -> bool {
    a == 1
}

/// Has no test that sees a difference.
pub fn unseen(a: u8) -> bool {
    a == 2
}

#[cfg(test)]
mod tests {
    #[test]
    fn check_works() {
        assert!(super::check(1));
        assert!(!super::check(2));
        let _ = super::unseen(1);
    }
}
";

#[test]
fn schema_mutants_are_killed_or_survive() {
    let run = run(&[("src/lib.rs", CHECK)], &["--tier", "schema"]);
    let report = &run.report;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(report["kind"], "mutation", "{report}");
    assert_eq!(report["summary"]["killed"], 1, "{report}");
    assert_eq!(report["summary"]["survived"], 1, "{report}");
    assert_eq!(report["summary"]["score"], 50.0, "{report}");
    let mutants = run.mutants();
    assert_eq!(mutants.len(), 2);
    assert!(mutants.iter().all(|m| m["tier"] == "schema"), "{report}");
    let check = &mutants[0];
    assert_eq!(check["status"], "killed", "{report}");
    assert_eq!(check["file"], "src/lib.rs");
    assert_eq!(check["line"], 4);
    assert_eq!(check["operator"], "relational-operator");
    assert!(
        check["killed_by"]
            .as_str()
            .is_some_and(|name| name.ends_with("check_works")),
        "{report}"
    );
    let id = check["id"].as_str().expect("an id");
    assert_eq!(id.len(), 16, "{id}");
    let unseen = &mutants[1];
    assert_eq!(unseen["status"], "survived", "{report}");
    assert_eq!(unseen["line"], 9);
    assert_eq!(unseen["retried"], true, "{report}");
}

/// The switch must select the same behavior as the plain edit of the source.
#[test]
fn a_schema_mutant_has_the_same_result_in_a_rebuild() {
    let schema = run(&[("src/lib.rs", CHECK)], &["--tier", "schema"]);
    let rebuild = run(
        &[("src/lib.rs", CHECK)],
        &["--tier", "schema", "--no-schema"],
    );
    assert_eq!(rebuild.code, Some(0), "{}", rebuild.stderr);
    assert_eq!(schema.mutants().len(), rebuild.mutants().len());
    for (a, b) in schema.mutants().iter().zip(rebuild.mutants()) {
        assert_eq!(a["id"], b["id"], "{a} {b}");
        assert_eq!(a["status"], b["status"], "{a} {b}");
        assert_eq!(a["tier"], "schema");
        assert_eq!(b["tier"], "rebuild");
        assert_eq!(b["removed_from_schema"], false);
    }
    assert_eq!(
        rebuild.report["schema"]["status"], "not_run",
        "{}",
        rebuild.report
    );
}

#[test]
fn the_source_tree_stays_untouched_after_a_run() {
    let run = run(&[("src/lib.rs", CHECK)], &["--tier", "all"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let source = run.source.path();
    assert_eq!(
        fs::read_to_string(source.join("src/lib.rs")).expect("read"),
        CHECK
    );
    assert!(!source.join("target").exists());
    assert!(!source.join("Cargo.lock").exists());
}

const ARITHMETIC: &str = r#"
/// Has a test.
pub fn add(a: u8, b: u8) -> u8 {
    a + b
}

/// `String` has no `Sub`, so the swap does not compile.
pub fn join(a: String) -> String {
    a + "x"
}

/// Only the first arm has a test.
pub fn label(a: u8) -> u8 {
    match a {
        1 => 10,
        2 => 20,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn add_works() {
        assert_eq!(super::add(2, 1), 3);
        assert_eq!(super::label(1), 10);
    }
}
"#;

#[test]
fn rebuild_mutants_run_in_their_own_tier() {
    let run = run(&[("src/lib.rs", ARITHMETIC)], &["--tier", "rebuild"]);
    let report = &run.report;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(
        run.mutants().iter().all(|m| m["tier"] == "rebuild"),
        "{report}"
    );
    let arithmetic = run.with_operator("arithmetic-operator");
    assert_eq!(arithmetic.len(), 2, "{report}");
    assert_eq!(arithmetic[0]["status"], "killed", "{report}");
    assert_eq!(arithmetic[1]["status"], "compile_error", "{report}");
    assert!(
        arithmetic[1]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("cannot subtract")),
        "{report}"
    );
    let arms = run.with_operator("deleted-match-arm");
    assert_eq!(arms.len(), 2, "{report}");
    assert_eq!(arms[0]["status"], "killed", "{report}");
    assert_eq!(arms[1]["status"], "survived", "{report}");
    let returns = run.with_operator("return-value");
    assert!(!returns.is_empty(), "{report}");
    assert!(returns.iter().any(|m| m["status"] == "killed"), "{report}");
    let scratch = run.work.path().join("run/scratch/src/lib.rs");
    assert_eq!(
        fs::read_to_string(scratch).expect("the scratch copy"),
        ARITHMETIC,
        "the last mutant must be gone from the scratch copy"
    );
}

/// A crate with `deny(warnings)` must not turn a lint in a mutant into a compile
/// error: the mutant is valid code, and the tests can judge it.
#[test]
fn a_lint_in_a_rebuild_mutant_is_not_a_compile_error() {
    let lib = r"
#![deny(warnings)]
/// The return value replaces the only use of the parameter.
pub fn same(a: bool) -> bool {
    a
}

#[cfg(test)]
mod tests {
    #[test]
    fn same_works() {
        assert!(super::same(true));
        assert!(!super::same(false));
    }
}
";
    let run = run(&[("src/lib.rs", lib)], &["--tier", "rebuild"]);
    let report = &run.report;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let returns = run.with_operator("return-value");
    assert_eq!(returns.len(), 2, "{report}");
    assert!(returns.iter().all(|m| m["status"] == "killed"), "{report}");
}

/// The mutant `left <= 0` never ends and grows by 1 MiB for each step. A sleep
/// bounds the rate, so the time limit bounds the memory if the guard is broken.
const GROW: &str = r"
/// Grows and shrinks.
pub fn grow(start: i64) -> usize {
    let mut left = start;
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    while left != 0 {
        blocks.push(vec![1_u8; 1 << 20]);
        std::thread::sleep(std::time::Duration::from_millis(5));
        left = if left > 0 { left - 1 } else { left + 1 };
    }
    blocks.len()
}

#[cfg(test)]
mod tests {
    #[test]
    fn grow_works() {
        assert_eq!(super::grow(5), 5);
    }
}
";

#[test]
fn the_guard_kills_a_mutant_that_allocates_without_limit() {
    let run = run(
        &[("src/lib.rs", GROW)],
        &[
            "--tier",
            "schema",
            "--memory-limit-mib",
            "64",
            "--timeout",
            "60",
        ],
    );
    let report = &run.report;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let mutant = run.find("<=");
    assert_eq!(mutant["status"], "runtime_error", "{report}");
    let reason = mutant["reason"].as_str().expect("a reason");
    assert!(reason.contains("memory guard"), "{reason}");
    assert!(reason.contains("over the limit of 64 MiB"), "{reason}");
    assert!(report["guard"]["kills"].as_u64() >= Some(1), "{report}");
    assert_eq!(report["guard"]["enabled"], true, "{report}");
    assert_eq!(report["guard"]["memory_limit_mib"], 64, "{report}");
    // The kill comes long before the time limit of 60 s for this mutant.
    assert!(mutant["duration_ms"].as_u64() < Some(60_000), "{report}");
}

/// This fixture has no sleep, so it grows as fast as the machine can write. On
/// 2026-11-01 such a mutant reached 24 GiB before a kill, because the guard waited
/// for `top`. The cap on the blocks bounds the memory if the guard is broken.
#[test]
fn the_guard_stops_a_fast_mutant_near_the_limit() {
    let lib = r"
/// Grows and shrinks.
pub fn grow(start: i64) -> usize {
    let mut left = start;
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    while left != 0 && blocks.len() < 6144 {
        blocks.push(vec![1_u8; 1 << 20]);
        left = if left > 0 { left - 1 } else { left + 1 };
    }
    blocks.len()
}

#[test]
fn grow_works() {
    assert_eq!(grow(5), 5);
}
";
    let run = run(
        &[("src/lib.rs", lib)],
        &["--tier", "schema", "--memory-limit-mib", "256"],
    );
    let report = &run.report;
    assert_eq!(run.code, Some(0), "{report}");
    let mutant = run
        .mutants()
        .iter()
        .find(|mutant| mutant["original"] == "left > 0" && mutant["replacement"] == "<=")
        .unwrap_or_else(|| panic!("{report}"));
    let reason = mutant["reason"]
        .as_str()
        .unwrap_or_else(|| panic!("{report}"));
    let reached: u64 = reason
        .split_whitespace()
        .find_map(|word| word.parse().ok())
        .unwrap_or_else(|| panic!("{reason}"));
    assert!(reached < 3072, "{reason}");
}

#[test]
fn a_test_over_the_limit_with_no_mutant_stops_the_run_with_the_reason() {
    let lib = r"
/// Has a test.
pub fn f(x: u8) -> bool {
    x == 1
}

#[test]
fn large() {
    let mut blocks = Vec::new();
    for _ in 0..512 {
        blocks.push(vec![1_u8; 1 << 20]);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(f(1) && !blocks.is_empty());
}
";
    let run = run(
        &[("src/lib.rs", lib)],
        &["--tier", "schema", "--memory-limit-mib", "64"],
    );
    assert_eq!(run.code, Some(2), "{}", run.report);
    let message = run.report["message"].as_str().expect("a message");
    assert!(message.contains("over the limit of 64 MiB"), "{message}");
    let help = run.report["help"].as_str().expect("a hint");
    assert!(help.contains("--memory-limit-mib"), "{help}");
}

#[test]
fn a_mutant_that_never_ends_gets_a_timeout() {
    let lib = r"
/// Counts down.
pub fn spin(mut left: u32) -> u32 {
    while left != 0 {
        left = if left > 100 { left } else { left - 1 };
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    left
}

#[cfg(test)]
mod tests {
    #[test]
    fn spin_works() {
        assert_eq!(super::spin(3), 0);
    }
}
";
    let run = run(
        &[("src/lib.rs", lib)],
        &["--tier", "schema", "--timeout", "3"],
    );
    let report = &run.report;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    // `left <= 100` is the mutant `<=` of `>`: the value stays at 3.
    let mutant = run.find("<=");
    assert_eq!(mutant["status"], "timeout", "{report}");
    assert_eq!(
        report["baseline"]["timeout_ms"]["schema_nextest"], 3000,
        "{report}"
    );
}

#[test]
fn a_mutant_that_no_build_reads_has_no_coverage() {
    let lib = r"
/// Built.
pub fn built(x: u8) -> bool { x == 1 }

/// Not built.
#[cfg(any())]
pub fn off(x: u8) -> bool { x == 2 }

#[cfg(test)]
mod tests {
    #[test]
    fn built_works() {
        assert!(super::built(1));
    }
}
";
    let run = run(&[("src/lib.rs", lib)], &["--tier", "all"]);
    let report = &run.report;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let off: Vec<&Value> = run
        .mutants()
        .iter()
        .filter(|mutant| mutant["line"] == 7)
        .collect();
    // `==` in the schema tier, and two return values in the rebuild tier.
    assert_eq!(off.len(), 3, "{report}");
    for mutant in off {
        assert_eq!(mutant["status"], "no_coverage", "{report}");
        assert!(
            mutant["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("no build reads")),
            "{report}"
        );
    }
}

#[test]
fn a_mutant_of_a_binary_target_is_read_after_the_library_compiles() {
    let main = "fn main() { println!(\"{}\", is_one(1)); }\nfn is_one(x: u8) -> bool { x == 1 }\n\
                #[test]\nfn one() { assert!(is_one(1)); }\n";
    let run = run(
        &[("src/lib.rs", CHECK), ("src/main.rs", main)],
        &["--tier", "schema"],
    );
    let report = &run.report;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let in_main: Vec<&Value> = run
        .mutants()
        .iter()
        .filter(|mutant| mutant["file"] == "src/main.rs")
        .collect();
    assert_eq!(in_main.len(), 1, "{report}");
    assert_eq!(in_main[0]["status"], "killed", "{report}");
}

#[test]
fn a_mutant_that_breaks_the_schema_runs_in_a_rebuild() {
    // The `&'static` borrow needs constant promotion, and a switch stops it. The
    // mutant itself compiles.
    let lib = r"
/// Promoted.
pub fn promoted() -> &'static bool {
    &(1 == 2)
}

#[cfg(test)]
mod tests {
    #[test]
    fn promoted_works() {
        assert!(!*super::promoted());
    }
}
";
    let all = run(&[("src/lib.rs", lib)], &["--tier", "all"]);
    let report = &all.report;
    assert_eq!(all.code, Some(0), "{}", all.stderr);
    let mutant = all.find("!=");
    assert_eq!(mutant["tier"], "rebuild", "{report}");
    assert_eq!(mutant["removed_from_schema"], true, "{report}");
    assert_eq!(mutant["status"], "killed", "{report}");
    assert_eq!(report["schema"]["removed"][0]["code"], "E0515", "{report}");

    let schema = run(&[("src/lib.rs", lib)], &["--tier", "schema"]);
    assert_eq!(
        schema.find("!=")["status"],
        "compile_error",
        "{}",
        schema.report
    );
}

#[test]
fn a_test_that_fails_with_no_mutant_decides_nothing() {
    let lib = r#"
/// Has a test.
pub fn check(a: u8) -> bool {
    a == 1
}

#[cfg(test)]
mod tests {
    #[test]
    fn check_works() {
        assert!(super::check(1));
    }

    #[test]
    fn broken() {
        panic!("always");
    }
}
"#;
    let run = run(&[("src/lib.rs", lib)], &["--tier", "schema"]);
    let report = &run.report;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(
        report["baseline"]["failures"].as_array().map(Vec::len),
        Some(1),
        "{report}"
    );
    let mutant = run.find("!=");
    assert_eq!(mutant["status"], "killed", "{report}");
    assert!(
        mutant["killed_by"]
            .as_str()
            .is_some_and(|name| name.ends_with("check_works")),
        "{report}"
    );
}

#[test]
fn a_test_that_breaks_with_the_schema_does_not_kill() {
    // The arm holds a copy of the multi-line operand. That moves the code under
    // it, so `line!()` differs in the schema build.
    let lib = "pub fn f(x: u8) -> bool {\n    x\n    .max(1)\n    == 1\n}\n\npub fn line() -> u32 {\n    line!()\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn line_is_stable() {\n        assert_eq!(super::line(), 8);\n    }\n\n    #[test]\n    fn other() {\n        assert!(super::f(1));\n    }\n}\n";
    let run = run(&[("src/lib.rs", lib)], &["--tier", "schema"]);
    let report = &run.report;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(
        report["baseline"]["regressions"].as_array().map(Vec::len),
        Some(1),
        "{report}"
    );
    let mutant = run.find("!=");
    assert_eq!(mutant["status"], "killed", "{report}");
    assert!(
        mutant["killed_by"]
            .as_str()
            .is_some_and(|name| name.ends_with("other")),
        "{report}"
    );
    // The line of a mutant is the line in the original file.
    assert_eq!(mutant["line"], 2, "{report}");
}

#[test]
fn only_failing_tests_stop_the_run() {
    let lib =
        "pub fn f(x: u8) -> bool { x == 1 }\n\n#[test]\nfn broken() { panic!(\"always\"); }\n";
    let run = run(&[("src/lib.rs", lib)], &["--tier", "schema"]);
    assert_eq!(run.code, Some(2), "{}", run.report);
    assert_eq!(run.report["error"], true, "{}", run.report);
    assert_eq!(
        run.report["code"], "CARGO_FALLOW_MUTATION_FAILED",
        "{}",
        run.report
    );
}

#[test]
fn a_package_with_no_test_has_no_coverage() {
    let run = run(
        &[("src/lib.rs", "pub fn f(x: u8) -> bool { x == 1 }\n")],
        &["--tier", "schema"],
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let mutant = run.find("!=");
    assert_eq!(mutant["status"], "no_coverage", "{}", run.report);
    assert!(run.report["summary"]["score"].is_null(), "{}", run.report);
}

#[test]
fn a_package_that_does_not_build_stops_the_run() {
    let lib = "pub fn f(x: u8) -> u8 { if x == 1 { \"x\" } else { 0 } }\n";
    let run = run(&[("src/lib.rs", lib)], &[]);
    assert_eq!(run.code, Some(2), "{}", run.report);
    assert_eq!(
        run.report["code"], "CARGO_FALLOW_MUTATION_FAILED",
        "{}",
        run.report
    );
}

#[test]
fn a_doctest_can_kill_a_mutant() {
    let lib = "/// Doc.\n///\n/// ```\n/// assert!(fixture::f(1));\n/// assert!(!fixture::f(2));\n/// ```\npub fn f(x: u8) -> bool {\n    x == 1\n}\n\n#[test]\nfn other() {}\n";
    let run = run(&[("src/lib.rs", lib)], &["--tier", "schema"]);
    let report = &run.report;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let mutant = run.find("!=");
    assert_eq!(mutant["status"], "killed", "{report}");
    assert!(
        mutant["killed_by"]
            .as_str()
            .is_some_and(|name| name.starts_with("src/lib.rs - f")),
        "{report}"
    );
}

#[test]
fn doctests_can_stay_out() {
    let lib = "/// Doc.\n///\n/// ```\n/// assert!(fixture::f(1));\n/// assert!(!fixture::f(2));\n/// ```\npub fn f(x: u8) -> bool {\n    x == 1\n}\n\n#[test]\nfn other() {}\n";
    let run = run(
        &[("src/lib.rs", lib)],
        &["--tier", "schema", "--no-doctests"],
    );
    assert_eq!(run.find("!=")["status"], "survived", "{}", run.report);
}

#[test]
fn a_mutant_that_passes_then_fails_is_flaky() {
    // The test passes in the first run with a mutant and fails in the second. It
    // reads the variable only to stay stable in the baseline runs.
    let lib = r#"
/// Has a test.
pub fn f(x: u8) -> bool {
    x == 1 || x == 1
}

#[cfg(test)]
mod tests {
    #[test]
    fn state() {
        assert!(super::f(1));
        if std::env::var("FALLOW_MUTANT").is_err() {
            return;
        }
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/seen");
        assert!(!std::path::Path::new(path).exists());
        std::fs::write(path, "").expect("write");
    }
}
"#;
    let run = run(
        &[("src/lib.rs", lib)],
        &["--tier", "schema", "--max-mutants", "1"],
    );
    let report = &run.report;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(report["summary"]["flaky"], 1, "{report}");
    assert_eq!(
        report["summary"]["pending"].as_u64().map(|n| n > 0),
        Some(true),
        "{report}"
    );
}

#[test]
fn no_run_compiles_and_reports_pending_mutants() {
    let run = run(&[("src/lib.rs", CHECK)], &["--no-run"]);
    let report = &run.report;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(
        run.mutants().iter().all(|m| m["status"] == "pending"),
        "{report}"
    );
    assert!(
        run.mutants().iter().any(|m| m["tier"] == "rebuild"),
        "{report}"
    );
    assert_eq!(report["schema"]["status"], "compiled", "{report}");
}

#[test]
fn a_time_budget_leaves_the_rest_pending() {
    let run = run(
        &[("src/lib.rs", CHECK)],
        &["--tier", "schema", "--time-budget", "0"],
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.report["summary"]["pending"], 2, "{}", run.report);
    assert_eq!(run.report["stopped_by"], "time_budget", "{}", run.report);
}

#[test]
fn an_invalid_mutant_id_aborts_the_test_process() {
    let run = run(&[("src/lib.rs", CHECK)], &["--tier", "schema", "--no-run"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let schema = run.work.path().join("run/schema");
    let target = run.work.path().join("run/target-schema");
    let test = |value: &str| {
        Process::new("cargo")
            .args(["test", "--lib", "--quiet", "--", "--nocapture"])
            .current_dir(&schema)
            .env("MBX_DISABLE", "1")
            .env("CARGO_TARGET_DIR", &target)
            .env("FALLOW_MUTANT", value)
            .output()
            .expect("cargo runs")
    };
    for value in ["not-hex", "0xfa60e87874a36367", ""] {
        let output = test(value);
        assert!(!output.status.success(), "{value}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("FALLOW_MUTANT is not a hexadecimal"),
            "{value}: {stderr}"
        );
    }
    assert!(test("0").status.success(), "id 0 is no mutant");
}

#[test]
fn the_memory_guard_can_be_switched_off_only_by_a_flag() {
    let run = run(
        &[("src/lib.rs", CHECK)],
        &["--tier", "schema", "--no-memory-guard"],
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.report["guard"]["enabled"], false, "{}", run.report);
    assert_eq!(run.report["summary"]["killed"], 1, "{}", run.report);
}

#[test]
fn human_output_lists_the_survivors() {
    let _one_run = ONE_RUN.lock().unwrap_or_else(PoisonError::into_inner);
    let source = tempfile::tempdir().expect("create a temporary directory");
    write_crate(source.path(), &[("src/lib.rs", CHECK)]);
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .env("MBX_DISABLE", "1")
        .args([
            "mutation",
            "--tier",
            "schema",
            "--quiet",
            "--jobs",
            "1",
            "--manifest-path",
        ])
        .arg(source.path().join("Cargo.toml"))
        .timeout(Duration::from_secs(280))
        .output()
        .expect("the binary runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("1 killed"), "{stdout}");
    assert!(stdout.contains("1 survived"), "{stdout}");
    assert!(stdout.contains("score 50.0%"), "{stdout}");
    assert!(stdout.contains("src/lib.rs:9"), "{stdout}");
}

#[test]
fn sarif_is_not_a_mutation_format() {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .args(["mutation", "--format", "sarif"])
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(2));
}

/// The test process has no other limit after the death of the parent. Without
/// the watchdog this sleep runs on for minutes.
#[test]
fn the_watchdog_kills_the_tests_when_the_parent_dies() {
    let lib = r#"
/// Has a test.
pub fn f(x: u8) -> bool {
    x == 1
}

#[cfg(test)]
mod tests {
    #[test]
    fn slow() {
        if std::env::var("FALLOW_MUTANT").is_ok() {
            let path = concat!(env!("CARGO_MANIFEST_DIR"), "/started");
            std::fs::write(path, std::process::id().to_string()).expect("write");
            std::thread::sleep(std::time::Duration::from_secs(120));
        }
        assert!(super::f(1));
    }
}
"#;
    let _one_run = ONE_RUN.lock().unwrap_or_else(PoisonError::into_inner);
    let source = tempfile::tempdir().expect("create a temporary directory");
    let work = tempfile::tempdir().expect("create a temporary directory");
    write_crate(source.path(), &[("src/lib.rs", lib)]);
    let binary = assert_cmd::cargo::cargo_bin("cargo-fallow");
    let mut parent = Process::new(binary)
        .args([
            "mutation",
            "--tier",
            "schema",
            "--quiet",
            "--timeout",
            "200",
            "--jobs",
            "1",
            "--manifest-path",
        ])
        .arg(source.path().join("Cargo.toml"))
        .arg("--work-dir")
        .arg(work.path().join("run"))
        .env("MBX_DISABLE", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the binary runs");
    let marker = work.path().join("run/schema/started");
    let start = Instant::now();
    while !marker.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "no test started"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let pid = fs::read_to_string(&marker).expect("read the marker");
    parent.kill().expect("kill the parent");
    parent.wait().expect("wait for the parent");
    let start = Instant::now();
    let alive = |pid: &str| {
        Process::new("ps")
            .args(["-o", "pid=", "-p", pid])
            .output()
            .is_ok_and(|output| !output.stdout.is_empty())
    };
    while alive(&pid) {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "the test process {pid} still runs"
        );
        thread::sleep(Duration::from_millis(100));
    }
}
