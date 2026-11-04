//! The gates of a `check` run.
//!
//! [`RustGateName`] holds the name of each gate of each command. The gate names
//! use the same spelling as `GateName` in fallow. The two lists stay equal by
//! convention until the protocol crate owns the names. There are two
//! exceptions: the JavaScript tool has no gate on mutation results
//! (`surviving-mutants`), and it counts an unused file in
//! `error-severity-findings`, not in its own gate (`unused-files`).

use std::collections::BTreeMap;
use std::fs;

use fallow_report_protocol::{GateFile, GateFileReason, GateOutcome, GateStatus};
use serde::Serialize;

use crate::parse::{Edition, ParseOutcome, parse_source};
use crate::workspace::{self, Workspace};

/// The name of a gate, as it appears in `gate_outcomes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RustGateName {
    /// `boundaries`: a rule found a violation.
    BoundaryViolations,
    /// `dupes`: the run found a clone group.
    DuplicationFindings,
    /// `dupes --threshold`: the duplicated lines are over the percentage.
    DuplicationThreshold,
    /// Fails when the run has a finding with the `error` severity.
    ErrorSeverityFindings,
    /// `size --base` or `size --staged`: a file over the line limit grew.
    FileGrowth,
    /// `complexity`: a function is over a limit.
    HealthFindings,
    /// Fails when a target root file does not parse.
    ParseError,
    /// Fails when a mutant survived the tests of a `mutants` run.
    SurvivingMutants,
    /// `files`: a `.rs` file that no target compiles.
    UnusedFiles,
    /// Fails when a `visibility` run has a finding.
    VisibilityFindings,
}

impl RustGateName {
    /// Every gate name, in wire order.
    #[cfg(test)]
    pub const ALL: [Self; 10] = [
        Self::BoundaryViolations,
        Self::DuplicationFindings,
        Self::DuplicationThreshold,
        Self::ErrorSeverityFindings,
        Self::FileGrowth,
        Self::HealthFindings,
        Self::ParseError,
        Self::SurvivingMutants,
        Self::UnusedFiles,
        Self::VisibilityFindings,
    ];

    /// The wire spelling of the name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BoundaryViolations => "boundary-violations",
            Self::DuplicationFindings => "duplication-findings",
            Self::DuplicationThreshold => "duplication-threshold",
            Self::FileGrowth => "file-growth",
            Self::HealthFindings => "health-findings",
            Self::ErrorSeverityFindings => "error-severity-findings",
            Self::ParseError => "parse-error",
            Self::SurvivingMutants => "surviving-mutants",
            Self::UnusedFiles => "unused-files",
            Self::VisibilityFindings => "visibility-findings",
        }
    }

    /// One sentence that tells what a failure of the gate means.
    pub const fn description(self) -> &'static str {
        match self {
            Self::BoundaryViolations => "A boundary rule found a violation.",
            Self::DuplicationFindings => "The run found a clone group.",
            Self::DuplicationThreshold => {
                "The duplicated lines are over the percentage of all lines."
            }
            Self::ErrorSeverityFindings => "The run has a finding with the error severity.",
            Self::FileGrowth => "A file over the line limit grew.",
            Self::HealthFindings => "A function is over a complexity limit.",
            Self::ParseError => "A target root file does not parse.",
            Self::SurvivingMutants => "A mutant survived: each test passed with the change.",
            Self::UnusedFiles => "A `.rs` file in a target folder is not part of a target.",
            Self::VisibilityFindings => "An item has a larger visibility than its uses need.",
        }
    }
}

/// The gate outcomes of a run and the target root files that no gate judged.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Evaluation {
    /// The gate outcomes, keyed by gate name.
    pub outcomes: BTreeMap<RustGateName, GateOutcome>,
    /// The target root files that could not be read, sorted by path.
    pub unreadable: Vec<UnreadableFile>,
}

/// A target root file that could not be read as UTF-8 text.
///
/// The `parse-error` gate does not judge such a file. The gate contract has
/// no reason for it, and fallow reports the same case as a
/// `source-read-failure` diagnostic, not as a gate file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableFile {
    /// The file path, relative to the workspace root, with `/` separators.
    pub path: String,
    /// The I/O or UTF-8 error.
    pub error: String,
}

