//! Runs the tests of one source tree and reads the results.
//!
//! nextest runs through its CLI with one process for each test, and the runner
//! reads its JSON. The doctests run separately through `cargo test --doc`,
//! because nextest does not run them.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::mutation::coverage::Objects;
use crate::mutation::guard::{Guard, Scope};
use crate::mutation::process::{Exit, cargo_with_flags, run_to_files, with_active_mutant};
use crate::mutation::status::{Status, Verdict};

/// The exit code of nextest when tests ran and one failed.
const NEXTEST_TESTS_FAILED: i32 = 100;
/// The time limit of one `cargo metadata` call and of one listing.
const LIST_LIMIT: Duration = Duration::from_secs(600);

/// How a test suite ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuiteStatus {
    /// The suite ran to its end.
    Completed,
    /// The suite passed the time limit.
    TimedOut,
    /// The runner or the build failed before the tests ended.
    DidNotRun,
}

/// The result of one test suite.
#[derive(Debug, Clone, Serialize)]
pub struct Suite {
    /// How the suite ended.
    pub status: SuiteStatus,
    /// The number of passed tests.
    pub pass: u32,
    /// The number of failed tests.
    pub fail: u32,
    /// The number of ignored tests.
    pub ignored: u32,
    /// The wall time in milliseconds.
    pub elapsed_ms: u64,
    /// The passes of each test name. A doctest name can repeat.
    #[serde(skip)]
    pub passes: BTreeMap<String, u32>,
    /// The failed tests, in the order of their results.
    #[serde(skip)]
    pub failed: Vec<String>,
    /// The time of each passed test in milliseconds, when the runner reports it.
    #[serde(skip)]
    pub times: BTreeMap<String, u64>,
}

impl Suite {
    fn new(status: SuiteStatus) -> Self {
        Self {
            status,
            pass: 0,
            fail: 0,
            ignored: 0,
            elapsed_ms: 0,
            passes: BTreeMap::new(),
            failed: Vec::new(),
            times: BTreeMap::new(),
        }
    }

    fn record(&mut self, name: &str, outcome: Outcome) {
        match outcome {
            Outcome::Pass => {
                self.pass += 1;
                *self.passes.entry(name.to_owned()).or_default() += 1;
            }
            Outcome::Fail => {
                self.fail += 1;
                self.passes.entry(name.to_owned()).or_default();
                self.failed.push(name.to_owned());
            }
            Outcome::Ignored => self.ignored += 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Pass,
    Fail,
    Ignored,
}

/// The two suites of one tree.
#[derive(Debug, Clone, Serialize)]
pub struct TestRun {
    /// The nextest suite.
    pub nextest: Suite,
    /// The doctest suite.
    pub doc: Suite,
}

/// One source tree with its build output.
#[derive(Clone, Copy)]
pub struct TestTree<'a> {
    /// The package folder in a workspace copy. cargo selects the package from
    /// this folder, because `-p name` is ambiguous when a dependency has the
    /// same name.
    pub dir: &'a Path,
    /// The target folder.
    pub target_dir: &'a Path,
    /// The folder for the output files.
    pub logs: &'a Path,
    /// The number of parallel build jobs.
    pub build_jobs: usize,
    /// The memory guard.
    pub guard: &'a Guard,
    /// More compiler flags for each crate of the build.
    pub rustflags: &'a str,
}

impl TestTree<'_> {
    /// A cargo command for this tree. An instrumented build script writes a
    /// profile, so the command sends each profile to the log folder.
    fn cargo(&self) -> Command {
        let mut command =
            cargo_with_flags(self.dir, self.target_dir, self.build_jobs, self.rustflags);
        if !self.rustflags.is_empty() {
            command.env("LLVM_PROFILE_FILE", self.logs.join("build-%p-%m.profraw"));
        }
        command
    }
}

