//! The report envelope that the four structure checks share.

use std::collections::BTreeMap;

use fallow_report_protocol::{GateOutcome, GateStatus};
use serde::Serialize;

use crate::gates::RustGateName as Gate;
use crate::report::TOOL_VERSION;
use crate::scope::Scope;

/// The version of the report shape of the structure checks. A change that
/// removes a member or changes its type increments it.
pub const SCHEMA_VERSION: u32 = 1;

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
}

impl<S, F> Report<S, F> {
    /// Removes the findings that the baseline of `scope` holds. `shared` has
    /// one finding in the shared shape for each finding, in the same order,
    /// and loses the same entries. Call it before the gates count.
    pub fn retain_baseline(&mut self, scope: &Scope, shared: &mut Vec<crate::finding::Finding>) {
        scope.retain_pairs(&mut self.findings, shared);
        self.total_issues = self.findings.len();
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

/// The steps in the shared finding shape.
pub fn shared_actions(actions: &[Action]) -> Vec<crate::finding::Action> {
    actions
        .iter()
        .map(|action| crate::finding::Action {
            kind: action.kind.to_owned(),
            auto_fixable: action.auto_fixable,
            description: action.description.clone(),
        })
        .collect()
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
