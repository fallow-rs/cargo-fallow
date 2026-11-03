//! Black-box tests of the `mutants` command: real cargo, rustc and nextest.
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
        // An outer nextest run names its own profile, and a fixture has no such profile.
        .env_remove("NEXTEST_PROFILE")
        .args([
            "mutants",
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

const MIXED: &str = r#"
pub fn add(a: u32, b: u32) -> u32 {
    a + b
}

pub fn join(a: String, b: &str) -> String {
    a + b
}

pub fn kind(a: u8) -> u8 {
    match a {
        1 => 10,
        _ => 20,
    }
}

pub const fn twice(a: u32) -> u32 {
    a * 2
}

#[cfg(test)]
mod tests {
    #[test]
    fn they_work() {
        assert_eq!(super::add(2, 3), 5);
        assert_eq!(super::join("a".to_owned(), "b"), "ab");
        assert_eq!(super::kind(1), 10);
        assert_eq!(super::kind(2), 20);
        assert_eq!(super::twice(2), 4);
    }
}
"#;

/// A run of both tiers puts the operators that are not type-safe by construction
/// into the schema. A mutant that breaks the schema runs as a rebuild, and a mutant
/// in a constant context stays a rebuild.
#[test]
fn rebuild_operators_run_in_the_schema_and_fall_back_to_a_rebuild() {
    let run = run(&[("src/lib.rs", MIXED)], &[]);
    let report = &run.report;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let on = |operator: &str, line: u64| -> Vec<&Value> {
        run.mutants()
            .iter()
            .filter(|m| m["operator"] == operator && m["line"] == line)
            .collect()
    };
    let has = |mutants: &[&Value], tier: &str, status: &str| {
        !mutants.is_empty()
            && mutants
                .iter()
                .all(|m| m["tier"] == tier && m["status"] == status)
    };
    // `u32 - u32` compiles, and the test sees it.
    assert!(
        has(&on("arithmetic-operator", 3), "schema", "killed"),
        "{report}"
    );
    // `String - &str` does not compile, so the mutant leaves the schema.
    let join = on("arithmetic-operator", 7);
    assert!(has(&join, "rebuild", "compile_error"), "{report}");
    assert!(
        join.iter().all(|m| m["removed_from_schema"] == true),
        "{report}"
    );
    // The new guard of the arm acts as a deleted arm.
    assert!(
        has(&on("deleted-match-arm", 12), "schema", "killed"),
        "{report}"
    );
    // A switch cannot run in a `const fn`.
    assert!(
        has(&on("arithmetic-operator", 18), "rebuild", "killed"),
        "{report}"
    );
    for line in [2, 6, 10] {
        assert!(
            has(&on("return-value", line), "schema", "killed"),
            "{report}"
        );
    }
    assert_eq!(report["summary"]["survived"], 0, "{report}");
}

#[test]
fn schema_mutants_are_killed_or_survive() {
    let run = run(&[("src/lib.rs", CHECK)], &["--tier", "schema"]);
    let report = &run.report;
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(report["kind"], "mutants", "{report}");
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
    // Only the cache folder of the run is in the target folder.
    let entries: Vec<_> = fs::read_dir(source.join("target"))
        .expect("the target folder")
        .map(|entry| entry.expect("an entry").file_name())
        .collect();
    assert_eq!(entries, ["fallow-mutation"]);
    assert!(!source.join("Cargo.lock").exists());
}

#[test]
fn no_cache_writes_nothing_next_to_the_source() {
    let run = run(
        &[("src/lib.rs", CHECK)],
        &["--tier", "schema", "--no-cache"],
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(!run.source.path().join("target").exists());
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
        let _ = super::join(String::new());
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
    assert_eq!(arms[1]["status"], "no_coverage", "{report}");
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
/// The limit of the test that uses it leaves room for the baseline: on a CI
/// runner a clean test process can use more than 100 MiB.
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
            "--memory-limit",
            "256",
            "--timeout",
            "60",
        ],
    );
    let report = &run.report;
    assert_eq!(run.code, Some(0), "{}\n{report}", run.stderr);
    let mutant = run.find("<=");
    assert_eq!(mutant["status"], "runtime_error", "{report}");
    let reason = mutant["reason"].as_str().expect("a reason");
    assert!(reason.contains("memory guard"), "{reason}");
    assert!(reason.contains("over the limit of 256 MiB"), "{reason}");
    assert!(report["guard"]["kills"].as_u64() >= Some(1), "{report}");
    assert_eq!(report["guard"]["enabled"], true, "{report}");
    assert_eq!(report["guard"]["memory_limit_mib"], 256, "{report}");
    // The kill comes long before the time limit of 60 s for this mutant.
    assert!(mutant["duration_ms"].as_u64() < Some(60_000), "{report}");
}

/// On 2026-11-01 a mutant without a sleep reached 24 GiB before a kill, because
/// the guard waited for `top`. The unit test
/// `the_rss_thread_stops_a_process_before_a_slow_read_of_top_ends` checks that
/// order. This test checks the full path with a fast mutant.
///
/// The guard reads RSS each 200 ms. Without a sleep, a mutant writes 8 to 20 GB/s,
/// so the RSS at the stop measures the memory bandwidth, not the guard: under
/// load it passed 3 GiB. A sleep of 1 ms for each MiB keeps the rate under about
/// 1 GB/s, five times the rate of `GROW`. Then a stop at the first table over
/// the limit stays under 1 GiB, also under load. The cap on the blocks bounds
/// the memory if the guard is broken.
#[test]
fn the_guard_stops_a_fast_mutant_near_the_limit() {
    let lib = r"
/// Grows and shrinks.
pub fn grow(start: i64) -> usize {
    let mut left = start;
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    while left != 0 && blocks.len() < 6144 {
        blocks.push(vec![1_u8; 1 << 20]);
        std::thread::sleep(std::time::Duration::from_millis(1));
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
        &["--tier", "schema", "--memory-limit", "256"],
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
    assert!(reached < 1536, "{reason}");
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
        &["--tier", "schema", "--memory-limit", "64"],
    );
    assert_eq!(run.code, Some(2), "{}", run.report);
    let message = run.report["message"].as_str().expect("a message");
    assert!(message.contains("over the limit of 64 MiB"), "{message}");
    let help = run.report["help"].as_str().expect("a hint");
    assert!(help.contains("--memory-limit"), "{help}");
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
    // The line of a mutant is the line of its operator in the original file.
    assert_eq!(mutant["line"], 4, "{report}");
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

/// A run with no selection and no doctests, where each test runs for each mutant.
fn run_plain_order(lib: &str) -> Run {
    run(
        &[("src/lib.rs", lib)],
        &["--tier", "schema", "--no-doctests", "--no-selection"],
    )
}

#[test]
fn doctests_can_stay_out() {
    let lib = "/// Doc.\n///\n/// ```\n/// assert!(fixture::f(1));\n/// assert!(!fixture::f(2));\n/// ```\npub fn f(x: u8) -> bool {\n    x == 1\n}\n\n#[test]\nfn other() {}\n";
    let run = run(
        &[("src/lib.rs", lib)],
        &["--tier", "schema", "--no-doctests"],
    );
    let mutant = run.find("!=");
    assert_eq!(mutant["status"], "no_coverage", "{}", run.report);
    assert_eq!(mutant["doctests_run"], 0, "{mutant}");
    let run = run_plain_order(lib);
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
        if !std::path::Path::new("/dev/fd/9").exists() {
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
        run.mutants()
            .iter()
            .any(|m| m["operator"] == "return-value"),
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
fn an_invalid_mutant_record_aborts_the_test_process() {
    let run = run(&[("src/lib.rs", CHECK)], &["--tier", "schema", "--no-run"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let schema = run.work.path().join("run/schema");
    let target = run.work.path().join("run/target-schema");
    let record = run.work.path().join("record");
    let test = |value: &str| {
        fs::write(&record, value).expect("write the record");
        Process::new("sh")
            .args(["-c", "exec 9<\"$1\"; shift; exec \"$@\"", "sh"])
            .arg(&record)
            .args(["cargo", "test", "--lib", "--quiet", "--", "--nocapture"])
            .current_dir(&schema)
            .env("MBX_DISABLE", "1")
            // An outer nextest run names its own profile, and a fixture has no such profile.
            .env_remove("NEXTEST_PROFILE")
            .env("CARGO_TARGET_DIR", &target)
            .output()
            .expect("cargo runs")
    };
    let output = test("fallow-mutant:not-hexadecimal!");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("the record of the active mutant is not valid"),
        "{stderr}"
    );
    assert!(
        test("fallow-mutant:0000000000000000").status.success(),
        "id 0 is no mutant"
    );
    // Another file on this descriptor is no record of the tool.
    assert!(test("another file with 30 bytes!!!!").status.success());
    assert!(test("short").status.success());
}

/// On macOS a `stat` of `/dev/fd/9` can fail with `EBADF` while another process
/// does the same lookup, also when the descriptor is open. The switch must then
/// still see the mutant: each test process of a mutant run reads the record at
/// the same time as the other test processes.
#[test]
fn the_switch_sees_the_mutant_while_other_processes_read_their_record() {
    const READERS: usize = 200;
    const AT_ONCE: usize = 4;
    const NOISE: usize = 4;
    let run = run(&[("src/lib.rs", CHECK)], &["--tier", "schema", "--no-run"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let killed = run
        .mutants()
        .iter()
        .find(|m| m["line"] == 4 && m["operator"] == "relational-operator")
        .and_then(|m| m["id"].as_str())
        .expect("the mutant that the test kills");
    let record = run.work.path().join("record");
    fs::write(&record, format!("fallow-mutant:{killed}")).expect("write the record");
    let deps = run.work.path().join("run/target-schema/debug/deps");
    let binary = fs::read_dir(&deps)
        .expect("the schema has test binaries")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("fixture-") && !name.contains('.'))
        })
        .expect("the test binary of the fixture");
    let with_record = |program: &[&str]| {
        let mut command = Process::new("sh");
        command
            .args(["-c", "exec 9<\"$1\"; shift; exec \"$@\"", "sh"])
            .arg(&record)
            .args(program)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command
    };
    // Each of these processes does the same lookup in a loop. The guard stops
    // them also when the test panics.
    struct Noise(Vec<std::process::Child>);
    impl Drop for Noise {
        fn drop(&mut self) {
            for process in &mut self.0 {
                let _ = process.kill();
                let _ = process.wait();
            }
        }
    }
    let _noise = Noise(
        std::iter::repeat_with(|| {
            with_record(&["sh", "-c", "while :; do [ -e /dev/fd/9 ]; done"])
                .stdout(Stdio::null())
                .spawn()
                .expect("start a process that reads its record")
        })
        .take(NOISE)
        .collect(),
    );
    let binary = binary.to_str().expect("a UTF-8 path");
    let mut misses = 0;
    for _ in 0..READERS / AT_ONCE {
        let readers: Vec<_> = std::iter::repeat_with(|| {
            with_record(&[binary, "--exact", "tests::check_works"])
                .spawn()
                .expect("start the test binary")
        })
        .take(AT_ONCE)
        .collect();
        for reader in readers {
            let output = reader.wait_with_output().expect("the test binary ends");
            // Only a failed assertion shows that the process saw the mutant.
            if !String::from_utf8_lossy(&output.stdout)
                .contains("test tests::check_works ... FAILED")
            {
                misses += 1;
            }
        }
    }
    assert_eq!(
        misses, 0,
        "{misses} of {READERS} test processes did not see the mutant"
    );
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
        // An outer nextest run names its own profile, and a fixture has no such profile.
        .env_remove("NEXTEST_PROFILE")
        .args([
            "mutants",
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
    assert!(
        stdout.contains("test selection: 1 tests in the map"),
        "{stdout}"
    );
    assert!(stdout.contains("  src/lib.rs\n    :9:"), "{stdout}");
}

#[test]
fn a_package_name_with_a_dash_gets_its_tests_selected() {
    let source = tempfile::tempdir().expect("create a temporary directory");
    let manifest = MANIFEST.replace("fixture", "my-crate");
    fs::create_dir(source.path().join("src")).expect("create a folder");
    fs::write(source.path().join("Cargo.toml"), manifest).expect("write the manifest");
    fs::write(source.path().join("src/lib.rs"), TWO_FUNCTIONS).expect("write a file");
    let (report, stderr, code) = run_in(
        source.path(),
        &["--tier", "schema", "--no-retry", "--no-cache"],
        None,
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(report["selection"]["tests_mapped"], 2, "{report}");
    for mutant in report["mutants"].as_array().expect("the mutants") {
        if mutant["operator"] == "relational-operator" {
            assert_eq!(mutant["status"], "survived", "{report}");
            assert_eq!(mutant["tests_run"], 1, "{mutant}");
        }
    }
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
        if std::path::Path::new("/dev/fd/9").exists() {
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
            "mutants",
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
        // An outer nextest run names its own profile, and a fixture has no such profile.
        .env_remove("NEXTEST_PROFILE")
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

/// Runs the command in a source folder that the caller keeps, so that a second
/// run can read the cache of the first one.
fn run_in(source: &Path, flags: &[&str], rustc: Option<&Path>) -> (Value, String, Option<i32>) {
    let _one_run = ONE_RUN.lock().unwrap_or_else(PoisonError::into_inner);
    let work = tempfile::tempdir().expect("create a temporary directory");
    let mut command = Command::cargo_bin("cargo-fallow").expect("the binary exists");
    if let Some(rustc) = rustc {
        command.env("RUSTC", rustc);
    }
    let output = command
        .env("MBX_DISABLE", "1")
        // An outer nextest run names its own profile, and a fixture has no such profile.
        .env_remove("NEXTEST_PROFILE")
        .args(["mutants", "--format", "json", "--jobs", "2"])
        .arg("--manifest-path")
        .arg(source.join("Cargo.toml"))
        .arg("--work-dir")
        .arg(work.path().join("run"))
        .args(flags)
        .timeout(Duration::from_secs(280))
        .output()
        .expect("the binary runs");
    (
        serde_json::from_slice(&output.stdout).unwrap_or(Value::Null),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code(),
    )
}

const TWO_FUNCTIONS: &str = r"
pub fn first(a: u8) -> bool {
    a == 1
}

pub fn second(a: u8) -> bool {
    a == 2
}

#[test]
fn first_runs() {
    let _ = first(1);
}

#[test]
fn second_runs() {
    let _ = second(1);
}
";

#[test]
fn a_mutant_runs_only_the_tests_that_execute_its_line() {
    let flags = ["--tier", "schema", "--no-retry"];
    let selected = run(&[("src/lib.rs", TWO_FUNCTIONS)], &flags);
    assert_eq!(selected.code, Some(0), "{}", selected.stderr);
    assert_eq!(selected.report["selection"]["enabled"], true);
    assert_eq!(selected.report["selection"]["map"], "built");
    assert_eq!(selected.report["selection"]["tests_mapped"], 2);
    for mutant in selected.with_operator("relational-operator") {
        assert_eq!(mutant["status"], "survived", "{}", selected.report);
        assert_eq!(mutant["tests_run"], 1, "{mutant}");
    }
    assert_eq!(selected.report["summary"]["no_coverage"], 0);

    let all = run(
        &[("src/lib.rs", TWO_FUNCTIONS)],
        &["--tier", "schema", "--no-retry", "--no-selection"],
    );
    assert_eq!(all.report["selection"]["enabled"], false);
    assert_eq!(all.report["selection"]["map"], "none");
    for mutant in all.with_operator("relational-operator") {
        assert_eq!(mutant["status"], "survived", "{}", all.report);
        assert_eq!(mutant["tests_run"], 2, "{mutant}");
    }
}

#[test]
fn a_mutant_that_no_test_executes_has_no_coverage_and_does_not_run() {
    let lib = "pub fn tested(a: u8) -> bool {\n    a == 1\n}\n\npub fn untested(a: u8) -> bool {\n    a == 2\n}\n\n#[test]\nfn tested_works() {\n    assert!(tested(1));\n}\n";
    let run = run(&[("src/lib.rs", lib)], &["--tier", "schema", "--no-retry"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let mutant = run
        .mutants()
        .iter()
        .find(|mutant| mutant["line"] == 6 && mutant["replacement"] == "!=")
        .expect("the mutant of `untested`");
    assert_eq!(mutant["status"], "no_coverage", "{}", run.report);
    assert_eq!(mutant["tests_run"], 0, "{mutant}");
    assert_eq!(mutant["doctests_run"], 0, "{mutant}");
    assert_eq!(run.find("!=")["status"], "killed", "{}", run.report);
}

/// A build costs time, so a mutant that no test executes must not reach a build.
/// A `compile_error` here shows that the mutant reached a build.
#[test]
fn a_rebuild_mutant_that_no_test_executes_does_not_build() {
    let lib = "pub fn untested(a: u32) -> String {\n    let _ = a;\n    String::new()\n}\n\n#[test]\nfn other() {}\n";
    let run = run(&[("src/lib.rs", lib)], &["--tier", "rebuild", "--no-retry"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(!run.mutants().is_empty(), "{}", run.report);
    for mutant in run.mutants() {
        assert_eq!(mutant["status"], "no_coverage", "{mutant}");
        assert_eq!(mutant["tests_run"], 0, "{mutant}");
    }
}

#[test]
fn the_fastest_test_runs_first() {
    let lib = "pub fn check(a: u8) -> bool {\n    a == 1\n}\n\n#[test]\nfn a_slow() {\n    std::thread::sleep(std::time::Duration::from_millis(400));\n    assert!(check(1));\n}\n\n#[test]\nfn z_fast() {\n    assert!(check(1));\n}\n";
    let flags = ["--tier", "schema", "--no-retry"];
    let selected = run(&[("src/lib.rs", lib)], &flags);
    let mutant = selected.find("!=");
    assert_eq!(mutant["status"], "killed", "{}", selected.report);
    assert_eq!(mutant["killed_by"], "fixture::fixture z_fast", "{mutant}");
    assert_eq!(mutant["tests_run"], 1, "{mutant}");

    // The plain order of nextest starts with the slow test, so the check above
    // shows the effect of the order.
    let plain = run(
        &[("src/lib.rs", lib)],
        &["--tier", "schema", "--no-retry", "--no-selection"],
    );
    let mutant = plain.find("!=");
    assert_eq!(mutant["killed_by"], "fixture::fixture a_slow", "{mutant}");
}

/// With the mutant, the slow test fails and the fast test never ends. The order
/// decides which one the run sees first. Both results count as detected, so the
/// score is the same. The README documents this one difference.
#[test]
fn the_order_decides_between_a_kill_and_a_timeout() {
    let lib = "pub fn check(a: u8) -> bool {\n    a == 1\n}\n\n#[test]\nfn a_fails() {\n    std::thread::sleep(std::time::Duration::from_millis(300));\n    assert!(check(1));\n}\n\n#[test]\nfn z_hangs() {\n    while !check(1) {\n        std::thread::sleep(std::time::Duration::from_millis(50));\n    }\n}\n";
    let flags = ["--tier", "schema", "--no-retry", "--timeout", "3"];
    let selected = run(&[("src/lib.rs", lib)], &flags);
    assert_eq!(
        selected.find("!=")["status"],
        "timeout",
        "{}",
        selected.report
    );
    let plain = run(
        &[("src/lib.rs", lib)],
        &[
            "--tier",
            "schema",
            "--no-retry",
            "--timeout",
            "3",
            "--no-selection",
        ],
    );
    assert_eq!(plain.find("!=")["status"], "killed", "{}", plain.report);
    assert_eq!(
        selected.report["summary"]["score"],
        plain.report["summary"]["score"]
    );
}

/// The compiler evaluates the constant, so no test process executes the body of
/// the `const fn`, and the line has a count of 0. A test can still see the change.
#[test]
fn a_mutant_in_a_const_fn_runs_each_test() {
    let lib = "const fn double(a: u8) -> u8 {\n    a * 2\n}\n\nconst FOUR: u8 = double(2);\n\npub fn four() -> u8 {\n    FOUR\n}\n\n#[test]\nfn four_works() {\n    assert_eq!(four(), 4);\n}\n";
    let run = run(&[("src/lib.rs", lib)], &["--tier", "rebuild", "--no-retry"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let mutant = run.find("/");
    assert_eq!(mutant["status"], "killed", "{}", run.report);
    assert_eq!(mutant["no_line_data"], true, "{mutant}");
}

#[test]
fn a_mutant_in_a_binary_gets_the_coverage_of_its_child_process() {
    let main = "fn main() {\n    let args = std::env::args().count();\n    if args == 1 {\n        print!(\"one\");\n    }\n}\n";
    let test = "#[test]\nfn prints() {\n    let out = std::process::Command::new(env!(\"CARGO_BIN_EXE_fixture\")).output().unwrap();\n    assert_eq!(out.stdout, b\"one\");\n}\n";
    let run = run(
        &[("src/main.rs", main), ("tests/cli.rs", test)],
        &["--tier", "schema", "--no-retry"],
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.find("!=")["status"], "killed", "{}", run.report);
}

/// The process ends through `_exit`, so it writes no profile. The test still
/// passes, so a map that reads this as "reaches nothing" loses a kill.
#[test]
fn a_test_that_writes_no_profile_runs_for_each_mutant() {
    let lib = "pub fn check(a: u8) -> bool {\n    a == 1\n}\n\nextern \"C\" {\n    fn _exit(code: i32) -> !;\n}\n\n#[test]\nfn no_profile() {\n    assert!(check(1));\n    unsafe { _exit(0) }\n}\n";
    let run = run(&[("src/lib.rs", lib)], &["--tier", "schema", "--no-retry"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.find("!=")["status"], "killed", "{}", run.report);
    assert_eq!(run.report["selection"]["tests_mapped"], 0, "{}", run.report);
    assert_eq!(
        run.report["selection"]["tests_unmapped"],
        serde_json::json!(["fixture::fixture$no_profile"]),
        "{}",
        run.report
    );
}

#[test]
fn a_doctest_still_decides_a_mutant_with_no_test_coverage() {
    let lib = "/// Doc.\n///\n/// ```\n/// assert!(fixture::f(1));\n/// assert!(!fixture::f(2));\n/// ```\npub fn f(x: u8) -> bool {\n    x == 1\n}\n\n/// Doc.\n///\n/// ```\n/// let _ = fixture::g(1);\n/// ```\npub fn g(x: u8) -> bool {\n    x == 3\n}\n\n#[test]\nfn other() {}\n";
    let run = run(&[("src/lib.rs", lib)], &["--tier", "schema"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let killed = run
        .mutants()
        .iter()
        .find(|mutant| mutant["line"] == 8 && mutant["replacement"] == "!=")
        .expect("the mutant of `f`");
    assert_eq!(killed["status"], "killed", "{}", run.report);
    assert_eq!(killed["tests_run"], 0, "{killed}");
    assert!(killed["doctests_run"].as_u64() > Some(0), "{killed}");
    let unseen = run
        .mutants()
        .iter()
        .find(|mutant| mutant["line"] == 17 && mutant["replacement"] == "!=")
        .expect("the mutant of `g`");
    assert_eq!(unseen["status"], "no_coverage", "{}", run.report);
    assert!(
        unseen["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("doctests")),
        "{unseen}"
    );
    assert!(unseen["doctests_run"].as_u64() > Some(0), "{unseen}");
}

#[test]
fn a_second_run_reads_the_map_until_a_source_changes() {
    let source = tempfile::tempdir().expect("create a temporary directory");
    write_crate(source.path(), &[("src/lib.rs", TWO_FUNCTIONS)]);
    let flags = ["--tier", "schema", "--no-retry"];
    let (first, _, _) = run_in(source.path(), &flags, None);
    assert_eq!(first["selection"]["map"], "built", "{first}");
    let (second, _, _) = run_in(source.path(), &flags, None);
    assert_eq!(second["selection"]["map"], "cached", "{second}");
    assert_eq!(second["summary"], first["summary"]);
    let changed =
        format!("{TWO_FUNCTIONS}\n#[test]\nfn third_runs() {{\n    let _ = first(2);\n}}\n");
    fs::write(source.path().join("src/lib.rs"), changed).expect("write the file");
    let (third, _, _) = run_in(source.path(), &flags, None);
    assert_eq!(third["selection"]["map"], "built", "{third}");
    assert_eq!(third["selection"]["tests_mapped"], 3, "{third}");
    let (uncached, _, _) = run_in(
        source.path(),
        &["--tier", "schema", "--no-retry", "--no-cache"],
        None,
    );
    assert_eq!(uncached["selection"]["map"], "built", "{uncached}");
}

#[test]
fn a_stopped_run_resumes_from_its_journal() {
    let source = tempfile::tempdir().expect("create a temporary directory");
    write_crate(source.path(), &[("src/lib.rs", TWO_FUNCTIONS)]);
    let flags = ["--tier", "schema", "--no-retry"];
    let (first, _, _) = run_in(
        source.path(),
        &["--tier", "schema", "--no-retry", "--max-mutants", "1"],
        None,
    );
    assert_eq!(
        first["summary"]["pending"].as_u64(),
        first["summary"]["total"].as_u64().map(|n| n - 1),
        "{first}"
    );
    assert_eq!(first["selection"]["resumed"], 0, "{first}");
    let (second, _, _) = run_in(source.path(), &flags, None);
    assert_eq!(second["summary"]["pending"], 0, "{second}");
    assert_eq!(second["selection"]["resumed"], 1, "{second}");
    // Each mutant has a result now, so the journal is gone.
    let (third, _, _) = run_in(source.path(), &flags, None);
    assert_eq!(third["selection"]["resumed"], 0, "{third}");
}

#[test]
fn another_option_does_not_resume_a_journal() {
    let source = tempfile::tempdir().expect("create a temporary directory");
    write_crate(source.path(), &[("src/lib.rs", TWO_FUNCTIONS)]);
    let _ = run_in(
        source.path(),
        &["--tier", "schema", "--no-retry", "--max-mutants", "1"],
        None,
    );
    let (other, _, _) = run_in(source.path(), &["--tier", "schema"], None);
    assert_eq!(other["selection"]["resumed"], 0, "{other}");
    assert_eq!(other["summary"]["pending"], 0, "{other}");
}

#[test]
fn missing_llvm_tools_stop_the_run_with_a_clear_error() {
    let source = tempfile::tempdir().expect("create a temporary directory");
    write_crate(source.path(), &[("src/lib.rs", TWO_FUNCTIONS)]);
    let empty = source.path().join("empty-sysroot");
    fs::create_dir(&empty).expect("create a folder");
    let rustc = source.path().join("fake-rustc");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = --print ] && [ \"$2\" = sysroot ]; then\n  echo {}\n  exit 0\nfi\nexec rustc \"$@\"\n",
        empty.display()
    );
    fs::write(&rustc, script).expect("write the script");
    let chmod = Process::new("chmod")
        .arg("+x")
        .arg(&rustc)
        .status()
        .expect("chmod runs");
    assert!(chmod.success());
    let (report, stderr, code) = run_in(source.path(), &["--tier", "schema"], Some(&rustc));
    assert_eq!(code, Some(2), "{report} {stderr}");
    let message = report["message"].as_str().unwrap_or_default();
    assert!(message.contains("llvm-profdata"), "{report}");
    let help = report["help"].as_str().unwrap_or_default();
    assert!(help.contains("rustup component add llvm-tools"), "{report}");
    assert!(help.contains("--no-selection"), "{report}");
    let (report, _, code) = run_in(
        source.path(),
        &["--tier", "schema", "--no-selection"],
        Some(&rustc),
    );
    assert_eq!(code, Some(0), "{report}");
}
