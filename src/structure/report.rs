//! The report envelope that the four structure checks share.

use std::collections::BTreeMap;

use fallow_report_protocol::{GateOutcome, GateStatus};
use serde::Serialize;

use crate::report::TOOL_VERSION;

/// The version of the report shape of the structure checks. A change that
/// removes a member or changes its type increments it.
pub const SCHEMA_VERSION: u32 = 1;

/// The gates of the structure checks, as they appear in `gate_outcomes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Gate {
    /// `boundaries --fail-on-violations`: a rule found a violation.
    BoundaryViolations,
    /// `dupes --fail-on-clones`: the run found a clone group.
    DuplicationFindings,
    /// `dupes --threshold`: the duplicated lines are over the percentage.
    DuplicationThreshold,
    /// `size --fail-on-growth`: a file over the line limit grew.
    FileGrowth,
    /// `complexity --fail-on-complexity`: a function is over a limit.
    HealthFindings,
}

impl Gate {
    /// The wire spelling of the name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BoundaryViolations => "boundary-violations",
            Self::DuplicationFindings => "duplication-findings",
            Self::DuplicationThreshold => "duplication-threshold",
            Self::FileGrowth => "file-growth",
            Self::HealthFindings => "health-findings",
        }
    }
}

/// The report of one structure check. The field order is the wire order.
#[derive(Debug, Clone, Serialize)]
pub struct Report<S, F> {
    /// The version of the report shape.
    pub schema_version: u32,
    /// The version of cargo-fallow.
    pub version: &'static str,
    /// The name of the subcommand.
    pub kind: &'static str,
    /// Always `rust`.
    pub language: &'static str,
    /// The counts and the settings of the run.
    pub summary: S,
    /// The number of findings.
    pub total_issues: usize,
    /// The findings, in a fixed order.
    pub findings: Vec<F>,
    /// The gate outcomes, keyed by gate name.
    pub gate_outcomes: BTreeMap<Gate, GateOutcome>,
}

impl<S, F> Report<S, F> {
    /// A report with the version fields set.
    pub fn new(kind: &'static str, summary: S, findings: Vec<F>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            version: TOOL_VERSION,
            kind,
            language: "rust",
            summary,
            total_issues: findings.len(),
            findings,
            gate_outcomes: BTreeMap::new(),
        }
    }

    /// True when one outcome fails the run.
    pub fn fails_run(&self) -> bool {
        self.gate_outcomes.values().any(GateOutcome::fails_run)
    }
}

/// A next step for one finding. No step changes the code by itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Action {
    /// The kind of the step.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// Always false.
    pub auto_fixable: bool,
    /// What the step does.
    pub description: String,
    /// The comment that suppresses the finding, for a `suppress-line` step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<&'static str>,
    /// Where the comment goes, for a `suppress-line` step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placement: Option<&'static str>,
}

impl Action {
    /// A step that a person does.
    pub fn manual(kind: &'static str, description: impl Into<String>) -> Self {
        Self {
            kind,
            auto_fixable: false,
            description: description.into(),
            comment: None,
            placement: None,
        }
    }

    /// The step that adds a suppression comment.
    pub fn suppress(comment: &'static str, placement: &'static str) -> Self {
        Self {
            kind: "suppress-line",
            auto_fixable: false,
            description: "Suppress with an inline comment".to_owned(),
            comment: Some(comment),
            placement: Some(placement),
        }
    }
}

/// The outcome of a gate that counts findings and fails on one or more.
pub fn count_gate(count: usize, enforced: bool) -> GateOutcome {
    let status = if count == 0 {
        GateStatus::Pass
    } else {
        GateStatus::Fail
    };
    #[expect(
        clippy::cast_precision_loss,
        reason = "a finding count never approaches the f64 integer limit"
    )]
    let observed = count as f64;
    GateOutcome {
        status,
        enforced,
        observed: Some(observed),
        threshold: Some(0.0),
        threshold_label: None,
        files: Vec::new(),
    }
}

/// The word for `count` things.
pub const fn plural<'a>(count: usize, one: &'a str, many: &'a str) -> &'a str {
    if count == 1 { one } else { many }
}

/// The human line of one gate.
pub fn gate_line(gate: Gate, outcome: &GateOutcome) -> String {
    let status = match outcome.status {
        GateStatus::Pass => "pass",
        GateStatus::Warn => "warn",
        GateStatus::Fail => "fail",
        GateStatus::Skipped => "skipped",
    };
    let enforced = if outcome.enforced {
        ""
    } else {
        ", not enforced"
    };
    format!("  {}: {status}{enforced}", gate.as_str())
}

/// The gate block of the human output.
pub fn gates_text(outcomes: &BTreeMap<Gate, GateOutcome>) -> String {
    let mut text = String::from("Gates:");
    for (gate, outcome) in outcomes {
        text.push('\n');
        text.push_str(&gate_line(*gate, outcome));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_names_match_serde() {
        for gate in [
            Gate::BoundaryViolations,
            Gate::DuplicationFindings,
            Gate::DuplicationThreshold,
            Gate::FileGrowth,
            Gate::HealthFindings,
        ] {
            assert_eq!(
                serde_json::to_value(gate).unwrap_or_default(),
                gate.as_str()
            );
        }
    }

    #[test]
    fn only_an_enforced_failure_fails_the_report() {
        let mut report: Report<(), ()> = Report::new("size", (), Vec::new());
        report
            .gate_outcomes
            .insert(Gate::FileGrowth, count_gate(2, false));
        assert!(!report.fails_run());
        report
            .gate_outcomes
            .insert(Gate::FileGrowth, count_gate(2, true));
        assert!(report.fails_run());
        report
            .gate_outcomes
            .insert(Gate::FileGrowth, count_gate(0, true));
        assert!(!report.fails_run());
    }
}
