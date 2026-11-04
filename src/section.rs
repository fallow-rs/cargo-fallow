//! The output of one check in each format, with its findings and its gates.
//!
//! A single command writes one section. `check` joins the sections of the
//! checks that ran into one report.

use std::collections::BTreeMap;
use std::io;

use fallow_report_protocol::GateOutcome;
use fallow_report_protocol::exit_code::{EXIT_GATE_FAILED, EXIT_OK};
use serde_json::{Value, json};

use crate::cli::Format;
use crate::error::CliError;
use std::fmt::Write as _;

use crate::finding::{Category, Draft, Finding, Impact, Location, Severity, codeclimate};
use crate::gates::RustGateName;
use crate::render;
use crate::suppress::{STALE_RULE, Stale};

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
            Format::GithubAnnotations => Ok(render::github::annotations(
                &self.sarif,
                render::github::path_prefix(),
            )),
            Format::GithubSummary => Ok(render::github::summary(
                &render::github::Summary {
                    kind: self.json["kind"].as_str().unwrap_or("check"),
                    total_issues: self.findings_count(),
                    exit_code: self.exit_code(),
                    mutants_line: render::github::mutants_line(&self.json),
                    gates: &self.gates,
                    sarif: &self.sarif,
                },
                render::github::path_prefix(),
            )),
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
        Ok(self.exit_code())
    }

    /// The exit code of the section: 1 when an enforced gate failed.
    pub fn exit_code(&self) -> u8 {
        if self.fails() {
            EXIT_GATE_FAILED
        } else {
            EXIT_OK
        }
    }

    /// The `total_issues` of the JSON report, or the number of findings.
    fn findings_count(&self) -> usize {
        self.json["total_issues"]
            .as_u64()
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(self.findings.len())
    }
}

impl Section {
    /// Adds a `stale-suppression` warning for each comment in `stale`: to the
    /// shared findings, the JSON report (`stale_suppressions`), the human
    /// text and the SARIF log. The gates do not count them.
    pub fn add_stale_suppressions(&mut self, check: &'static str, stale: &[Stale]) {
        if stale.is_empty() {
            return;
        }
        let mut lines = String::new();
        let mut rows = Vec::new();
        for entry in stale {
            let kinds = entry.kinds.join(", ");
            let line = entry.line.to_string();
            let finding = Finding::new(Draft {
                check,
                rule: STALE_RULE,
                severity: Severity::Warning,
                message: format!("The suppression of `{kinds}` hides no finding."),
                location: Location::at(entry.path.clone(), entry.line, 1),
                identity: &[&entry.path, &kinds, &line],
                category: Category::Clarity,
                impact: Impact::Minor,
            })
            .with_actions(vec![crate::finding::Action::new(
                "remove-suppression",
                false,
                "Remove the suppression comment",
            )]);
            let _ = writeln!(
                lines,
                "  {STALE_RULE}: {}:{} ({kinds})",
                entry.path, entry.line
            );
            rows.push(json!({ "path": entry.path, "line": entry.line, "kinds": entry.kinds }));
            if let Some(results) = self.sarif["runs"][0]["results"].as_array_mut() {
                results.push(json!({
                    "ruleId": STALE_RULE,
                    "level": "warning",
                    "message": { "text": finding.message },
                    "locations": [{
                        "physicalLocation": {
                            "artifactLocation": { "uri": entry.path },
                            "region": { "startLine": entry.line, "startColumn": 1 },
                        }
                    }],
                }));
            }
            self.findings.push(finding);
        }
        if let Value::Object(map) = &mut self.json {
            map.insert("stale_suppressions".to_owned(), Value::Array(rows));
        }
        if let Some(rules) = self.sarif["runs"][0]["tool"]["driver"]["rules"].as_array_mut()
            && !rules.iter().any(|rule| rule["id"] == STALE_RULE)
        {
            rules.push(json!({
                "id": STALE_RULE,
                "shortDescription": { "text": "A suppression comment hides no finding." },
            }));
        }
        // The lines go above the gate block of the text.
        let at = if self.human.starts_with("Gates:") {
            Some(0)
        } else {
            self.human.find("\nGates:").map(|at| at + 1)
        };
        match at {
            Some(at) => self.human.insert_str(at, &lines),
            None => {
                if !self.human.is_empty() && !self.human.ends_with('\n') {
                    self.human.push('\n');
                }
                self.human.push_str(&lines);
            }
        }
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