/// What one test run does.
#[derive(Debug, Clone)]
pub struct RunSpec {
    /// The name prefix of the output files.
    pub label: String,
    /// The active mutant of a schema build.
    pub mutant: Option<u64>,
    /// The time limit of the suite.
    pub limit: Duration,
    /// True when the suite stops at the first failure.
    pub fail_fast: bool,
    /// The test processes that run at the same time.
    pub jobs: usize,
    /// The tests that stay out of the run.
    pub exclude: BTreeSet<String>,
    /// The filter of the tests to run. With a value, `exclude` has no effect.
    pub only: Option<String>,
    /// The value of `LLVM_PROFILE_FILE` for the tests.
    pub profile_file: Option<PathBuf>,
    /// A nextest tool config with the order of the tests.
    pub order: Option<PathBuf>,
    /// True when the run removes its output files.
    pub remove_logs: bool,
}

impl RunSpec {
    /// A run of each test with `jobs` processes, with no mutant and with the time
    /// limit of one hour.
    pub fn new(jobs: usize) -> Self {
        Self {
            label: String::new(),
            mutant: None,
            limit: Duration::from_hours(1),
            fail_fast: false,
            jobs,
            exclude: BTreeSet::new(),
            only: None,
            profile_file: None,
            order: None,
            remove_logs: false,
        }
    }
}

/// The files that let nextest run without a call of cargo.
#[derive(Debug, Clone)]
pub struct BuildMeta {
    binaries: PathBuf,
    cargo: PathBuf,
}

/// The JSON of `nextest list --list-type binaries-only`.
#[derive(Deserialize)]
struct BinaryList {
    #[serde(rename = "rust-build-meta")]
    meta: BinaryListMeta,
    #[serde(rename = "rust-binaries")]
    binaries: BTreeMap<String, BinaryListEntry>,
}

#[derive(Deserialize)]
struct BinaryListMeta {
    #[serde(rename = "target-directory")]
    target_directory: PathBuf,
    #[serde(rename = "non-test-binaries", default)]
    non_test_binaries: BTreeMap<String, Vec<NonTestBinary>>,
}

#[derive(Deserialize)]
struct NonTestBinary {
    path: PathBuf,
}

#[derive(Deserialize)]
struct BinaryListEntry {
    #[serde(rename = "binary-id")]
    id: String,
    #[serde(rename = "binary-name")]
    name: String,
    #[serde(rename = "binary-path")]
    path: PathBuf,
}

impl BuildMeta {
    /// Reads the binaries of this build. A path in the list is relative to the
    /// target folder when it is not absolute.
    pub fn read_objects(&self) -> io::Result<Objects> {
        let text = fs::read_to_string(&self.binaries)?;
        let list: BinaryList = serde_json::from_str(&text)?;
        let root = &list.meta.target_directory;
        let mut tests: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        for entry in list.binaries.into_values() {
            let package = entry
                .id
                .split_once("::")
                .map_or(entry.id.as_str(), |(package, _)| package);
            let suite = format!("{package}::{}", entry.name);
            tests.entry(suite).or_default().push(root.join(entry.path));
        }
        Ok(Objects {
            tests,
            non_test: list
                .meta
                .non_test_binaries
                .into_values()
                .flatten()
                .map(|binary| root.join(binary.path))
                .collect(),
        })
    }
}

/// Builds the tests and lists the binaries. Returns the stderr text of a build
/// that failed.
pub fn prepare(tree: TestTree<'_>, label: &str) -> io::Result<Result<BuildMeta, String>> {
    let binaries = tree.logs.join(format!("{label}-binaries.json"));
    let err = tree.logs.join(format!("{label}-binaries.err"));
    let mut command = tree.cargo();
    command.args([
        "nextest",
        "list",
        "--list-type",
        "binaries-only",
        "--message-format",
        "json",
    ]);
    let exit = run_to_files(
        command,
        &binaries,
        &err,
        LIST_LIMIT,
        tree.guard,
        Scope::Build,
    )?;
    if exit != Exit::Code(Some(0)) {
        return Ok(Err(fs::read_to_string(err)?));
    }
    let meta = tree.logs.join(format!("{label}-metadata.json"));
    let err = tree.logs.join(format!("{label}-metadata.err"));
    let mut command = tree.cargo();
    command.args([
        "metadata",
        "--format-version=1",
        "--all-features",
        "--no-deps",
    ]);
    let exit = run_to_files(command, &meta, &err, LIST_LIMIT, tree.guard, Scope::Build)?;
    if exit != Exit::Code(Some(0)) {
        return Ok(Err(fs::read_to_string(err)?));
    }
    Ok(Ok(BuildMeta {
        binaries,
        cargo: meta,
    }))
}

