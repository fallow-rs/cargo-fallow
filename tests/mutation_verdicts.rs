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
    let run = run(
        &[("src/main.rs", main), ("tests/cli.rs", test)],
        &["--tier", "schema", "--no-selection", "-j", "1"],
    );
    run.assert_all("killed");
}

/// A test that removes each environment variable still runs the mutant.
#[test]
fn a_test_that_clears_the_environment_runs_the_mutant() {
    let lib = r#"
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
"#;
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
