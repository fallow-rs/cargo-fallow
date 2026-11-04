//! The output of one check in each format, with its findings and its gates.
//!
//! A single command writes one section. `check` joins the sections of the
//! checks that ran into one report.

use std::collections::BTreeMap;
use std::io;

use fallow_report_protocol::GateOutcome;
use fallow_report_protocol::exit_code::{EXIT_GATE_FAILED, EXIT_OK};
use serde_json::Value;

use crate::cli::Format;
use crate::error::CliError;
use crate::finding::{Finding, codeclimate};
use crate::gates::RustGateName;
use crate::render;

/// The output of one check.
#[derive(Debug, Clone)]
pub struct Section {
    /// The text for a person.
    pub human: String,
    /// The JSON report of the check.
    pub json: Value,
    /// The SARIF log of the check.
    pub sarif: Value,
    /// The findings in the shared shape.
    pub findings: Vec<Finding>,
    /// The gate outcomes of the check.
    pub gates: BTreeMap<RustGateName, GateOutcome>,
}

impl Section {
    /// True when an enforced gate of the check failed.
    pub fn fails(&self) -> bool {
        self.gates.values().any(GateOutcome::fails_run)
    }

    /// The text of the section in `format`.
    pub fn render(&self, format: Format) -> Result<String, CliError> {
        match format {
            Format::Human => Ok(self.human.clone()),
            Format::Json => pretty(&self.json),
            Format::Sarif => pretty(&self.sarif),
            Format::Codeclimate => pretty(&codeclimate(&self.findings)),
            Format::Compact => Ok(render::compact::lines(&self.findings)),
            Format::Markdown => {
                let command = self.json["kind"].as_str().unwrap_or("check");
                Ok(render::markdown::render(&render::markdown::Document {
                    command,
                    notes: Vec::new(),
                    gates: &self.gates,
                    groups: vec![render::markdown::Group {
                        check: command,
                        findings: &self.findings,
                    }],
                }))
            }
        }
    }

    /// Writes the section to stdout and returns the exit code.
    pub fn write(&self, format: Format) -> Result<u8, CliError> {
        let text = self.render(format)?;
        render::write_report(text.trim_end()).map_err(|err| CliError::output(&err))?;
        Ok(if self.fails() {
            EXIT_GATE_FAILED
        } else {
            EXIT_OK
        })
    }
}

/// A value as pretty JSON.
pub fn pretty(value: &impl serde::Serialize) -> Result<String, CliError> {
    serde_json::to_string_pretty(value).map_err(|err| CliError::output(&io::Error::other(err)))
}

/// A value as a JSON tree.
pub fn to_value(value: &impl serde::Serialize) -> Result<Value, CliError> {
    serde_json::to_value(value).map_err(|err| CliError::output(&io::Error::other(err)))
}

#[cfg(test)]
mod tests {
    use fallow_report_protocol::GateStatus;

    use super::*;

    fn section(gates: &[(RustGateName, GateStatus, bool)]) -> Section {
        Section {
            human: String::new(),
            json: Value::Null,
            sarif: Value::Null,
            findings: Vec::new(),
            gates: gates
                .iter()
                .map(|(name, status, enforced)| (*name, GateOutcome::new(*status, *enforced)))
                .collect(),
        }
    }

    #[test]
    fn only_an_enforced_failure_fails_the_section() {
        assert!(!section(&[]).fails());
        assert!(!section(&[(RustGateName::FileGrowth, GateStatus::Fail, false)]).fails());
        assert!(!section(&[(RustGateName::FileGrowth, GateStatus::Pass, true)]).fails());
        assert!(
            section(&[
                (RustGateName::FileGrowth, GateStatus::Pass, true),
                (RustGateName::HealthFindings, GateStatus::Fail, true),
            ])
            .fails()
        );
    }

    #[test]
    fn codeclimate_of_a_section_without_findings_is_an_empty_array() {
        let text = section(&[]).render(Format::Codeclimate).unwrap_or_default();
        assert_eq!(text, "[]");
    }
}