/// Runs nextest on the prepared binaries. The call runs no build.
pub fn run_nextest(
    tree: TestTree<'_>,
    meta: &BuildMeta,
    spec: &RunSpec,
) -> io::Result<(Exit, Suite)> {
    let mut command = tree.cargo();
    command
        .env("NEXTEST_EXPERIMENTAL_LIBTEST_JSON", "1")
        .args(["nextest", "run", "--no-tests=pass", "--retries=0"])
        .arg(if spec.fail_fast {
            "--fail-fast"
        } else {
            "--no-fail-fast"
        })
        .arg("--binaries-metadata")
        .arg(&meta.binaries)
        .arg("--cargo-metadata")
        .arg(&meta.cargo)
        .args([
            "-j",
            &spec.jobs.to_string(),
            "--message-format",
            "libtest-json",
        ]);
    if let Some(filter) = spec.only.clone().or_else(|| exclude_filter(&spec.exclude)) {
        command.args(["-E", &filter]);
    }
    if let Some(order) = &spec.order {
        command.arg("--tool-config-file").arg(tool_config(order));
    }
    if let Some(profile) = &spec.profile_file {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    // This run builds nothing, and a test that starts cargo gets a plain compiler.
    command.env("RUSTC_WRAPPER", "");
    let command = activate(command, tree, spec)?;
    let out = tree.logs.join(format!("{}-nextest.json", spec.label));
    let err = tree.logs.join(format!("{}-nextest.err", spec.label));
    let start = Instant::now();
    let exit = run_to_files(command, &out, &err, spec.limit, tree.guard, Scope::Tests)?;
    let status = match exit {
        Exit::TimedOut | Exit::Killed(_) => SuiteStatus::TimedOut,
        Exit::Code(Some(0 | NEXTEST_TESTS_FAILED)) => SuiteStatus::Completed,
        Exit::Code(_) => SuiteStatus::DidNotRun,
    };
    let mut suite = parse_nextest(&fs::read_to_string(&out)?, status);
    suite.elapsed_ms = elapsed_ms(start);
    if spec.remove_logs {
        remove_logs(&[out, err]);
    }
    Ok((exit, suite))
}

/// Selects the mutant of `spec` in `command`.
fn activate(command: Command, tree: TestTree<'_>, spec: &RunSpec) -> io::Result<Command> {
    let Some(mutant) = spec.mutant else {
        return Ok(command);
    };
    let record = tree.logs.join(format!("{}.active", spec.label));
    with_active_mutant(command, &record, mutant)
}

/// The filter that leaves out `exclude`, with the binary part of each name
/// removed. A name in two binaries stays out of both, and that only loses a test.
fn exclude_filter(exclude: &BTreeSet<String>) -> Option<String> {
    if exclude.is_empty() {
        return None;
    }
    let terms: Vec<String> = exclude
        .iter()
        .map(|name| {
            let test = name
                .rsplit_once('$')
                .map_or(name.as_str(), |(_, test)| test);
            format!("test(={})", escape_filter(test))
        })
        .collect();
    Some(format!("not ({})", terms.join(" | ")))
}

/// The argument of `--tool-config-file`.
fn tool_config(path: &Path) -> OsString {
    let mut value = OsString::from("cargo-fallow:");
    value.push(path);
    value
}

/// A test name of the JSON of nextest: `package::binary$test`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestName<'a> {
    /// The package.
    pub package: &'a str,
    /// The name of the test binary.
    pub binary: &'a str,
    /// The name of the test.
    pub test: &'a str,
}

