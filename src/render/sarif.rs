//! The SARIF 2.1.0 log.
//!
//! A CI system that reads only SARIF must see a failed gate. Each file of a
//! failed gate becomes a result, and each failed gate becomes a tool
//! execution notification. A failed gate is not a tool failure, so
//! `executionSuccessful` stays true.

use std::collections::BTreeMap;

use fallow_report_protocol::{GateFile, GateFileReason, GateOutcome, GateStatus};
use serde_json::{Value, json};

use crate::gates::RustGateName;
use crate::report::TOOL_VERSION;

const SARIF_SCHEMA: &str = "https://json.schemastore.org/sarif-2.1.0.json";
const SARIF_VERSION: &str = "2.1.0";
const TOOL_NAME: &str = "cargo-fallow";
const INFORMATION_URI: &str = "https://github.com/fallow-rs/cargo-fallow";

/// The SARIF log of the gates of a `check` run. Each file of a failed gate
/// is a result.
pub fn gate_log(gates: &BTreeMap<RustGateName, GateOutcome>) -> Value {
    let flagged: Vec<(RustGateName, &GateOutcome, &'static str)> = gates
        .iter()
        .filter_map(|(name, outcome)| level(outcome).map(|level| (*name, outcome, level)))
        .collect();
    let results: Vec<Value> = flagged
        .iter()
        .flat_map(|(name, outcome, level)| {
            outcome.files.iter().map(move |file| {
                json!({
                    "ruleId": name.as_str(),
                    "level": level,
                    "message": { "text": file_message(*name, file) },
                    "locations": [{
                        "physicalLocation": {
                            "artifactLocation": { "uri": file.path }
                        }
                    }],
                })
            })
        })
        .collect();
    let rules: Vec<Value> = flagged
        .iter()
        .filter(|(_, outcome, _)| !outcome.files.is_empty())
        .map(|(name, _, _)| {
            json!({
                "id": name.as_str(),
                "shortDescription": { "text": rule_description(*name) },
            })
        })
        .collect();
    document(&rules, &flagged, &results)
}

/// A SARIF log with one run. Each gate in `flagged` becomes a tool execution
/// notification.
pub(super) fn document(
    rules: &[Value],
    flagged: &[(RustGateName, &GateOutcome, &'static str)],
    results: &[Value],
) -> Value {
    let mut invocation = json!({ "executionSuccessful": true });
    if !flagged.is_empty() {
        invocation["toolExecutionNotifications"] = flagged
            .iter()
            .map(|(name, outcome, level)| notification(*name, outcome, level))
            .collect();
    }

    json!({
        "$schema": SARIF_SCHEMA,
        "version": SARIF_VERSION,
        "runs": [{
            "tool": {
                "driver": {
                    "name": TOOL_NAME,
                    "version": TOOL_VERSION,
                    "informationUri": INFORMATION_URI,
                    "rules": rules,
                }
            },
            "invocations": [invocation],
            "results": results,
        }],
    })
}

/// The SARIF level of a gate that did not pass, or `None` when it passed or
/// did not run. Only an enforced failure is an `error`, because only that
/// failure gives exit code 1.
pub(super) const fn level(outcome: &GateOutcome) -> Option<&'static str> {
    match outcome.status {
        GateStatus::Fail if outcome.enforced => Some("error"),
        GateStatus::Fail | GateStatus::Warn => Some("warning"),
        GateStatus::Pass | GateStatus::Skipped => None,
    }
}

pub(super) fn notification(name: RustGateName, outcome: &GateOutcome, level: &str) -> Value {
    let status = if outcome.status == GateStatus::Warn {
        "warned"
    } else {
        "failed"
    };
    let enforced = if outcome.enforced {
        "enforced"
    } else {
        "not enforced"
    };
    json!({
        "level": level,
        "message": {
            "text": format!("The `{}` gate {status} ({enforced}).", name.as_str()),
        },
        "descriptor": { "id": name.as_str() },
    })
}

pub(super) const fn file_message(name: RustGateName, file: &GateFile) -> &'static str {
    match (name, file.reason) {
        (RustGateName::ParseError, Some(GateFileReason::NestingTooDeep)) => {
            "The file nests too deep to parse."
        }
        (RustGateName::ParseError, None) => "The file does not parse.",
        (RustGateName::ErrorSeverityFindings, _) => {
            "The file has a finding with the error severity."
        }
        (RustGateName::SurvivingMutants, _) => "The file has a mutant that no test detects.",
        (RustGateName::VisibilityFindings, _) => {
            "The file has an item whose visibility can be smaller."
        }
        // The structure gates count findings and list no gate files.
        (
            RustGateName::ApiRemovals
            | RustGateName::BoundaryViolations
            | RustGateName::DuplicationFindings
            | RustGateName::DuplicationThreshold
            | RustGateName::FileGrowth
            | RustGateName::StaleBaseline
            | RustGateName::HealthFindings
            | RustGateName::UnusedFiles,
            _,
        ) => "The file has a finding of the gate.",
    }
}

pub(super) const fn rule_description(name: RustGateName) -> &'static str {
    name.description()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_error_message_names_the_reason() {
        let mut file = GateFile {
            path: "a.rs".to_owned(),
            error_count: 1,
            panicked: true,
            reason: None,
        };
        assert_eq!(
            file_message(RustGateName::ParseError, &file),
            "The file does not parse."
        );
        file.reason = Some(GateFileReason::NestingTooDeep);
        assert_eq!(
            file_message(RustGateName::ParseError, &file),
            "The file nests too deep to parse."
        );
    }
}
