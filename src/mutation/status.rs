//! The status set of a mutant and the rules that give a mutant its status.
//!
//! The set is the same as in the `fallow mutation` command of the JavaScript
//! tool, and the retry rules follow its design.

use serde::{Deserialize, Serialize};

/// The status of one mutant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// A test failed with the mutant.
    Killed,
    /// Each test passed with the mutant, also in the retry.
    Survived,
    /// The tests did not end within the limit.
    Timeout,
    /// Two runs of the same mutant disagree.
    Flaky,
    /// No test can reach the mutant: no build reads it, or the package has no
    /// test that passes.
    NoCoverage,
    /// The mutant does not compile.
    CompileError,
    /// The run has no test result: the memory guard killed it, or a runner failed.
    RuntimeError,
    /// The run stopped before this mutant, or the command ran no mutant.
    Pending,
}

impl Status {
    /// The wire name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Killed => "killed",
            Self::Survived => "survived",
            Self::Timeout => "timeout",
            Self::Flaky => "flaky",
            Self::NoCoverage => "no_coverage",
            Self::CompileError => "compile_error",
            Self::RuntimeError => "runtime_error",
            Self::Pending => "pending",
        }
    }
}

/// How the engine runs a mutant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// In one build with all other schema mutants, selected by a runtime switch.
    Schema,
    /// In its own rebuild of a scratch copy.
    Rebuild,
}

/// The result of the runs of one mutant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// The status.
    pub status: Status,
    /// The reason for a status without a test result.
    pub reason: Option<String>,
    /// The first test that failed.
    pub killed_by: Option<String>,
}

impl Verdict {
    /// A verdict with only a status.
    pub const fn new(status: Status) -> Self {
        Self {
            status,
            reason: None,
            killed_by: None,
        }
    }

    /// A verdict with a reason.
    pub fn with_reason(status: Status, reason: impl Into<String>) -> Self {
        Self {
            reason: Some(reason.into()),
            ..Self::new(status)
        }
    }

    /// A kill by one test.
    pub fn killed(test: impl Into<String>) -> Self {
        Self {
            killed_by: Some(test.into()),
            ..Self::new(Status::Killed)
        }
    }

    /// True when the status gets one retry. A kill gets none: the test failed
    /// with the mutant and passed in the baseline.
    pub const fn wants_retry(&self) -> bool {
        matches!(self.status, Status::Survived | Status::Timeout)
    }
}

/// The reason of a survivor when a test failed next to another test, and each test
/// passed one at a time.
pub const OVERLAP: &str = "a test failed next to another test, and each test passed one at a time";

/// The reason of a survivor when its retry failed a test, and each test passed one
/// at a time.
pub const RETRY_PASSED_ALONE: &str = "the retry failed a test, and each test passed one at a \
     time: the cause is an overlap with another test or a flaky test";

/// Merges the first run and its retry.
///
/// A failure in the retry has had a second look with no other test process, as a
/// kill in the first run has. `retry_failed` is true when the retry failed a test,
/// before that second look. A failure that does not repeat in the second look
/// arrives as a survivor with more than one job, or as flaky with one job.
///
/// | First | Retry | Result |
/// | --- | --- | --- |
/// | survived | survived | survived |
/// | survived | survived after a failure | survived, with the reason of the failure |
/// | survived | timeout | survived |
/// | survived | killed | flaky |
/// | survived | flaky | flaky |
/// | timeout | timeout | timeout |
/// | timeout | survived after a failure | timeout |
/// | timeout | killed | killed |
/// | any | `runtime_error` | `runtime_error` |
/// | any | other result | flaky |
pub fn merge_retry(first: Verdict, retry: Verdict, retry_failed: bool) -> Verdict {
    match (first.status, retry.status) {
        (_, Status::RuntimeError) | (Status::Timeout, Status::Killed) => retry,
        (Status::Survived, Status::Survived) if retry_failed => Verdict {
            reason: Some(RETRY_PASSED_ALONE.to_owned()),
            ..first
        },
        // The failure of the retry did not repeat, so it says nothing about the
        // mutant. The first run timed out for real.
        (Status::Timeout, Status::Survived) if retry_failed => first,
        (Status::Survived | Status::Timeout, Status::Timeout)
        | (Status::Survived, Status::Survived) => first,
        (Status::Survived, Status::Killed) => Verdict {
            reason: Some("the first run passed each test, and the retry failed a test".to_owned()),
            ..retry_flaky(retry)
        },
        (Status::Survived, Status::Flaky) => Verdict {
            reason: Some(
                "the first run passed each test, the retry failed a test, and a third run passed"
                    .to_owned(),
            ),
            ..retry_flaky(retry)
        },
        (_, _) => Verdict::with_reason(Status::Flaky, "two runs of the mutant disagree"),
    }
}

fn retry_flaky(retry: Verdict) -> Verdict {
    Verdict {
        status: Status::Flaky,
        ..retry
    }
}

