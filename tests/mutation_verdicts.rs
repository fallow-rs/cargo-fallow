//! Black-box tests of the verdict of a mutant: real cargo, rustc and nextest.
//!
//! Each fixture has a test that a broken engine reads wrong. The memory guard has
//! no probe on Windows, so the file is for Unix only.
#![cfg(unix)]
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

const MANIFEST: &str = "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\npublish = false\n\n[workspace]\n";

fn write_files(dir: &Path, files: &[(&str, &str)]) {
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
    _source: TempDir,
    _work: TempDir,
}

impl Run {
    fn mutants(&self) -> &Vec<Value> {
        self.report["mutants"].as_array().expect("the mutants")
    }

    /// The status of each mutant, sorted.
    fn statuses(&self) -> Vec<String> {
        let mut statuses: Vec<String> = self
            .mutants()
            .iter()
            .map(|mutant| mutant["status"].as_str().expect("a status").to_owned())
            .collect();
        statuses.sort();
        statuses
    }

    fn assert_all(&self, status: &str) {
        assert!(!self.mutants().is_empty(), "{}", self.report);
        assert!(
            self.mutants().iter().all(|m| m["status"] == status),
            "{}\n{}",
            self.report["mutants"],
            self.stderr
        );
    }
}

/// One run at a time: each run has up to two test processes.
static ONE_RUN: Mutex<()> = Mutex::new(());

/// Runs the command in `folder` of a new tree that has `files`.
fn run_in(folder: &str, files: &[(&str, &str)], flags: &[&str]) -> Run {
    let _one_run = ONE_RUN.lock().unwrap_or_else(PoisonError::into_inner);
    let source = tempfile::tempdir().expect("create a temporary directory");
    let work = tempfile::tempdir().expect("create a temporary directory");
    write_files(source.path(), files);
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .env("MBX_DISABLE", "1")
        // An outer nextest run names its own profile, and a fixture has no such profile.
        .env_remove("NEXTEST_PROFILE")
        .current_dir(source.path().join(folder))
        .args(["mutants", "--no-cache", "--format", "json", "--work-dir"])
        .arg(work.path().join("run"))
        .args(flags)
        .timeout(Duration::from_secs(280))
        .output()
        .expect("the binary runs");
    Run {
        report: serde_json::from_slice(&output.stdout).unwrap_or(Value::Null),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        code: output.status.code(),
        _source: source,
        _work: work,
    }
}

/// Runs the command on a crate with the manifest of one package.
fn run(files: &[(&str, &str)], flags: &[&str]) -> Run {
    let mut all = vec![("Cargo.toml", MANIFEST)];
    all.extend_from_slice(files);
    run_in(".", &all, flags)
}

/// A switch that allocates fails a test that counts allocations.
#[test]
fn a_test_that_counts_allocations_kills_no_schema_mutant() {
    let lib = r"
pub fn fast(a: u8) -> u8 {
    let mut n = 0;
    if a > 10 { n = 1; }
    if a != 5 { n += 1; }
    n
}
#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    struct C;
    unsafe impl GlobalAlloc for C {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            COUNT.fetch_add(1, Ordering::SeqCst);
            unsafe { System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) }
        }
    }
    #[global_allocator]
    static A: C = C;
    #[test]
    fn fast_does_not_allocate() {
        let before = COUNT.load(Ordering::SeqCst);
        let _ = super::fast(5);
        assert_eq!(COUNT.load(Ordering::SeqCst), before);
    }
}
";
    let run = run(
        &[("src/lib.rs", lib)],
        &[
            "--tier",
            "schema",
            "--no-selection",
            "--no-retry",
            "-j",
            "1",
        ],
    );
    run.assert_all("survived");
}

