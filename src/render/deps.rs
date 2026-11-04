//! The human and SARIF forms of the `deps` report.

use std::collections::BTreeSet;
use std::fmt::Write;

use fallow_report_protocol::GateOutcome;
use serde_json::{Value, json};

use super::human::{gate_line, plural};
use super::sarif::{document, level};
use crate::deps::report::{DepsReport, Finding, Rule, Severity};
use crate::gates::RustGateName;

/// The sections of the human form, in output order.
const SECTIONS: [(&str, &[Rule]); 4] = [
    (
        "Unused dependencies",
        &[
            Rule::UnusedDependency,
            Rule::UnusedDevDependency,
            Rule::UnusedBuildDependency,
            Rule::UnusedWorkspaceDependency,
        ],
    ),
    ("Move to [dev-dependencies]", &[Rule::MisplacedDependency]),
    (
        "Warnings",
        &[Rule::UnusedOptionalDependency, Rule::FeatureOnlyDependency],
    ),
    ("Redundant ignore entries", &[Rule::RedundantIgnore]),
];

/// The report for a person: the findings by section and manifest, then the gate.
pub fn human(report: &DepsReport) -> String {
    let mut out = String::new();
    let members = report.members;
    // A write to a `String` cannot fail.
    let _ = writeln!(
        out,
        "cargo fallow deps: {members} {}",
        plural(members, "member", "members")
    );
    if report.findings.is_empty() {
        out.push_str("No issues found\n");
    }
    for (title, rules) in SECTIONS {
        let rows: Vec<&Finding> = report
            .findings
            .iter()
            .filter(|finding| rules.contains(&finding.rule))
            .collect();
        if rows.is_empty() {
            continue;
        }
        let _ = writeln!(out, "\n{title} ({})", rows.len());
        let mut manifest = "";
        for row in rows {
            if row.manifest_path != manifest {
                manifest = &row.manifest_path;
                let _ = writeln!(out, "  {manifest}");
            }
            let line = row.line.map(|line| format!(":{line} ")).unwrap_or_default();
            let _ = writeln!(out, "    {line}{}", detail(row));
        }
    }
    for skipped in &report.skipped {
        let _ = writeln!(
            out,
            "\nNot judged: {} ({})",
            skipped.package, skipped.reason
        );
    }
    out.push_str("\nGates:\n");
    for (name, outcome) in &report.gate_outcomes {
        let _ = writeln!(out, "  {}: {}", name.as_str(), gate_line(outcome));
    }
    out
}

/// The text after the line number of one finding.
fn detail(row: &Finding) -> String {
    let mut text = row.dependency.clone();
    if let Some(package) = &row.package_name {
        let _ = write!(text, " (package `{package}`)");
    }
    match &row.platform {
        Some(platform) => {
            let _ = write!(text, " [target.'{platform}'.{}]", row.section);
        }
        None => {
            let _ = write!(text, " [{}]", row.section);
        }
    }
    match row.rule {
        Rule::MisplacedDependency => {
            let _ = write!(text, ", used only by {}", row.used_by.join(", "));
        }
        Rule::UnusedOptionalDependency => text.push_str(", optional, no code uses it"),
        Rule::FeatureOnlyDependency => {
            text.push_str(", no code uses it, a feature sets its features");
        }
        _ => {}
    }
    text
}

/// The report as a SARIF log. Each finding is a result.
pub fn sarif(report: &DepsReport) -> Value {
    let flagged: Vec<(RustGateName, &GateOutcome, &'static str)> = report
        .gate_outcomes
        .iter()
        .filter_map(|(name, outcome)| level(outcome).map(|level| (*name, outcome, level)))
        .collect();
    let error_level = flagged
        .iter()
        .find(|(name, _, _)| *name == RustGateName::ErrorSeverityFindings)
        .map_or("warning", |(_, _, level)| level);
    let results: Vec<Value> = report
        .findings
        .iter()
        .map(|finding| result(finding, error_level))
        .collect();
    let rules: Vec<Value> = report
        .findings
        .iter()
        .map(|finding| finding.rule)
        .collect::<BTreeSet<Rule>>()
        .into_iter()
        .map(|rule| {
            json!({
                "id": rule.as_str(),
                "shortDescription": { "text": rule.description() },
            })
        })
        .collect();
    document(&rules, &flagged, &results)
}

/// The findings in the shared shape. The id uses the manifest, the table, the
/// platform and the key, not the line.
pub fn findings(report: &DepsReport) -> Vec<crate::finding::Finding> {
    report.findings.iter().map(shared_finding).collect()
}

/// One finding in the shared shape.
pub fn shared_finding(finding: &Finding) -> crate::finding::Finding {
    use crate::finding::{Action, Category, Draft, Finding as Shared, Impact, Location};
    let (severity, impact) = match finding.severity {
        Severity::Error => (crate::finding::Severity::Error, Impact::Major),
        Severity::Warning => (crate::finding::Severity::Warning, Impact::Minor),
    };
    let platform = finding.platform.as_deref().unwrap_or_default();
    let package = finding.package.as_deref().unwrap_or_default();
    Shared::new(Draft {
        check: "deps",
        rule: finding.rule.as_str(),
        severity,
        message: message(finding),
        location: Location {
            path: finding.manifest_path.clone(),
            line: finding.line,
            column: None,
            end_line: None,
        },
        identity: &[
            &finding.manifest_path,
            package,
            &finding.section,
            platform,
            &finding.dependency,
        ],
        category: Category::BugRisk,
        impact,
    })
    .with_actions(
        finding
            .actions
            .iter()
            .map(|action| Action::new(action.kind, action.auto_fixable, &action.description))
            .collect(),
    )
}

/// The message of a finding: its first step, or the detail line.
fn message(finding: &Finding) -> String {
    finding
        .actions
        .first()
        .map_or_else(|| detail(finding), |action| action.description.clone())
}

fn result(finding: &Finding, error_level: &str) -> Value {
    let level = match finding.severity {
        Severity::Error => error_level,
        Severity::Warning => "note",
    };
    let message = message(finding);
    let mut location = json!({ "artifactLocation": { "uri": finding.manifest_path } });
    if let Some(line) = finding.line {
        location["region"] = json!({ "startLine": line });
    }
    json!({
        "ruleId": finding.rule.as_str(),
        "level": level,
        "message": { "text": message },
        "locations": [{ "physicalLocation": location }],
    })
}
