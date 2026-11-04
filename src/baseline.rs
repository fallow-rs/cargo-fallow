//! Baselines: the findings of an earlier run that a later run does not report.
//!
//! `--save-baseline <PATH>` writes the id, the check and the rule of each
//! finding of a run. `--baseline <PATH>` reads the file, and the report then
//! leaves out each finding whose id is in it. A file entry with no finding in
//! the run is stale. The ids are the stable finding ids, so an edit above a
//! finding does not change its id. A clone group of `dupes` has one entry: the
//! fingerprint of the group.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use fallow_report_protocol::{GateOutcome, GateStatus};
use serde::{Deserialize, Serialize};

use crate::error::{CliError, ErrorCode};
use crate::finding::Finding;

/// The version of the baseline file shape.
pub const SCHEMA_VERSION: u32 = 1;

/// The key scheme of the ids in a baseline file.
pub const IDENTITY: &str = "cargo-fallow-finding-id-v1";

/// One finding in a baseline file.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[schemars(rename = "BaselineEntry")]
pub struct Entry {
    /// The check that found it, for example `deps`.
    pub check: String,
    /// The rule, for example `unused-dependency`.
    pub rule: String,
    /// The stable id of the finding.
    pub id: String,
}

impl Entry {
    /// The entry of a finding in the shared shape.
    pub fn of(finding: &Finding) -> Self {
        Self {
            check: finding.check.to_owned(),
            rule: finding.rule.clone(),
            id: finding.id.clone(),
        }
    }
}

/// The baseline file. The field order is the wire order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "BaselineFile")]
pub struct File {
    /// The version of the file shape.
    pub schema_version: u32,
    /// The command that saved the file.
    pub kind: String,
    /// Always `rust`.
    pub language: String,
    /// The key scheme of the ids.
    pub identity: String,
    /// The findings, sorted by check, rule and id.
    pub findings: Vec<Entry>,
}

/// A loaded baseline and the entries that the run matched.
#[derive(Debug, Default)]
pub struct Loaded {
    /// The path of the file, as the user gave it.
    pub path: String,
    /// The ids of the file.
    ids: BTreeSet<String>,
    /// The entries of the file.
    entries: Vec<Entry>,
    /// The ids that a finding of the run matched.
    matched: Mutex<BTreeSet<String>>,
}

impl Loaded {
    /// Reads a baseline file. A missing file, a file that is not JSON, or a
    /// file of another shape is an error.
    pub fn read(path: &Path) -> Result<Self, CliError> {
        let display = path.display().to_string();
        let text = fs::read_to_string(path).map_err(|err| {
            CliError::new(
                ErrorCode::InvalidArgument,
                format!("cannot read the baseline `{display}`: {err}"),
            )
            .with_help("Give the path of a file that `--save-baseline` wrote.")
        })?;
        let file: File = serde_json::from_str(&text).map_err(|err| {
            CliError::new(
                ErrorCode::InvalidArgument,
                format!("the baseline `{display}` is not a cargo-fallow baseline: {err}"),
            )
            .with_help("Write the file again with `--save-baseline`.")
        })?;
        if file.schema_version != SCHEMA_VERSION || file.identity != IDENTITY {
            return Err(CliError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "the baseline `{display}` has version {} and key scheme `{}`, but this version reads version {SCHEMA_VERSION} and `{IDENTITY}`",
                    file.schema_version, file.identity
                ),
            )
            .with_help("Write the file again with `--save-baseline`."));
        }
        Ok(Self {
            path: display,
            ids: file.findings.iter().map(|entry| entry.id.clone()).collect(),
            entries: file.findings,
            matched: Mutex::default(),
        })
    }

    /// True when the baseline holds `id`. The id then counts as matched.
    pub fn absorbs(&self, id: &str) -> bool {
        if !self.ids.contains(id) {
            return false;
        }
        self.matched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_owned());
        true
    }

    /// The number of file entries that a finding matched.
    pub fn matched(&self) -> usize {
        self.matched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// The entries of the checks in `checks` that no finding matched.
    pub fn stale(&self, checks: &[&str]) -> Vec<Entry> {
        let matched = self.matched.lock().unwrap_or_else(PoisonError::into_inner);
        self.entries
            .iter()
            .filter(|entry| checks.contains(&entry.check.as_str()))
            .filter(|entry| !matched.contains(&entry.id))
            .cloned()
            .collect()
    }
}