/// A site in a global allocator runs while the switch reads its value. A switch
/// that allocates in that read hangs each test.
#[test]
fn a_site_in_a_global_allocator_does_not_hang_the_tests() {
    let lib = r"
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
pub struct Counting;
pub static BIG: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if l.size() > 4096 {
            BIG.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}
#[global_allocator]
static A: Counting = Counting;
pub fn no_test_asserts_this(a: u8) -> bool {
    a == 1
}
#[test]
fn big_counts() {
    let _ = no_test_asserts_this(1);
    let before = BIG.load(Ordering::Relaxed);
    let _v = vec![0u8; 8192];
    assert!(BIG.load(Ordering::Relaxed) > before);
}
";
    let run = run(
        &[("src/lib.rs", lib)],
        &[
            "--tier",
            "schema",
            "--no-selection",
            "--no-retry",
            "--timeout",
            "20",
            "-j",
            "1",
        ],
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(
        !run.statuses().iter().any(|status| status == "timeout"),
        "{}",
        run.report["mutants"]
    );
    let of = |replacement: &str| {
        run.mutants()
            .iter()
            .find(|mutant| mutant["replacement"] == replacement)
            .map(|mutant| mutant["status"].clone())
    };
    assert_eq!(of("!=").expect("the mutant of `a == 1`"), "survived");
}

/// A child process with an empty environment still selects the mutant.
#[test]
fn a_child_with_an_empty_environment_runs_the_mutant() {
    let main = r#"
fn main() {
    let a: u8 = std::env::args().nth(1).unwrap().parse().unwrap();
    if a == 1 { println!("one"); } else { println!("other"); }
}
"#;
    let test = r#"
use std::process::Command;
#[test]
fn prints() {
    let o = Command::new(env!("CARGO_BIN_EXE_fixture")).arg("1").env_clear().output().unwrap();
    assert_eq!(o.stdout, b"one\n");
}
"#;
    let files = [("src/main.rs", main), ("tests/cli.rs", test)];
    run(&files, &["--tier", "schema", "--no-selection", "-j", "1"]).assert_all("killed");
    run(&files, &["--tier", "schema", "-j", "1"]).assert_all("killed");
}

/// A test that removes each environment variable still runs the mutant.
#[test]
fn a_test_that_clears_the_environment_runs_the_mutant() {
    let lib = r"
pub fn f(a: u8) -> bool {
    a == 1
}
#[test]
fn isolated_env() {
    #[allow(unused_unsafe)]
    for (k, _) in std::env::vars() { unsafe { std::env::remove_var(k) } }
    assert!(f(1));
    assert!(!f(2));
}
";
    let run = run(
        &[("src/lib.rs", lib)],
        &["--tier", "schema", "--no-selection", "-j", "1"],
    );
    run.assert_all("killed");
}

/// `forbid` of a lint must not stop the doctests of the schema build.
#[test]
fn a_forbidden_lint_does_not_stop_the_doctests_of_the_schema() {
    let lib = r"#![forbid(unused_parens)]
/// ```
/// assert!(fixture::f(1));
/// assert!(!fixture::f(2));
/// ```
pub fn f(a: u8) -> bool {
    a == 1
}
#[test]
fn other() {}
";
    let run = run(
        &[("src/lib.rs", lib)],
        &["--tier", "schema", "--no-selection", "-j", "1"],
    );
    run.assert_all("killed");
}

/// A shebang line stays the first line of the schema build.
#[test]
fn a_root_file_with_a_shebang_compiles_as_a_schema() {
    let main = r"#!/usr/bin/env run-cargo-script
fn f(a: u8) -> bool {
    a == 1
}
fn main() { println!(); }
#[test]
fn t() { assert!(f(1)); assert!(!f(2)); }
";
    let run = run(
        &[("src/main.rs", main)],
        &["--tier", "schema", "--no-selection", "-j", "1"],
    );
    assert_eq!(run.report["schema"]["status"], "compiled", "{}", run.report);
    run.assert_all("killed");
}

/// rustc accepts a file with a byte order mark, so the file has mutants.
#[test]
fn a_file_with_a_byte_order_mark_has_mutants() {
    let lib = "\u{feff}pub fn f(a: u8) -> bool {\n    a == 1\n}\n#[test]\nfn t() { assert!(f(1)); assert!(!f(2)); }\n";
    let run = run(
        &[("src/lib.rs", lib)],
        &["--tier", "schema", "--no-selection", "-j", "1"],
    );
    assert_eq!(
        run.report["gaps"]["files_skipped"],
        Value::Array(Vec::new())
    );
    run.assert_all("killed");
}

const DENY_WARNINGS: &str = r"#![deny(warnings)]
pub fn f(a: u8) -> u8 {
    let b = a;
    b.wrapping_add(1)
}
#[test]
fn t() { assert_eq!(f(1), 2); }
";

/// A `rustflags` key under `[target]` takes the place of `build.rustflags`, so
/// the lint cap must reach the compiler in another way.
#[test]
fn the_lint_cap_reaches_the_compiler_with_target_rustflags() {
    let config = "[target.'cfg(all())']\nrustflags = [\"--cfg\", \"myflag\"]\n";
    let run = run(
        &[
            ("src/lib.rs", DENY_WARNINGS),
            (".cargo/config.toml", config),
        ],
        &["--tier", "rebuild", "--no-selection", "-j", "1"],
    );
    let returns: Vec<&Value> = run
        .mutants()
        .iter()
        .filter(|mutant| mutant["operator"] == "return-value")
        .collect();
    assert!(!returns.is_empty(), "{}", run.report);
    assert!(
        returns.iter().all(|mutant| mutant["status"] == "killed"),
        "{}",
        run.report["mutants"]
    );
}

/// A test that fails with no mutant says nothing about a test with the same name
/// in another binary.
#[test]
fn a_known_failure_does_not_hide_a_test_with_the_same_name() {
    let run = run(
        &[
            ("src/lib.rs", "pub fn f(a: u8) -> bool {\n    a == 1\n}\n"),
            (
                "tests/a.rs",
                "#[test]\nfn check() { panic!(\"fails with no mutant\"); }\n",
            ),
            (
                "tests/b.rs",
                "#[test]\nfn check() { assert!(fixture::f(1)); assert!(!fixture::f(2)); }\n",
            ),
        ],
        &["--tier", "schema", "-j", "1"],
    );
    run.assert_all("killed");
    assert_eq!(run.mutants()[0]["killed_by"], "fixture::b check");
}

/// A doctest name has no line number, so the count of failures tells two doctests
/// of one item apart.
#[test]
fn a_known_doctest_failure_does_not_hide_a_new_failure_of_the_same_item() {
    let lib = r#"
/// ```
/// panic!("fails with no mutant");
/// ```
///
/// ```
/// assert!(fixture::f(1));
/// assert!(!fixture::f(2));
/// ```
pub fn f(a: u8) -> bool {
    a == 1
}
#[test]
fn other() {}
"#;
    let run = run(
        &[("src/lib.rs", lib)],
        &["--tier", "schema", "--no-selection", "-j", "1"],
    );
    run.assert_all("killed");
}

/// The nextest config of the project can end a test. No assert failed, so the
/// status says that the test did not end.
#[test]
fn a_test_that_the_nextest_config_ends_is_a_timeout() {
    let lib = r"
pub fn wait(a: u8) {
    while a != 1 {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
#[test]
fn hangs_under_mutant() { wait(1); }
";
    let config = "[profile.default]\nslow-timeout = { period = \"1s\", terminate-after = 2 }\n";
    let run = run(
        &[("src/lib.rs", lib), (".config/nextest.toml", config)],
        &[
            "--tier",
            "schema",
            "--no-selection",
            "--no-retry",
            "-j",
            "1",
        ],
    );
    run.assert_all("timeout");
    assert_eq!(run.mutants()[0]["killed_by"], Value::Null);
    assert_eq!(run.mutants()[0]["tests_run"], 1, "{}", run.report);
}

/// A run over the limit of the tool has run a test.
#[test]
fn a_mutant_over_the_time_limit_reports_the_test_that_ran() {
    let lib = r"
pub fn wait(a: u8) {
    while a != 1 {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
#[test]
fn hangs_under_mutant() { wait(1); }
";
    let run = run(
        &[("src/lib.rs", lib)],
        &[
            "--tier",
            "schema",
            "--no-selection",
            "--no-retry",
            "--timeout",
            "3",
            "-j",
            "1",
        ],
    );
    run.assert_all("timeout");
    assert_eq!(run.mutants()[0]["tests_run"], 1, "{}", run.report);
}

const PORT: &str = r#"
pub fn sum(a: u8) -> u8 {
    let mut n = 0;
    if a > 10 { n = 1; }
    if a < 5 { n = 2; }
    if a == 7 { n = 3; }
    if a != 8 { n = 4; }
    n
}
#[test]
fn uses_fixed_port() {
    let _l = std::net::TcpListener::bind("127.0.0.1:47123").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(700));
    let _ = sum(6);
}
"#;

/// Two workers can run one port test at the same time. The failure comes from the
/// overlap, so the mutant is no kill.
#[test]
fn a_failure_from_two_workers_is_no_kill() {
    let run = run(
        &[("src/lib.rs", PORT)],
        &["--tier", "schema", "-j", "2", "--no-selection"],
    );
    run.assert_all("survived");
}

/// The jobs flag of a run overrides `test-threads = 1` of the project.
#[test]
fn a_failure_from_parallel_tests_in_a_rebuild_is_no_kill() {
    let lib = r#"
pub fn add(a: u8) -> u8 {
    a + 1
}
fn port_user() {
    let _l = std::net::TcpListener::bind("127.0.0.1:47124").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(900));
    let _ = add(1);
}
#[test]
fn t1() { port_user(); }
#[test]
fn t2() { port_user(); }
"#;
    let config = "[profile.default]\ntest-threads = 1\n";
    let run = run(
        &[("src/lib.rs", lib), (".config/nextest.toml", config)],
        &["--tier", "rebuild", "-j", "2"],
    );
    run.assert_all("survived");
}

/// One failure with no link to the mutant is no kill.
#[test]
fn one_failure_that_does_not_repeat_is_flaky() {
    let lib = r#"
pub fn f(a: u8) -> u8 {
    if a > 10 { 1 } else { 0 }
}
#[test]
fn fails_once() {
    let _ = f(6);
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/runs");
    let runs: u32 = std::fs::read_to_string(path).ok().and_then(|t| t.parse().ok()).unwrap_or(0) + 1;
    std::fs::write(path, runs.to_string()).unwrap();
    // Each copy has its own count. In the schema copy, run 1 is the baseline and run
    // 2 is the first mutant.
    assert!(runs != 2);
}
"#;
    let run = run(
        &[("src/lib.rs", lib)],
        &[
            "--tier",
            "schema",
            "--no-selection",
            "-j",
            "1",
            "--max-mutants",
            "1",
        ],
    );
    let first = &run.mutants()[0];
    assert_eq!(first["status"], "flaky", "{}", run.report);
    assert_eq!(first["retried"], true, "{}", run.report);
}

/// A real kill stays a kill, also with a second run.
#[test]
fn a_kill_that_repeats_stays_a_kill_with_the_same_test() {
    let lib = "pub fn f(a: u8) -> bool {\n    a == 1\n}\n#[test]\nfn fast() { let _ = f(1); }\n#[test]\nfn t() { assert!(f(1)); assert!(!f(2)); }\n";
    for flags in [
        &["--tier", "schema", "-j", "2"][..],
        &["--tier", "schema", "-j", "2", "--no-selection"][..],
    ] {
        let run = run(&[("src/lib.rs", lib)], flags);
        run.assert_all("killed");
        assert_eq!(run.mutants()[0]["killed_by"], "fixture::fixture t");
    }
}

/// `--no-retry` keeps the first result of a kill.
#[test]
fn no_retry_gives_a_kill_no_second_run() {
    let lib = "pub fn f(a: u8) -> bool {\n    a == 1\n}\n#[test]\nfn t() { assert!(!f(2)); }\n";
    let run = run(&[("src/lib.rs", lib)], &["--tier", "schema", "--no-retry"]);
    run.assert_all("killed");
    assert_eq!(run.mutants()[0]["retried"], false);
}

/// A test that needs an earlier test fails in the coverage map, where it runs
/// alone. Such a test says nothing about a mutant in a run of selected tests.
#[test]
fn a_test_that_needs_another_test_kills_no_mutant() {
    let lib = r#"
pub fn sum(a: u8) -> u8 {
    if a > 10 { 1 } else { 0 }
}
#[test]
fn a_setup() {
    let _ = sum(6);
    std::thread::sleep(std::time::Duration::from_millis(400));
    std::fs::write("state.tmp", "x").unwrap();
}
#[test]
fn b_use() {
    let _ = sum(6);
    std::fs::read_to_string("state.tmp").unwrap();
    std::fs::remove_file("state.tmp").unwrap();
}
"#;
    let config = "[profile.default]\ntest-threads = 1\n";
    let files = [("src/lib.rs", lib), (".config/nextest.toml", config)];
    for flags in [
        &["--tier", "schema", "-j", "1"][..],
        &["--tier", "schema", "-j", "1", "--no-selection"][..],
    ] {
        let run = run(&files, flags);
        run.assert_all("survived");
    }
}

const DROP_ORDER: &str = r#"
use std::cell::RefCell;
pub struct G<'a>(pub &'a RefCell<Vec<&'static str>>);
impl Drop for G<'_> {
    fn drop(&mut self) {
        self.0.borrow_mut().push("drop");
    }
}
impl G<'_> {
    pub fn v(&self) -> u8 {
        1
    }
}
pub fn order(log: &RefCell<Vec<&'static str>>, x: u8) -> bool {
    let r = (G(log).v() == x, log.borrow_mut().push("after")).0;
    r
}
#[test]
fn order_kept() {
    let l = RefCell::new(vec![]);
    assert!(order(&l, 1));
    assert_eq!(*l.borrow(), ["after", "drop"]);
}
"#;

/// The switch moves the end of a temporary, so the test fails in the schema build.
/// The mutant then needs a rebuild, and the report says so.
#[test]
fn a_test_that_the_schema_breaks_still_judges_its_mutants() {
    for flags in [
        &["--tier", "schema"][..],
        &["--tier", "schema", "--no-selection"][..],
    ] {
        let run = run(&[("src/lib.rs", DROP_ORDER)], flags);
        assert_eq!(run.code, Some(0), "{}", run.stderr);
        run.assert_all("killed");
        assert_eq!(run.mutants()[0]["tier"], "rebuild");
        assert_eq!(
            run.report["schema"]["moved_to_rebuild"], 1,
            "{}",
            run.report
        );
    }
}

/// `column!()` on the line of a site gives another value in the schema build.
#[test]
fn a_test_that_reads_a_column_still_judges_its_mutants() {
    let lib = "pub fn at(a: u8) -> (bool, u32) { (a == 1, column!()) }\n#[test]\nfn column_kept() { assert_eq!(at(1), (true, 44)); assert_eq!(at(2), (false, 44)); }\n";
    let run = run(&[("src/lib.rs", lib)], &["--tier", "schema"]);
    run.assert_all("killed");
}

/// A mutant in a `const fn` can loop in the compiler, because the lint cap turns
/// off the lint that stops a long evaluation.
#[test]
fn a_mutant_that_hangs_the_compiler_is_a_compile_error() {
    let lib = r"
pub const fn up(mut i: i64) -> i64 {
    while i < 10 {
        i += 1;
    }
    i
}
pub const X: i64 = up(0);
#[test]
fn t() { assert_eq!(X, 10); }
";
    let run = run(
        &[("src/lib.rs", lib)],
        &["--tier", "rebuild", "--no-selection"],
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let hang = run
        .mutants()
        .iter()
        .find(|mutant| mutant["replacement"] == "-=")
        .expect("the mutant of `+=`");
    assert_eq!(hang["status"], "compile_error", "{}", run.report);
    assert!(
        hang["reason"]
            .as_str()
            .is_some_and(|text| text.contains("time limit")),
        "{}",
        run.report
    );
}

/// A root package with `default-members` is not in the default set of cargo.
#[test]
fn a_root_package_outside_the_default_members_runs_its_tests() {
    let root = "[package]\nname = \"rootp\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\nmembers = [\"a\"]\ndefault-members = [\"a\"]\n";
    let member = "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
    let lib = "pub fn f(a: u8) -> bool {\n    a == 1\n}\n#[test]\nfn t() { assert!(f(1)); assert!(!f(2)); }\n";
    let files = [
        ("Cargo.toml", root),
        ("a/Cargo.toml", member),
        ("a/src/lib.rs", "#[test]\nfn other() {}\n"),
        ("src/lib.rs", lib),
    ];
    for flags in [
        &["-p", "rootp", "--tier", "schema"][..],
        &["-p", "rootp", "--tier", "schema", "--no-selection"][..],
    ] {
        let run = run_in(".", &files, flags);
        run.assert_all("killed");
    }
}

const WORKSPACE: [(&str, &str); 6] = [
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"core\", \"app\"]\nresolver = \"2\"\n",
    ),
    (
        "core/Cargo.toml",
        "[package]\nname = \"core-x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ),
    (
        "app/Cargo.toml",
        "[package]\nname = \"app-x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\ncore-x = { path = \"../core\" }\n",
    ),
    (
        "core/src/lib.rs",
        "pub fn f(a: u8) -> bool {\n    a == 1\n}\n#[test]\nfn unrelated() {}\n",
    ),
    (
        "app/src/lib.rs",
        "pub fn g(a: u8) -> bool { core_x::f(a) }\n",
    ),
    (
        "app/tests/it.rs",
        "#[test]\nfn t() { assert!(app_x::g(1)); assert!(!app_x::g(2)); }\n",
    ),
];

/// The run says which members depend on the package and do not run their tests.
#[test]
fn the_report_names_the_members_whose_tests_do_not_run() {
    let flags = ["-p", "core-x", "--tier", "schema", "--no-selection"];
    let run = run_in(".", &WORKSPACE, &flags);
    run.assert_all("survived");
    assert_eq!(
        run.report["gaps"]["untested_dependents"],
        serde_json::json!(["app-x"]),
        "{}",
        run.report
    );
}

/// `--test-workspace` runs the tests of each member against a survivor, also against
/// a rebuild mutant, and a mutant that a test of the package kills needs no such run.
#[test]
fn test_workspace_runs_the_tests_of_the_other_members() {
    for flags in [
        &["-p", "core-x", "--test-workspace", "--tier", "schema"][..],
        &[
            "-p",
            "core-x",
            "--test-workspace",
            "--tier",
            "schema",
            "--no-selection",
        ][..],
    ] {
        let run = run_in(".", &WORKSPACE, flags);
        run.assert_all("killed");
        assert_eq!(run.mutants()[0]["killed_by"], "app-x::it t");
        assert_eq!(
            run.report["gaps"]["untested_dependents"],
            Value::Array(Vec::new())
        );
        assert_eq!(run.report["workspace_tests"]["mutants_run"], 1);
        assert_eq!(run.report["workspace_tests"]["detected"], 1);
    }
    let run = run_in(".", &WORKSPACE, &["-p", "core-x", "--test-workspace"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert!(
        run.mutants().iter().any(|m| m["tier"] == "rebuild"),
        "{}",
        run.report
    );
    run.assert_all("killed");
}

/// A mutant that no test in the workspace detects is a survivor, also when no test of
/// the package executes it.
#[test]
fn test_workspace_leaves_a_real_survivor() {
    let mut files = WORKSPACE;
    files[5] = (
        "app/tests/it.rs",
        "#[test]\nfn t() { let _ = app_x::g(1); }\n",
    );
    let run = run_in(
        ".",
        &files,
        &["-p", "core-x", "--test-workspace", "--tier", "schema"],
    );
    run.assert_all("survived");
    assert_eq!(run.report["workspace_tests"]["mutants_run"], 1);
    assert_eq!(run.report["workspace_tests"]["detected"], 0);
    // A pass of the whole workspace gets no second run.
    assert_eq!(run.mutants()[0]["retried"], false);
}

/// A test of the package kills the mutant, so the workspace tests do not run.
#[test]
fn test_workspace_skips_a_detected_mutant() {
    let mut files = WORKSPACE;
    files[3] = (
        "core/src/lib.rs",
        "pub fn f(a: u8) -> bool {\n    a == 1\n}\n#[test]\nfn t() { assert!(f(1)); assert!(!f(2)); }\n",
    );
    let run = run_in(
        ".",
        &files,
        &["-p", "core-x", "--test-workspace", "--tier", "schema"],
    );
    run.assert_all("killed");
    assert_eq!(run.report["workspace_tests"]["mutants_run"], 0);
}

/// A Cargo config above the workspace root changes the build, so the copy needs it.
#[test]
fn a_cargo_config_above_the_workspace_reaches_the_copy() {
    let manifest = "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n[lints.rust]\nunexpected_cfgs = { level = \"allow\" }\n";
    let lib = r#"
pub fn f(a: u8) -> bool {
    a == 1
}
#[cfg(extra)]
pub fn only_with_extra(a: u8) -> bool {
    a == 2
}
#[test]
fn env_guarded() {
    if option_env!("FEATURE_OFF").is_some() || std::env::var("FEATURE_ON").is_err() {
        return;
    }
    assert!(f(1));
    assert!(!f(2));
}
#[cfg(extra)]
#[test]
fn extra_test() {
    assert!(only_with_extra(2));
    assert!(!only_with_extra(3));
}
"#;
    let upper = "[env]\nFEATURE_ON = \"upper\"\n[build]\nrustflags = [\"--cfg\", \"from_upper\"]\n";
    let middle = "[build]\nrustflags = [\"--cfg\", \"extra\"]\n";
    let run = run_in(
        "repo/rust",
        &[
            (".cargo/config.toml", upper),
            ("repo/.cargo/config.toml", middle),
            ("repo/rust/Cargo.toml", manifest),
            ("repo/rust/src/lib.rs", lib),
        ],
        &["--tier", "schema"],
    );
    run.assert_all("killed");
}

/// A test can leave a child that runs on. No tree holds that child after the end of
/// the test, so the runner must stop it.
#[test]
fn a_child_that_outlives_its_test_does_not_outlive_the_run() {
    let lib = r#"
pub fn f(a: u8) -> bool {
    a == 1
}
#[test]
fn leaves_a_child() {
    let _ = f(1);
    if std::env::var("ORPHAN").is_ok() {
        std::thread::sleep(std::time::Duration::from_secs(300));
        return;
    }
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["leaves_a_child", "orphan-marker-48213"])
        .env("ORPHAN", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::mem::forget(child);
}
"#;
    let run = run(
        &[("src/lib.rs", lib)],
        &[
            "--tier",
            "schema",
            "--no-selection",
            "--no-retry",
            "-j",
            "1",
        ],
    );
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let found = std::process::Command::new("pgrep")
        .args(["-f", "orphan-marker-48213"])
        .output()
        .expect("pgrep runs");
    assert!(
        found.stdout.is_empty(),
        "an orphan runs: {}",
        String::from_utf8_lossy(&found.stdout)
    );
}

/// A signal ends the command with no cleanup code, so the watchdog removes the
/// temporary folder.
#[test]
fn a_signal_leaves_no_temporary_folder() {
    let _one_run = ONE_RUN.lock().unwrap_or_else(PoisonError::into_inner);
    let source = tempfile::tempdir().expect("create a temporary directory");
    let temp = tempfile::tempdir().expect("create a temporary directory");
    let mut files = vec![("Cargo.toml", MANIFEST)];
    files.push((
        "src/lib.rs",
        "pub fn f(a: u8) -> bool {\n    a == 1\n}\n#[test]\nfn t() { std::thread::sleep(std::time::Duration::from_secs(60)); assert!(f(1)); }\n",
    ));
    write_files(source.path(), &files);
    let mut parent = std::process::Command::new(assert_cmd::cargo::cargo_bin("cargo-fallow"))
        .args(["mutants", "--no-cache", "--quiet", "--tier", "schema"])
        .current_dir(source.path())
        .env("MBX_DISABLE", "1")
        // An outer nextest run names its own profile, and a fixture has no such profile.
        .env_remove("NEXTEST_PROFILE")
        .env("TMPDIR", temp.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the binary runs");
    let folders = || {
        fs::read_dir(temp.path())
            .expect("read the folder")
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("cargo-fallow-mutation-")
            })
            .count()
    };
    let start = std::time::Instant::now();
    while folders() == 0 {
        assert!(start.elapsed() < Duration::from_secs(60), "no work folder");
        std::thread::sleep(Duration::from_millis(50));
    }
    let name = fs::read_dir(temp.path())
        .expect("read the folder")
        .flatten()
        .next()
        .expect("an entry")
        .file_name();
    assert!(
        name.to_string_lossy().len() > "cargo-fallow-mutation-".len() + 4,
        "the name is not random: {name:?}"
    );
    // The run reaches the tests of the baseline.
    std::thread::sleep(Duration::from_secs(5));
    let status = std::process::Command::new("kill")
        .args(["-TERM", &parent.id().to_string()])
        .status()
        .expect("kill runs");
    assert!(status.success());
    parent.wait().expect("wait for the parent");
    let start = std::time::Instant::now();
    while folders() > 0 {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the temporary folder stays"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A limit of 0 marks each mutant as a timeout, so it counts as detected.
#[test]
fn a_time_limit_of_zero_is_a_usage_error() {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .args([
            "mutants",
            "--timeout",
            "0",
            "--manifest-path",
            "/no/such/file",
        ])
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--timeout"), "{stderr}");
}

const TWO_FILES: [(&str, &str); 3] = [
    (
        "src/lib.rs",
        "pub mod other;\npub fn f(a: u8) -> bool {\n    a == 1\n}\n#[test]\nfn t() { assert!(f(1)); }\n",
    ),
    (
        "src/other.rs",
        "pub fn g(a: u8) -> bool {\n    a == 2\n}\n#[test]\nfn u() { assert!(g(2)); }\n",
    ),
    ("Cargo.toml", MANIFEST),
];

/// `--file` keeps the mutants of one file. The root file still holds the helper.
#[test]
fn the_file_flag_limits_the_mutants() {
    let run = run_in(
        ".",
        &TWO_FILES,
        &["--file", "src/other.rs", "--tier", "all"],
    );
    assert!(!run.mutants().is_empty(), "{}", run.report);
    assert!(
        run.mutants().iter().all(|m| m["file"] == "src/other.rs"),
        "{}",
        run.report["mutants"]
    );
    assert!(run.mutants().iter().all(|m| m["status"] != "pending"));
    assert_eq!(run.code, Some(0), "{}", run.stderr);
}

/// A value with no file gives an error, and no clean result.
#[test]
fn a_file_flag_that_matches_nothing_is_an_error() {
    let run = run_in(".", &TWO_FILES, &["--file", "src/none.rs"]);
    assert_eq!(run.code, Some(2), "{}", run.report);
    assert!(
        run.report.to_string().contains("src/none.rs"),
        "{}",
        run.report
    );
}

/// The human report says which facts change the verdicts.
#[test]
fn the_human_report_names_the_gaps() {
    let _one_run = ONE_RUN.lock().unwrap_or_else(PoisonError::into_inner);
    let source = tempfile::tempdir().expect("create a temporary directory");
    let work = tempfile::tempdir().expect("create a temporary directory");
    write_files(source.path(), &WORKSPACE);
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .env("MBX_DISABLE", "1")
        // An outer nextest run names its own profile, and a fixture has no such profile.
        .env_remove("NEXTEST_PROFILE")
        .current_dir(source.path())
        .args([
            "mutants",
            "--no-cache",
            "-p",
            "core-x",
            "--tier",
            "schema",
            "--work-dir",
        ])
        .arg(work.path().join("run"))
        .timeout(Duration::from_secs(280))
        .output()
        .expect("the binary runs");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("app-x"), "{text}");
    assert!(text.contains("--test-workspace"), "{text}");
    let source = tempfile::tempdir().expect("create a temporary directory");
    let work = tempfile::tempdir().expect("create a temporary directory");
    write_files(
        source.path(),
        &[("Cargo.toml", MANIFEST), ("src/lib.rs", DROP_ORDER)],
    );
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .env("MBX_DISABLE", "1")
        // An outer nextest run names its own profile, and a fixture has no such profile.
        .env_remove("NEXTEST_PROFILE")
        .current_dir(source.path())
        .args(["mutants", "--no-cache", "--tier", "schema", "--work-dir"])
        .arg(work.path().join("run"))
        .timeout(Duration::from_secs(280))
        .output()
        .expect("the binary runs");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("order_kept"), "{text}");
    assert!(text.contains("ran as a rebuild"), "{text}");
}
