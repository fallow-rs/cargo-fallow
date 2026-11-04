//! The human and SARIF forms of the `mutants` reports.

use std::collections::BTreeMap;
use std::fmt::Write;

use fallow_report_protocol::GateOutcome;
use serde_json::{Value, json};

use super::human::{gate_line, plural};
use super::sarif::{document, level, rule_description};
use crate::gates::RustGateName;
use crate::mutation::report::{
    CoverageState, InspectReport, MapSource, MutantRow, Report, StopReason, survivor_message,
};
use crate::mutation::status::{Status, Tier};

/// The rule of each mutant that survived.
pub const SURVIVING_RULE: &str = "surviving-mutant";

/// The most source lines that the human form of inspect shows for each side. The
/// JSON form has all lines.
const MAX_CHANGE_LINES: usize = 20;

/// The sections with one line for each mutant.
const SECTIONS: [(Status, &str); 3] = [
    (Status::Survived, "Survived"),
    (Status::Flaky, "Flaky"),
    (Status::RuntimeError, "Runtime error"),
];

/// The report for a person: the summary first, then the survivors by file.
pub fn human(report: &Report) -> String {
    let mut out = String::new();
    // A write to a `String` cannot fail.
    let _ = writeln!(
        out,
        "cargo fallow mutants: {} {}",
        report.package, report.package_version
    );
    summary_lines(&mut out, report);
    for (status, title) in SECTIONS {
        section(&mut out, &report.mutants, status, title);
    }
    out.push_str("\nGates:\n");
    for (name, outcome) in &report.gate_outcomes {
        let _ = writeln!(out, "  {}: {}", name.as_str(), gate_line(outcome));
    }
    if report.total_issues > 0 {
        out.push_str("\nInspect a mutant: cargo fallow mutants --inspect <id>\n");
    }
    out
}

fn summary_lines(out: &mut String, report: &Report) {
    let s = &report.summary;
    let score = s.score.map_or_else(
        || "no score".to_owned(),
        |score| format!("score {score:.1}%"),
    );
    let counts = [
        (s.killed, "killed"),
        (s.survived, "survived"),
        (s.timeout, "timeout"),
        (s.flaky, "flaky"),
        (s.no_coverage, "no coverage"),
        (s.compile_error, "compile error"),
        (s.runtime_error, "runtime error"),
        (s.pending, "pending"),
    ]
    .iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, name)| format!("{count} {name}"))
    .collect::<Vec<_>>()
    .join(", ");
    let noun = plural(s.total, "mutant", "mutants");
    let _ = if counts.is_empty() {
        writeln!(out, "{} {noun}, {score}", s.total)
    } else {
        writeln!(out, "{} {noun}, {score}: {counts}", s.total)
    };
    if let Some(diff) = &report.in_diff {
        let base = diff.base.get(..12).unwrap_or(&diff.base);
        let _ = writeln!(
            out,
            "changed code against {} ({base}): {} changed {}, {} {} outside the diff",
            diff.reference,
            diff.changed_files,
            plural(diff.changed_files, "file", "files"),
            diff.mutants_outside,
            plural(diff.mutants_outside, "mutant", "mutants"),
        );
        if s.total == 0 {
            out.push_str("no mutant on a changed line\n");
        }
    }
    if let Some(reason) = report.stopped_by {
        let text = match reason {
            StopReason::TimeBudget => "the time budget ended",
            StopReason::LowMemory => "free memory stayed under the floor",
        };
        let _ = writeln!(out, "the run stopped early: {text}");
    }
    if report.guard.kills > 0 {
        let _ = writeln!(out, "memory guard kills: {}", report.guard.kills);
    }
    let selection = &report.selection;
    if selection.enabled {
        let _ = writeln!(
            out,
            "test selection: {} tests in the map, {} with no data, {} test runs, {} doctest runs",
            selection.tests_mapped,
            selection.tests_unmapped.len(),
            selection.tests_run,
            selection.doctests_run
        );
    }
    gap_lines(out, report);
    if selection.resumed > 0 {
        let _ = writeln!(
            out,
            "resumed: {} mutants came from the journal",
            selection.resumed
        );
    }
}

/// The facts that change how a person reads the verdicts.
fn gap_lines(out: &mut String, report: &Report) {
    if !report.baseline.regressions.is_empty() {
        let _ = writeln!(
            out,
            "the schema build changes the result of {} ({}), so {} ran as a rebuild",
            plural(report.baseline.regressions.len(), "a test", "these tests"),
            report.baseline.regressions.join(", "),
            plural(
                report.schema.moved_to_rebuild,
                "1 mutant",
                &format!("{} mutants", report.schema.moved_to_rebuild)
            ),
        );
    }
    let alone = &report.selection.tests_fail_alone;
    if !alone.is_empty() {
        let _ = writeln!(
            out,
            "these tests fail alone with no mutant, so they decide nothing: {}",
            alone.join(", ")
        );
    }
    if let Some(workspace) = &report.workspace_tests {
        let _ = writeln!(
            out,
            "workspace tests: {} of {} remaining {} detected",
            workspace.detected,
            workspace.mutants_run,
            plural(workspace.mutants_run, "mutant", "mutants"),
        );
    }
    let dependents = &report.gaps.untested_dependents;
    if !dependents.is_empty() {
        let _ = writeln!(
            out,
            "the tests of these members that depend on this package did not run: {}. \
             Pass `--test-workspace` to run them",
            dependents.join(", ")
        );
    }
}

