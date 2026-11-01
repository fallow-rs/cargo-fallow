//! Runs the tests with no active mutant and compares two runs.
//!
//! nextest runs through its CLI, and the runner reads its JSON. The doctests
//! run separately through `cargo test --doc`, because nextest does not run them.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::mutation::process::{Exit, JOBS, cargo, run_to_files};

/// The exit code of nextest when tests ran and one failed.
const NEXTEST_TESTS_FAILED: i32 = 100;

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
    /// The passes of each test name. A doctest name can repeat.
    #[serde(skip)]
    pub passes: BTreeMap<String, u32>,
}

impl Suite {
    fn new(status: SuiteStatus) -> Self {
        Self {
            status,
            pass: 0,
            fail: 0,
            ignored: 0,
            passes: BTreeMap::new(),
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

/// The inputs of one test run.
#[derive(Debug, Clone, Copy)]
pub struct TestPlan<'a> {
    /// The package folder in a workspace copy. cargo selects the package from
    /// this folder, because `-p name` is ambiguous when a dependency has the
    /// same name.
    pub dir: &'a Path,
    /// The target folder.
    pub target_dir: &'a Path,
    /// The folder for the output files.
    pub logs: &'a Path,
    /// The name prefix of the output files.
    pub label: &'a str,
    /// The time limit of each suite.
    pub limit: Duration,
}

/// Runs nextest and the doctests.
pub fn run_tests(plan: TestPlan<'_>) -> io::Result<TestRun> {
    Ok(TestRun {
        nextest: run_nextest(plan)?,
        doc: run_doctests(plan)?,
    })
}

fn run_nextest(plan: TestPlan<'_>) -> io::Result<Suite> {
    let mut command = cargo(plan.dir, plan.target_dir);
    command
        .env("NEXTEST_EXPERIMENTAL_LIBTEST_JSON", "1")
        .args(["nextest", "run", "--no-fail-fast", "--no-tests=pass"])
        .args(["-j", JOBS, "--message-format", "libtest-json"]);
    let out = plan.logs.join(format!("{}-nextest.json", plan.label));
    let err = plan.logs.join(format!("{}-nextest.err", plan.label));
    let exit = run_to_files(command, &out, &err, plan.limit)?;
    let status = match exit {
        Exit::TimedOut => SuiteStatus::TimedOut,
        Exit::Code(Some(0 | NEXTEST_TESTS_FAILED)) => SuiteStatus::Completed,
        Exit::Code(_) => SuiteStatus::DidNotRun,
    };
    Ok(parse_nextest(&fs::read_to_string(out)?, status))
}

#[derive(Deserialize)]
struct NextestLine {
    #[serde(rename = "type")]
    kind: String,
    event: String,
    name: Option<String>,
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
    }
    suite
}

fn run_doctests(plan: TestPlan<'_>) -> io::Result<Suite> {
    let mut command = cargo(plan.dir, plan.target_dir);
    command.args([
        "test",
        "--doc",
        "--no-fail-fast",
        "--",
        "--test-threads",
        JOBS,
    ]);
    let out = plan.logs.join(format!("{}-doc.out", plan.label));
    let err = plan.logs.join(format!("{}-doc.err", plan.label));
    let exit = run_to_files(command, &out, &err, plan.limit)?;
    let suite = parse_doctests(&fs::read_to_string(out)?);
    let errors = fs::read_to_string(err)?;
    let status = match exit {
        Exit::TimedOut => SuiteStatus::TimedOut,
        Exit::Code(Some(0)) => SuiteStatus::Completed,
        // cargo refuses `--doc` for a package with no library.
        Exit::Code(_) if errors.contains("no library targets found") => SuiteStatus::Completed,
        Exit::Code(_) if suite.pass + suite.fail > 0 => SuiteStatus::Completed,
        Exit::Code(_) => SuiteStatus::DidNotRun,
    };
    Ok(Suite { status, ..suite })
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

fn lost_passes(clean: &Suite, schema: &Suite) -> Vec<String> {
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