/// Computes the `parse-error` gate for the workspace. `enforced` is true when
/// a target root file that does not parse fails the run.
pub fn evaluate(workspace: &Workspace, enforced: bool) -> Evaluation {
    let (parse_outcome, unreadable) = parse_error(workspace, enforced);
    Evaluation {
        outcomes: BTreeMap::from([(RustGateName::ParseError, parse_outcome)]),
        unreadable,
    }
}

/// Parses the root file of each target.
fn parse_error(workspace: &Workspace, enforced: bool) -> (GateOutcome, Vec<UnreadableFile>) {
    let mut roots: Vec<(&str, &str)> = workspace
        .members
        .iter()
        .flat_map(|member| {
            member
                .targets
                .iter()
                .map(|target| (target.src_path.as_str(), target.edition.as_str()))
        })
        .collect();
    roots.sort_unstable();
    roots.dedup_by_key(|root| root.0);
    let mut files = Vec::new();
    let mut unreadable = Vec::new();
    for (path, edition) in roots {
        match fs::read_to_string(workspace::absolute_path(&workspace.root, path)) {
            Ok(source) => files.extend(gate_file(
                path,
                parse_source(&source, parse_edition(edition)),
            )),
            Err(err) => unreadable.push(UnreadableFile {
                path: path.to_owned(),
                error: err.to_string(),
            }),
        }
    }
    let status = if files.is_empty() {
        GateStatus::Pass
    } else {
        GateStatus::Fail
    };
    (GateOutcome::with_files(status, enforced, files), unreadable)
}

/// The parser edition for the edition name in the Cargo metadata. A name that
/// this version of the parser does not know gets its newest edition.
pub fn parse_edition(name: &str) -> Edition {
    match name {
        "2015" => Edition::Edition2015,
        "2018" => Edition::Edition2018,
        "2021" => Edition::Edition2021,
        "2024" => Edition::Edition2024,
        _ => Edition::CURRENT,
    }
}

/// The `parse-error` gate item of one file, or `None` when the gate does not
/// count the file.
fn gate_file(path: &str, parsed: ParseOutcome) -> Option<GateFile> {
    match parsed {
        ParseOutcome::Parsed => None,
        // The parser recovers from an error and reads the whole file.
        ParseOutcome::Failed { error_count } => Some(GateFile {
            path: path.to_owned(),
            error_count,
            panicked: false,
            reason: None,
        }),
        ParseOutcome::Panicked => Some(GateFile {
            path: path.to_owned(),
            error_count: 1,
            panicked: true,
            reason: None,
        }),
        // The contract sets the count and the parser outcome to placeholders
        // when `reason` is set.
        ParseOutcome::TooDeep => Some(GateFile {
            path: path.to_owned(),
            error_count: 1,
            panicked: true,
            reason: Some(GateFileReason::NestingTooDeep),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_file_follows_the_contract_for_each_parse_result() {
        assert_eq!(gate_file("a.rs", ParseOutcome::Parsed), None);
        assert_eq!(
            gate_file("a.rs", ParseOutcome::Failed { error_count: 2 }),
            Some(GateFile {
                path: "a.rs".to_owned(),
                error_count: 2,
                panicked: false,
                reason: None,
            })
        );
        assert_eq!(
            gate_file("a.rs", ParseOutcome::Panicked),
            Some(GateFile {
                path: "a.rs".to_owned(),
                error_count: 1,
                panicked: true,
                reason: None,
            })
        );
        assert_eq!(
            gate_file("a.rs", ParseOutcome::TooDeep),
            Some(GateFile {
                path: "a.rs".to_owned(),
                error_count: 1,
                panicked: true,
                reason: Some(GateFileReason::NestingTooDeep),
            })
        );
    }

    #[test]
    fn edition_names_map_to_the_parser_editions() {
        assert_eq!(parse_edition("2015"), Edition::Edition2015);
        assert_eq!(parse_edition("2018"), Edition::Edition2018);
        assert_eq!(parse_edition("2021"), Edition::Edition2021);
        assert_eq!(parse_edition("2024"), Edition::Edition2024);
        assert_eq!(parse_edition("2099"), Edition::CURRENT);
    }

    #[test]
    fn wire_names_match_serde_and_keep_the_wire_order() {
        for name in RustGateName::ALL {
            let wire = serde_json::to_value(name).unwrap_or_default();
            assert_eq!(wire, name.as_str());
        }
        let names: Vec<&str> = RustGateName::ALL.iter().map(|name| name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted);
    }
}