fn section(out: &mut String, rows: &[MutantRow], status: Status, title: &str) {
    let mut by_file: BTreeMap<&str, Vec<&MutantRow>> = BTreeMap::new();
    let mut count = 0;
    for row in rows.iter().filter(|row| row.status == status) {
        by_file.entry(&row.file).or_default().push(row);
        count += 1;
    }
    if count == 0 {
        return;
    }
    let _ = writeln!(out, "\n{title} ({count})");
    for (file, mut rows) in by_file {
        // A file can have mutants of both tiers, and a person reads by position.
        rows.sort_by_key(|row| (row.line, row.column));
        let _ = writeln!(out, "  {file}");
        for row in rows {
            let _ = write!(
                out,
                "    :{}:{} {} `{}` -> `{}` [{}]",
                row.line, row.column, row.operator, row.original, row.replacement, row.id
            );
            if let Some(reason) = &row.reason {
                let _ = write!(out, ": {reason}");
            }
            out.push('\n');
        }
    }
}

/// The report as a SARIF log. Each survivor is a result at its line. The log gives no
/// column: the report counts bytes, and a SARIF column counts UTF-16 units.
pub fn sarif(report: &Report) -> Value {
    let flagged: Vec<(RustGateName, &GateOutcome, &'static str)> = report
        .gate_outcomes
        .iter()
        .filter_map(|(name, outcome)| level(outcome).map(|level| (*name, outcome, level)))
        .collect();
    let name = RustGateName::SurvivingMutants;
    let result_level = flagged
        .iter()
        .find(|(gate, _, _)| *gate == name)
        .map_or("warning", |(_, _, level)| level);
    let results: Vec<Value> = report
        .mutants
        .iter()
        .filter(|row| row.status == Status::Survived)
        .map(|row| {
            json!({
                "ruleId": name.as_str(),
                "level": result_level,
                "message": { "text": survivor_message(row) },
                "locations": [{
                    "physicalLocation": {
                        "artifactLocation": { "uri": row.file },
                        "region": { "startLine": row.line },
                    }
                }],
                "partialFingerprints": { "mutantId": row.id },
            })
        })
        .collect();
    let rules: Vec<Value> = if results.is_empty() {
        Vec::new()
    } else {
        vec![json!({
            "id": name.as_str(),
            "shortDescription": { "text": rule_description(name) },
        })]
    };
    document(&rules, &flagged, &results)
}

/// The survivors in the shared finding shape. The id is the hash of the mutant
/// id, which stays the same while the source of the mutant stays the same.
pub fn findings(report: &Report) -> Vec<crate::finding::Finding> {
    use crate::finding::{Category, Draft, Finding, Impact, Location, Severity};
    report
        .mutants
        .iter()
        .filter(|row| row.status == Status::Survived)
        .map(|row| {
            Finding::new(Draft {
                check: "mutants",
                rule: SURVIVING_RULE,
                severity: Severity::Error,
                message: survivor_message(row),
                location: Location {
                    path: row.file.clone(),
                    line: u32::try_from(row.line).ok(),
                    column: None,
                    end_line: None,
                },
                identity: &[&row.id],
                category: Category::BugRisk,
                impact: Impact::Major,
            })
        })
        .collect()
}

/// The section of a report.
pub fn report_section(report: &Report) -> Result<crate::section::Section, crate::error::CliError> {
    Ok(crate::section::Section {
        human: human(report),
        json: crate::section::to_value(report)?,
        sarif: sarif(report),
        findings: findings(report),
        gates: report.gate_outcomes.clone(),
    })
}

/// The inspect report for a person.
pub fn inspect_human(report: &InspectReport) -> String {
    let mutant = &report.mutant;
    let mut out = String::new();
    let _ = writeln!(out, "mutant {}", mutant.id);
    let _ = writeln!(out, "  {}:{}:{}", mutant.file, mutant.line, mutant.column);
    let _ = writeln!(out, "  operator: {}", mutant.operator);
    let tier = match mutant.tier {
        Tier::Schema => "schema",
        Tier::Rebuild => "rebuild",
    };
    let _ = writeln!(out, "  tier: {tier}");
    if mutant.under_cfg {
        out.push_str("  a `cfg` attribute is on an enclosing item\n");
    }
    out.push_str("\nChange\n");
    change_side(&mut out, '-', &mutant.before);
    change_side(&mut out, '+', &mutant.after);
    coverage_lines(&mut out, report);
    out
}

fn change_side(out: &mut String, sign: char, text: &str) {
    let total = text.lines().count();
    for line in text.lines().take(MAX_CHANGE_LINES) {
        let _ = writeln!(out, "  {sign} {line}");
    }
    if total > MAX_CHANGE_LINES {
        let _ = writeln!(out, "  {sign} ... {} more lines", total - MAX_CHANGE_LINES);
    }
}

fn coverage_lines(out: &mut String, report: &InspectReport) {
    let coverage = &report.coverage;
    let source = match coverage.map {
        MapSource::Built => "built by this command",
        MapSource::Cached => "read from the cache",
        MapSource::None => "not available",
    };
    let _ = writeln!(out, "\nTests (coverage map {source})");
    match coverage.state {
        CoverageState::Covered => {
            for test in &coverage.tests {
                let _ = writeln!(out, "  {test}");
            }
        }
        CoverageState::NotCovered => out.push_str("  No test executes these lines.\n"),
        CoverageState::NoLineData => {
            out.push_str("  The map has no data on these lines, so a run has each test.\n");
        }
        CoverageState::ConstantContext => {
            out.push_str("  The compiler can evaluate this code, so a run has each test.\n");
        }
    }
    if !coverage.tests_without_data.is_empty() {
        out.push_str("\nTests with no coverage data (a run has each of them)\n");
        for test in &coverage.tests_without_data {
            let _ = writeln!(out, "  {test}");
        }
    }
    out.push_str("\nDoctests have no coverage data. A run has them when no other test fails.\n");
}