/// The share of detected mutants, in percent, or `None` with no test result.
/// A flaky mutant, an error and a mutant with no coverage do not count.
pub fn score(killed: usize, timeout: usize, survived: usize) -> Option<f64> {
    let detected = killed + timeout;
    let total = detected + survived;
    (total > 0).then(|| detected as f64 * 100.0 / total as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merged(first: Status, retry: Status) -> Status {
        merge_retry(
            Verdict::new(first),
            Verdict::new(retry),
            retry == Status::Killed,
        )
        .status
    }

    fn merged_after_a_failure(first: Status, retry: Status) -> Status {
        merge_retry(Verdict::new(first), Verdict::new(retry), true).status
    }

    #[test]
    fn retry_table() {
        use Status::{Flaky, Killed, RuntimeError, Survived, Timeout};
        assert_eq!(merged(Survived, Survived), Survived);
        assert_eq!(merged(Survived, Timeout), Survived);
        assert_eq!(merged(Timeout, Timeout), Timeout);
        assert_eq!(merged(Timeout, Killed), Killed);
        assert_eq!(merged(Survived, Killed), Flaky);
        assert_eq!(merged(Survived, Flaky), Flaky);
        assert_eq!(merged(Timeout, Flaky), Flaky);
        assert_eq!(merged(Timeout, Survived), Flaky);
        assert_eq!(merged(Survived, RuntimeError), RuntimeError);
        assert_eq!(merged(Timeout, RuntimeError), RuntimeError);
        assert_eq!(merged_after_a_failure(Survived, Survived), Survived);
        assert_eq!(merged_after_a_failure(Timeout, Survived), Timeout);
        assert_eq!(merged_after_a_failure(Survived, Flaky), Flaky);
        assert_eq!(merged_after_a_failure(Timeout, Flaky), Flaky);
    }

    #[test]
    fn a_plain_survivor_has_no_reason() {
        let result = merge_retry(
            Verdict::new(Status::Survived),
            Verdict::new(Status::Survived),
            false,
        );
        assert_eq!(result, Verdict::new(Status::Survived));
    }

    #[test]
    fn a_flaky_kill_keeps_the_test_name() {
        let result = merge_retry(Verdict::new(Status::Survived), Verdict::killed("t"), true);
        assert_eq!(result.status, Status::Flaky);
        assert_eq!(result.killed_by.as_deref(), Some("t"));
        assert!(result.reason.is_some());
    }

    #[test]
    fn a_retry_failure_from_overlap_keeps_a_survivor_with_the_reason() {
        let retry = Verdict::with_reason(Status::Survived, OVERLAP);
        let result = merge_retry(Verdict::new(Status::Survived), retry, true);
        assert_eq!(result.status, Status::Survived);
        assert_eq!(result.reason.as_deref(), Some(RETRY_PASSED_ALONE));
        assert_eq!(result.killed_by, None);
    }

    #[test]
    fn a_retry_failure_from_overlap_keeps_a_timeout() {
        let first = Verdict::with_reason(Status::Timeout, "limit");
        let retry = Verdict::with_reason(Status::Survived, OVERLAP);
        assert_eq!(merge_retry(first.clone(), retry, true), first);
    }

    #[test]
    fn a_retry_failure_that_does_not_repeat_is_flaky_with_the_test_name() {
        let retry = Verdict {
            status: Status::Flaky,
            reason: Some("the first run failed a test, and the second run passed".to_owned()),
            killed_by: Some("t".to_owned()),
        };
        let result = merge_retry(Verdict::new(Status::Survived), retry, true);
        assert_eq!(result.status, Status::Flaky);
        assert_eq!(result.killed_by.as_deref(), Some("t"));
        assert!(
            result
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("third run"))
        );
    }

    #[test]
    fn only_a_survivor_and_a_timeout_get_a_retry() {
        for (status, wanted) in [
            (Status::Survived, true),
            (Status::Timeout, true),
            (Status::Killed, false),
            (Status::RuntimeError, false),
            (Status::CompileError, false),
        ] {
            assert_eq!(Verdict::new(status).wants_retry(), wanted, "{status:?}");
        }
    }

    #[test]
    fn score_counts_only_test_results() {
        assert_eq!(score(0, 0, 0), None);
        assert_eq!(score(3, 1, 4), Some(50.0));
        assert_eq!(score(1, 0, 0), Some(100.0));
    }

    #[test]
    fn statuses_have_the_wire_names() {
        let all = [
            Status::Killed,
            Status::Survived,
            Status::Timeout,
            Status::Flaky,
            Status::NoCoverage,
            Status::CompileError,
            Status::RuntimeError,
            Status::Pending,
        ];
        let names = serde_json::to_string(&all).unwrap_or_default();
        let joined: Vec<String> = all.iter().map(|s| format!("\"{}\"", s.name())).collect();
        assert_eq!(names, format!("[{}]", joined.join(",")));
        assert_eq!(
            names,
            r#"["killed","survived","timeout","flaky","no_coverage","compile_error","runtime_error","pending"]"#
        );
    }
}