impl<'a> TestName<'a> {
    /// Reads a name. A package name and a binary name have no `$` and no `:`. A
    /// custom test harness can use any text as a name. Such a name gets `None`,
    /// because a filter that nextest refuses stops each run that has the filter.
    pub fn parse(name: &'a str) -> Option<Self> {
        let (suite, test) = name.split_once('$')?;
        let (package, binary) = suite.split_once("::")?;
        let plain = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '.');
        if !name.replacen('$', "", 1).chars().all(plain) {
            return None;
        }
        Some(Self {
            package,
            binary,
            test,
        })
    }

    /// The filter of the binary of this test.
    pub fn binary_filter(&self) -> String {
        format!(
            "package(={}) & binary(={})",
            escape_filter(self.package),
            escape_filter(self.binary)
        )
    }

    /// The filter of this test. A bin target and a test target with one name share
    /// this filter, and that only runs one more test.
    pub fn filter(&self) -> String {
        format!(
            "{} & test(={})",
            self.binary_filter(),
            escape_filter(self.test)
        )
    }
}

/// Escapes a text for a filter of nextest.
pub fn escape_filter(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '(' | ')' | ',' | '\\' | '/' | '|' | '&' | ' ') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn elapsed_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// A mutant run writes one file per mutant, so a large crate does not fill the
/// disk. A failed removal only leaves a file in a work folder.
fn remove_logs(paths: &[PathBuf]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

#[derive(Deserialize)]
struct NextestLine {
    #[serde(rename = "type")]
    kind: String,
    event: String,
    name: Option<String>,
    exec_time: Option<f64>,
}

fn parse_nextest(stdout: &str, status: SuiteStatus) -> Suite {
    let mut suite = Suite::new(status);
    for line in stdout.lines() {
        let Ok(parsed) = serde_json::from_str::<NextestLine>(line) else {
            continue;
        };
        let Some(name) = parsed.name.filter(|_| parsed.kind == "test") else {
            continue;
        };
        let outcome = match parsed.event.as_str() {
            "ok" => Outcome::Pass,
            "failed" | "timeout" => Outcome::Fail,
            "ignored" => Outcome::Ignored,
            _ => continue,
        };
        suite.record(&name, outcome);
        if outcome == Outcome::Pass
            && let Some(seconds) = parsed.exec_time
        {
            suite.times.insert(name, seconds_to_ms(seconds));
        }
    }
    suite
}

/// A time from the JSON of nextest. A value that is not a time counts as 0.
fn seconds_to_ms(seconds: f64) -> u64 {
    Duration::try_from_secs_f64(seconds).map_or(0, |time| {
        u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
    })
}

/// Runs the doctests of the tree.
pub fn run_doctests(tree: TestTree<'_>, spec: &RunSpec) -> io::Result<(Exit, Suite)> {
    let mut command = tree.cargo();
    command.args([
        "test",
        "--doc",
        "--no-fail-fast",
        "--",
        "--test-threads",
        &spec.jobs.to_string(),
    ]);
    let command = activate(command, tree, spec)?;
    let out = tree.logs.join(format!("{}-doc.out", spec.label));
    let err = tree.logs.join(format!("{}-doc.err", spec.label));
    let start = Instant::now();
    let exit = run_to_files(command, &out, &err, spec.limit, tree.guard, Scope::Tests)?;
    let suite = parse_doctests(&fs::read_to_string(&out)?);
    let errors = fs::read_to_string(&err)?;
    let status = match exit {
        Exit::TimedOut | Exit::Killed(_) => SuiteStatus::TimedOut,
        Exit::Code(Some(0)) => SuiteStatus::Completed,
        // cargo refuses `--doc` for a package with no library.
        Exit::Code(_) if errors.contains("no library targets found") => SuiteStatus::Completed,
        Exit::Code(_) if suite.pass + suite.fail > 0 => SuiteStatus::Completed,
        Exit::Code(_) => SuiteStatus::DidNotRun,
    };
    if spec.remove_logs {
        remove_logs(&[out, err]);
    }
    Ok((
        exit,
        Suite {
            status,
            elapsed_ms: elapsed_ms(start),
            ..suite
        },
    ))
}

/// Reads the text of a doctest run. libtest has no JSON format on stable. The
/// name leaves out the line number, because a schema can move a doctest.
fn parse_doctests(stdout: &str) -> Suite {
    let mut suite = Suite::new(SuiteStatus::Completed);
    for line in stdout.lines() {
        let Some((name, result)) = line
            .strip_prefix("test ")
            .and_then(|rest| rest.rsplit_once(" ... "))
        else {
            continue;
        };
        let outcome = match result {
            "ok" => Outcome::Pass,
            "FAILED" => Outcome::Fail,
            result if result.starts_with("ignored") => Outcome::Ignored,
            _ => continue,
        };
        suite.record(&strip_line_number(name), outcome);
    }
    suite
}

/// The verdict of one suite with an active mutant, or `None` when each test that
/// counts passed. A test in `known_failures` fails with no mutant, so it decides
/// nothing.
pub fn classify(
    runner: &str,
    exit: &Exit,
    suite: &Suite,
    known_failures: &BTreeSet<String>,
) -> Option<Verdict> {
    let code = match exit {
        Exit::Killed(reason) => {
            return Some(Verdict::with_reason(
                Status::RuntimeError,
                reason.to_string(),
            ));
        }
        Exit::TimedOut => return Some(Verdict::new(Status::Timeout)),
        Exit::Code(Some(0)) => return None,
        Exit::Code(code) => code,
    };
    if let Some(test) = suite
        .failed
        .iter()
        .find(|name| !known_failures.contains(*name))
    {
        return Some(Verdict::killed(test.replace('$', " ")));
    }
    if !suite.failed.is_empty() {
        return None;
    }
    if suite.status == SuiteStatus::Completed && *code != Some(NEXTEST_TESTS_FAILED) {
        return None;
    }
    let text = code.map_or_else(|| "a signal".to_owned(), |code| format!("exit code {code}"));
    Some(Verdict::with_reason(
        Status::RuntimeError,
        format!("{runner} ended with {text} and no failed test"),
    ))
}

/// `src/lib.rs - f (line 12) - compile fail` gives `src/lib.rs - f - compile fail`.
fn strip_line_number(name: &str) -> String {
    let Some((head, tail)) = name.split_once(" (line ") else {
        return name.to_owned();
    };
    let tail = tail.split_once(')').map_or("", |(_, rest)| rest);
    format!("{head}{tail}")
}

/// The comparison of a clean run and a schema run.
#[derive(Debug, Clone, Serialize)]
pub struct Comparison {
    /// The tests that pass in the clean run and do not pass in the schema run.
    pub regressions: Vec<String>,
    /// True when both runs completed and `regressions` is empty.
    pub tests_pass_with_schema: bool,
}

/// The tests that fail in a run.
pub fn failures(run: &TestRun) -> BTreeSet<String> {
    run.nextest
        .failed
        .iter()
        .chain(&run.doc.failed)
        .cloned()
        .collect()
}

/// Compares the two runs.
pub fn compare(clean: &TestRun, schema: &TestRun) -> Comparison {
    let mut regressions = lost_passes(&clean.nextest, &schema.nextest);
    regressions.extend(lost_passes(&clean.doc, &schema.doc));
    let completed = [&clean.nextest, &clean.doc, &schema.nextest, &schema.doc]
        .iter()
        .all(|suite| suite.status == SuiteStatus::Completed);
    Comparison {
        tests_pass_with_schema: completed && regressions.is_empty(),
        regressions,
    }
}

/// The tests with fewer passes in `schema` than in `clean`.
pub fn lost_passes(clean: &Suite, schema: &Suite) -> Vec<String> {
    clean
        .passes
        .iter()
        .filter(|&(name, &passes)| schema.passes.get(name).copied().unwrap_or(0) < passes)
        .map(|(name, _)| name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mutation::guard::KillReason;

    fn suite(tests: &[(&str, Outcome)]) -> Suite {
        let mut suite = Suite::new(SuiteStatus::Completed);
        for (name, outcome) in tests {
            suite.record(name, *outcome);
        }
        suite
    }

    fn run(nextest: Suite) -> TestRun {
        TestRun {
            nextest,
            doc: Suite::new(SuiteStatus::Completed),
        }
    }

    #[test]
    fn nextest_lines_are_counted() {
        let stdout = concat!(
            "not json\n",
            r#"{"type":"suite","event":"started","test_count":3}"#,
            "\n",
            r#"{"type":"test","event":"started","name":"p::p$a"}"#,
            "\n",
            r#"{"type":"test","event":"ok","name":"p::p$a","exec_time":0.1}"#,
            "\n",
            r#"{"type":"test","event":"failed","name":"p::p$b"}"#,
            "\n",
            r#"{"type":"test","event":"ignored","name":"p::p$c"}"#,
            "\n",
            r#"{"type":"suite","event":"failed","passed":1,"failed":1}"#,
            "\n",
        );
        let parsed = parse_nextest(stdout, SuiteStatus::Completed);
        assert_eq!((parsed.pass, parsed.fail, parsed.ignored), (1, 1, 1));
        assert_eq!(parsed.passes.get("p::p$a"), Some(&1));
        assert_eq!(parsed.passes.get("p::p$b"), Some(&0));
        assert_eq!(parsed.times.get("p::p$a"), Some(&100));
        assert_eq!(parsed.times.get("p::p$b"), None);
    }

    #[test]
    fn a_test_name_has_three_parts() {
        let name = TestName::parse("my-p::my_p$t::a").filter(|name| name.test == "t::a");
        assert_eq!(
            name.map(|name| name.filter()).as_deref(),
            Some("package(=my-p) & binary(=my_p) & test(=t::a)")
        );
        assert_eq!(TestName::parse("no-dollar::x"), None);
        assert_eq!(TestName::parse("no-suite$t"), None);
        assert_eq!(TestName::parse("p::b$a test (with) text"), None);
    }

    #[test]
    fn a_time_that_is_not_a_time_counts_as_zero() {
        assert_eq!(seconds_to_ms(-1.0), 0);
        assert_eq!(seconds_to_ms(f64::NAN), 0);
        assert_eq!(seconds_to_ms(1.5), 1500);
    }

    #[test]
    fn the_binary_list_gives_the_objects() {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let binaries = dir.path().join("list.json");
        let text = r#"{"rust-build-meta":{"target-directory":"/t","non-test-binaries":{"p":[{"name":"m","path":"debug/m"}]}},"rust-binaries":{"x":{"binary-id":"p::bin/x","binary-name":"x","binary-path":"/t/debug/deps/x"},"l":{"binary-id":"my-p","binary-name":"my_p","binary-path":"/t/debug/deps/l"}}}"#;
        let Ok(()) = fs::write(&binaries, text) else {
            panic!("write the list");
        };
        let meta = BuildMeta {
            binaries,
            cargo: PathBuf::new(),
        };
        let Ok(objects) = meta.read_objects() else {
            panic!("the list is valid");
        };
        assert_eq!(objects.tests["p::x"], [Path::new("/t/debug/deps/x")]);
        assert_eq!(objects.tests["my-p::my_p"], [Path::new("/t/debug/deps/l")]);
        assert_eq!(objects.non_test, [Path::new("/t/debug/m")]);
    }

    #[test]
    fn doctest_lines_are_counted_and_names_lose_the_line() {
        let stdout = "\nrunning 4 tests\n\
                      test src/lib.rs - f (line 2) ... ok\n\
                      test src/lib.rs - f (line 9) ... ok\n\
                      test src/lib.rs - g (line 12) - compile fail ... FAILED\n\
                      test src/lib.rs - h (line 14) ... ignored, reason\n\
                      test result: FAILED. 2 passed; 1 failed\n";
        let parsed = parse_doctests(stdout);
        assert_eq!((parsed.pass, parsed.fail, parsed.ignored), (2, 1, 1));
        assert_eq!(parsed.passes.get("src/lib.rs - f"), Some(&2));
        assert_eq!(parsed.passes.get("src/lib.rs - g - compile fail"), Some(&0));
    }

    #[test]
    fn exclude_filter_uses_the_test_part_and_escapes() {
        assert_eq!(exclude_filter(&BTreeSet::new()), None);
        let names: BTreeSet<String> = ["p::p$t::a".to_owned(), "b(x, 1)".to_owned()].into();
        assert_eq!(
            exclude_filter(&names).as_deref(),
            Some("not (test(=b\\(x\\,\\ 1\\)) | test(=t::a))")
        );
    }

    fn failed_suite(names: &[&str]) -> Suite {
        let mut suite = Suite::new(SuiteStatus::Completed);
        for name in names {
            suite.record(name, Outcome::Fail);
        }
        suite
    }

    fn verdict(exit: &Exit, suite: &Suite, known: &[&str]) -> Option<Verdict> {
        let known = known.iter().map(|name| (*name).to_owned()).collect();
        classify("nextest", exit, suite, &known)
    }

    #[test]
    fn exit_zero_decides_nothing() {
        let suite = failed_suite(&[]);
        assert_eq!(verdict(&Exit::Code(Some(0)), &suite, &[]), None);
    }

    #[test]
    fn first_new_failure_kills_the_mutant() {
        let suite = failed_suite(&["old", "p::p$new"]);
        let result = verdict(&Exit::Code(Some(100)), &suite, &["old"]);
        assert_eq!(result, Some(Verdict::killed("p::p new")));
    }

    #[test]
    fn only_a_known_failure_decides_nothing() {
        let suite = failed_suite(&["old"]);
        assert_eq!(verdict(&Exit::Code(Some(100)), &suite, &["old"]), None);
    }

    #[test]
    fn a_timeout_and_a_guard_kill_have_their_own_status() {
        let suite = failed_suite(&["x"]);
        let timeout = verdict(&Exit::TimedOut, &suite, &[]);
        assert_eq!(timeout, Some(Verdict::new(Status::Timeout)));
        let killed = verdict(&Exit::Killed(KillReason::Blind), &suite, &[]);
        assert_eq!(
            killed.map(|verdict| verdict.status),
            Some(Status::RuntimeError)
        );
    }

    #[test]
    fn a_runner_failure_without_a_test_is_a_runtime_error() {
        let suite = Suite::new(SuiteStatus::DidNotRun);
        for (exit, text) in [
            (Exit::Code(Some(104)), "exit code 104"),
            (Exit::Code(None), "a signal"),
        ] {
            let result = verdict(&exit, &suite, &[]);
            let reason = result
                .and_then(|verdict| verdict.reason)
                .unwrap_or_default();
            assert!(reason.contains(text), "{reason}");
        }
    }

    #[test]
    fn lost_pass_is_a_regression() {
        let clean = run(suite(&[("a", Outcome::Pass), ("b", Outcome::Pass)]));
        let schema = run(suite(&[("a", Outcome::Pass), ("b", Outcome::Fail)]));
        let result = compare(&clean, &schema);
        assert_eq!(result.regressions, ["b"]);
        assert!(!result.tests_pass_with_schema);
    }

    #[test]
    fn missing_test_is_a_regression() {
        let result = compare(&run(suite(&[("a", Outcome::Pass)])), &run(suite(&[])));
        assert_eq!(result.regressions, ["a"]);
    }

    #[test]
    fn same_failure_in_both_runs_is_no_regression() {
        let clean = run(suite(&[("a", Outcome::Fail)]));
        let schema = run(suite(&[("a", Outcome::Fail)]));
        assert!(compare(&clean, &schema).tests_pass_with_schema);
    }

    #[test]
    fn suite_that_did_not_complete_fails_the_comparison() {
        let clean = run(Suite::new(SuiteStatus::Completed));
        let schema = run(Suite::new(SuiteStatus::TimedOut));
        let result = compare(&clean, &schema);
        assert!(result.regressions.is_empty());
        assert!(!result.tests_pass_with_schema);
    }
}
