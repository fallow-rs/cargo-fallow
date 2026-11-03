//! The report of a `check` run: the sections of the checks that ran, in one
//! document.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use fallow_report_protocol::GateOutcome;
use fallow_report_protocol::exit_code::{EXIT_GATE_FAILED, EXIT_OK};
use serde::Serialize;
use serde_json::{Map, Value};

use super::CheckName;
use crate::cli::Format;
use crate::error::CliError;
use crate::finding::{Finding, codeclimate};
use crate::gates::RustGateName;
use crate::health::HealthScore;
use crate::render;
use crate::report::TOOL_VERSION;
use crate::section::{Section, pretty};
use crate::workspace::{Member, Workspace};

/// The version of the report shape of `check`. A change that removes a member
/// or changes its type increments it.
pub const SCHEMA_VERSION: u32 = 2;

/// The report of a `check` run. The field order is the wire order.
#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    /// The version of the report shape.
    pub schema_version: u32,
    /// The version of cargo-fallow.
    pub version: &'static str,
    /// Always `check`.
    pub kind: &'static str,
    /// Always `rust`.
    pub language: &'static str,
    /// The workspace members.
    pub workspace: WorkspaceSummary,
    /// The checks that ran, in run order.
    pub checks: Vec<CheckName>,
    /// The number of findings.
    pub total_issues: usize,
    /// The health score of the Rust code, when a check with a penalty ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthScore>,
    /// The findings of each check in the shared shape, in check order.
    pub findings: Vec<Finding>,
    /// The gate outcomes of each check that ran, keyed by gate name.
    pub gate_outcomes: BTreeMap<RustGateName, GateOutcome>,
    /// The report of each check that ran, keyed by check name. Each report is
    /// the JSON report of the command of the same name. The parse gate has no
    /// report of its own.
    pub sections: Map<String, Value>,
    /// The text and SARIF forms of each section. They are not part of the
    /// JSON report.
    #[serde(skip)]
    parts: Vec<(CheckName, Section)>,
}

/// The workspace part of the report.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceSummary {
    /// The members, sorted by name.
    pub members: Vec<Member>,
}

impl CheckReport {
    /// Joins the sections of one run.
    pub fn new(
        workspace: Workspace,
        parts: Vec<(CheckName, Section)>,
        health: Option<HealthScore>,
    ) -> Self {
        let mut gate_outcomes = BTreeMap::new();
        let mut findings = Vec::new();
        let mut sections = Map::new();
        for (name, section) in &parts {
            for (gate, outcome) in &section.gates {
                // Each gate name belongs to one check.
                debug_assert!(!gate_outcomes.contains_key(gate), "{gate:?} repeats");
                gate_outcomes.insert(*gate, outcome.clone());
            }
            findings.extend(section.findings.iter().cloned());
            if !section.json.is_null() {
                sections.insert(name.as_str().to_owned(), section.json.clone());
            }
        }
        Self {
            schema_version: SCHEMA_VERSION,
            version: TOOL_VERSION,
            kind: "check",
            language: "rust",
            workspace: WorkspaceSummary {
                members: workspace.members,
            },
            checks: parts.iter().map(|(name, _)| *name).collect(),
            total_issues: findings.len(),
            health,
            findings,
            gate_outcomes,
            sections,
            parts,
        }
    }

    /// True when an enforced gate of a check failed.
    pub fn fails_run(&self) -> bool {
        self.gate_outcomes.values().any(GateOutcome::fails_run)
    }

    /// Writes the report to stdout and returns the exit code.
    pub fn write(&self, format: Format) -> Result<u8, CliError> {
        let text = match format {
            Format::Human => self.human(),
            Format::Json => pretty(self)?,
            Format::Sarif => pretty(&self.sarif())?,
            Format::Codeclimate => pretty(&codeclimate(&self.findings))?,
        };
        render::write_stdout(text.trim_end()).map_err(|err| CliError::output(&err))?;
        Ok(if self.fails_run() {
            EXIT_GATE_FAILED
        } else {
            EXIT_OK
        })
    }

    /// The number of targets in all members.
    pub fn target_count(&self) -> usize {
        self.workspace
            .members
            .iter()
            .map(|member| member.targets.len())
            .sum()
    }

    /// The report for a person: a header, the health score, the text of each
    /// check without its gate block, and one gate block for the run.
    pub fn human(&self) -> String {
        let members = self.workspace.members.len();
        let targets = self.target_count();
        let mut text = format!(
            "cargo fallow check: {members} {}, {targets} {}\n",
            plural(members, "member", "members"),
            plural(targets, "target", "targets"),
        );
        let names: Vec<&str> = self.checks.iter().map(|name| name.as_str()).collect();
        let _ = writeln!(text, "Checks: {}", names.join(", "));
        if let Some(health) = &self.health {
            let _ = writeln!(
                text,
                "Health (rust): {:.1} ({}), formula version {}",
                health.score, health.grade, health.formula_version
            );
        }
        if self.total_issues == 0 {
            text.push_str("No issues found\n");
        } else {
            let _ = writeln!(
                text,
                "{} {}",
                self.total_issues,
                plural(self.total_issues, "issue", "issues")
            );
        }
        for (name, section) in &self.parts {
            if *name == CheckName::Parse {
                continue;
            }
            let body = without_gates(&section.human);
            let _ = write!(text, "\n== {} ==\n{}\n", name.as_str(), body.trim_end());
        }
        text.push('\n');
        text.push_str(&render::parse_human(&self.gate_outcomes));
        text
    }

    /// One SARIF log with one run: the results, the rules and the gate
    /// notifications of the SARIF log of each check.
    pub fn sarif(&self) -> Value {
        let mut log = render::empty_sarif();
        let mut rules: Vec<Value> = Vec::new();
        let mut results: Vec<Value> = Vec::new();
        let mut notes: Vec<Value> = Vec::new();
        let mut successful = true;
        for (_, section) in &self.parts {
            let run = &section.sarif["runs"][0];
            for rule in array(&run["tool"]["driver"]["rules"]) {
                if !rules.iter().any(|known| known["id"] == rule["id"]) {
                    rules.push(rule.clone());
                }
            }
            results.extend(array(&run["results"]).iter().cloned());
            let invocation = &run["invocations"][0];
            notes.extend(
                array(&invocation["toolExecutionNotifications"])
                    .iter()
                    .cloned(),
            );
            successful &= invocation["executionSuccessful"] != Value::Bool(false);
        }
        let run = &mut log["runs"][0];
        run["tool"]["driver"]["rules"] = Value::Array(rules);
        run["results"] = Value::Array(results);
        let invocation = &mut run["invocations"][0];
        invocation["executionSuccessful"] = Value::Bool(successful);
        if !notes.is_empty() {
            invocation["toolExecutionNotifications"] = Value::Array(notes);
        }
        log
    }
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

/// The text of a check without its gate block. The block starts at the last
/// line `Gates:` and goes to the end.
fn without_gates(text: &str) -> &str {
    match text.rfind("Gates:") {
        Some(start) if text[..start].is_empty() || text[..start].ends_with('\n') => &text[..start],
        _ => text,
    }
}

const fn plural<'a>(count: usize, one: &'a str, many: &'a str) -> &'a str {
    if count == 1 { one } else { many }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_block_goes_and_the_body_stays() {
        assert_eq!(without_gates("a\nb\n\nGates:\n  x: pass\n"), "a\nb\n\n");
        assert_eq!(without_gates("Gates:\n  x: pass"), "");
        assert_eq!(without_gates("no block"), "no block");
        assert_eq!(without_gates("text Gates: inline"), "text Gates: inline");
    }
}