/// The `baseline` member of a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[schemars(rename = "BaselineSummary")]
pub struct Summary {
    /// The path of the file.
    pub path: String,
    /// The file entries that a finding of the run matched. The report leaves
    /// out these findings.
    pub matched: usize,
    /// The file entries of the checks that ran with no finding in the run.
    /// A run with `--changed-since` does not judge them, so the list is empty.
    pub stale: Vec<Entry>,
}

/// The `stale-baseline` gate. A run that cannot judge the entries skips it.
pub fn stale_gate(stale: usize, judged: bool, enforced: bool) -> GateOutcome {
    if !judged {
        return GateOutcome::new(GateStatus::Skipped, enforced);
    }
    crate::structure::report::count_gate(stale, enforced)
}

/// The entries that a run collects for `--save-baseline`.
#[derive(Debug, Default)]
pub struct Recorder {
    /// The file to write.
    pub path: PathBuf,
    entries: Mutex<BTreeSet<Entry>>,
}

impl Recorder {
    /// A recorder that writes to `path`.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            entries: Mutex::default(),
        }
    }

    /// Adds the entry of one finding.
    pub fn record(&self, entry: Entry) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(entry);
    }

    /// Writes the file. `kind` is the command of the run.
    pub fn write(&self, kind: &str) -> Result<(), CliError> {
        let findings: Vec<Entry> = self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect();
        let file = File {
            schema_version: SCHEMA_VERSION,
            kind: kind.to_owned(),
            language: "rust".to_owned(),
            identity: IDENTITY.to_owned(),
            findings,
        };
        let mut text = serde_json::to_string_pretty(&file)
            .map_err(|err| CliError::output(&std::io::Error::other(err)))?;
        text.push('\n');
        fs::write(&self.path, text).map_err(|err| {
            CliError::new(
                ErrorCode::OutputFailed,
                format!("cannot write the baseline `{}`: {err}", self.path.display()),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(check: &str, id: &str) -> Entry {
        Entry {
            check: check.to_owned(),
            rule: "r".to_owned(),
            id: id.to_owned(),
        }
    }

    fn loaded(entries: Vec<Entry>) -> Loaded {
        Loaded {
            path: "b.json".to_owned(),
            ids: entries.iter().map(|entry| entry.id.clone()).collect(),
            entries,
            matched: Mutex::default(),
        }
    }

    #[test]
    fn an_absorbed_id_is_matched_and_the_rest_of_a_check_that_ran_is_stale() {
        let baseline = loaded(vec![
            entry("deps", "a"),
            entry("deps", "b"),
            entry("dupes", "c"),
        ]);
        assert!(baseline.absorbs("a"));
        assert!(baseline.absorbs("a"));
        assert!(!baseline.absorbs("z"));
        assert_eq!(baseline.matched(), 1);
        assert_eq!(baseline.stale(&["deps"]), [entry("deps", "b")]);
        assert_eq!(baseline.stale(&["deps", "dupes"]).len(), 2);
        assert!(baseline.stale(&["size"]).is_empty());
    }

    #[test]
    fn the_stale_gate_skips_a_run_that_cannot_judge() {
        assert_eq!(stale_gate(3, false, true).status, GateStatus::Skipped);
        assert_eq!(stale_gate(0, true, true).status, GateStatus::Pass);
        assert!(stale_gate(1, true, true).fails_run());
        assert!(!stale_gate(1, true, false).fails_run());
    }
}
