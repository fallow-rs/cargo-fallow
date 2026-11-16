//! Baselines: the findings of an earlier run that a later run does not report.
//!
//! `--save-baseline <PATH>` writes the id, the check and the rule of each
//! finding of a run. `--baseline <PATH>` reads the file, and the report then
//! leaves out each finding whose id is in it. A file entry with no finding in
//! the run is stale. The ids are the stable finding ids, so an edit above a
//! finding does not change its id. A clone group of `dupes` has one entry: the
//! fingerprint of the group, with the lines and the functions of each copy.
//! An edit inside a saved clone changes its fingerprint, so a group with a new
//! fingerprint still matches a saved group when its copies are in the same
//! functions (see [`Loaded::absorbs_edited_clones`]).

use std::collections::{BTreeMap, BTreeSet};
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
    /// The lines of each copy of a `dupes` clone group. The other checks
    /// leave the field out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instances: Vec<Region>,
}

impl Entry {
    /// The entry of a finding in the shared shape.
    pub fn of(finding: &Finding) -> Self {
        Self::new(finding.check, &finding.rule, &finding.id)
    }

    /// An entry with no clone copies.
    pub fn new(check: &str, rule: &str, id: &str) -> Self {
        Self {
            check: check.to_owned(),
            rule: rule.to_owned(),
            id: id.to_owned(),
            instances: Vec::new(),
        }
    }
}

/// The place of one copy of a saved clone group.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[schemars(rename = "BaselineRegion")]
pub struct Region {
    /// The file, relative to the workspace root.
    pub file: String,
    /// The first line, from 1.
    pub start_line: u32,
    /// The last line.
    pub end_line: u32,
    /// The sorted names of the functions that share a line with the copy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub functions: Vec<String>,
}

impl Region {
    /// The number of lines.
    const fn lines(&self) -> u32 {
        self.end_line.saturating_sub(self.start_line) + 1
    }

    /// The number of lines that `self` and `other` share.
    fn shared_lines(&self, other: &Self) -> u32 {
        if self.file != other.file {
            return 0;
        }
        let start = self.start_line.max(other.start_line);
        let end = self.end_line.min(other.end_line);
        if end < start { 0 } else { end - start + 1 }
    }
}

/// True when the copy `found` is the saved copy `saved` after an edit. The
/// file must be the same. A copy in functions must touch only functions that
/// the saved copy touched, so an edit above the copy does not matter. A copy
/// outside each function must share at least half of its lines with the
/// saved copy.
fn same_copy(saved: &Region, found: &Region) -> bool {
    if saved.file != found.file {
        return false;
    }
    if found.functions.is_empty() {
        return saved.shared_lines(found) * 2 >= found.lines();
    }
    found
        .functions
        .iter()
        .all(|name| saved.functions.contains(name))
}

/// The distance of the copies of a clone group from the copies of a saved
/// group: for each copy, the line distance from the nearest saved copy that
/// [`same_copy`] accepts, plus the change of the line count.
fn distance(saved: &[Region], found: &[Region]) -> u64 {
    found
        .iter()
        .map(|copy| {
            saved
                .iter()
                .filter(|old| same_copy(old, copy))
                .map(|old| {
                    u64::from(old.start_line.abs_diff(copy.start_line))
                        + u64::from(old.lines().abs_diff(copy.lines()))
                })
                .min()
                .unwrap_or(u64::MAX / 4)
        })
        .sum()
}

/// The line count of the longest copy.
fn span(copies: &[Region]) -> u32 {
    copies.iter().map(Region::lines).max().unwrap_or_default()
}

/// The saved ids, with the number of saved copies of each.
fn saved_ids(entries: &[Entry]) -> BTreeMap<String, usize> {
    entries
        .iter()
        .map(|entry| (entry.id.clone(), entry.instances.len()))
        .collect()
}

/// True when each copy of `found` is a copy of `saved` after an edit. A clone
/// needs two copies, so a group with fewer is never covered.
fn covers(saved: &[Region], found: &[Region]) -> bool {
    found.len() >= 2
        && found
            .iter()
            .all(|copy| saved.iter().any(|old| same_copy(old, copy)))
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
    /// The ids of the file, with the number of saved copies of a clone
    /// group. The number is 0 for an entry with no copies.
    ids: BTreeMap<String, usize>,
    /// The entries of the file.
    entries: Vec<Entry>,
    /// The ids that a finding of the run matched.
    matched: Mutex<BTreeSet<String>>,
    /// The ids of the saved clone groups that an edited clone group matched.
    edited: Mutex<BTreeSet<String>>,
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
            ids: saved_ids(&file.findings),
            entries: file.findings,
            matched: Mutex::default(),
            edited: Mutex::default(),
        })
    }

    /// True when the baseline holds `id`. The id then counts as matched.
    pub fn absorbs(&self, id: &str) -> bool {
        if !self.ids.contains_key(id) {
            return false;
        }
        self.matched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_owned());
        true
    }

    /// True when the baseline holds the entry. A clone group with more
    /// copies than the saved group is new duplication, also with an equal
    /// fingerprint, because the fingerprint reads the tokens of one copy.
    pub fn absorbs_entry(&self, entry: &Entry) -> bool {
        let saved = self.ids.get(&entry.id).copied().unwrap_or_default();
        if saved > 0 && entry.instances.len() > saved {
            return false;
        }
        self.absorbs(&entry.id)
    }

    /// The clone groups, by the copies of each group, that a saved clone
    /// group matches after an edit. A group matches a saved group that no
    /// fingerprint matched, when each copy of the group is a copy of the
    /// saved group after an edit (see [`same_copy`]). The group with the
    /// least distance (see [`distance`]) matches. An edit can split a clone,
    /// so a further group matches while the matched groups together have no
    /// more lines than the saved group. The saved group then counts as
    /// matched. An edit can also join two clones, so each saved group that no
    /// group matched counts as matched when it covers a matched group.
    ///
    /// Call it after [`Self::absorbs_entry`] has seen each group of the run.
    pub fn absorbs_edited_clones(&self, groups: &[Vec<Region>]) -> Vec<bool> {
        let mut pairs: Vec<(u64, usize, usize)> = {
            let matched = self.matched.lock().unwrap_or_else(PoisonError::into_inner);
            self.entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| !entry.instances.is_empty() && !matched.contains(&entry.id))
                .flat_map(|(saved, entry)| {
                    groups
                        .iter()
                        .enumerate()
                        .filter(|(_, copies)| covers(&entry.instances, copies))
                        .map(move |(found, copies)| {
                            (distance(&entry.instances, copies), saved, found)
                        })
                })
                .collect()
        };
        pairs.sort_unstable();
        let mut absorbed = vec![false; groups.len()];
        let mut used: BTreeMap<usize, u32> = BTreeMap::new();
        let mut edited = self.edited.lock().unwrap_or_else(PoisonError::into_inner);
        for &(_, saved, found) in &pairs {
            if absorbed[found] {
                continue;
            }
            let lines = span(&groups[found]);
            let total = match used.get(&saved) {
                None => lines,
                Some(before) if before + lines <= span(&self.entries[saved].instances) => {
                    before + lines
                }
                Some(_) => continue,
            };
            used.insert(saved, total);
            absorbed[found] = true;
            edited.insert(self.entries[saved].id.clone());
        }
        for (_, saved, found) in pairs {
            if absorbed[found] && !used.contains_key(&saved) {
                edited.insert(self.entries[saved].id.clone());
            }
        }
        absorbed
    }

    /// The number of file entries that a finding matched.
    pub fn matched(&self) -> usize {
        let matched = self.matched.lock().unwrap_or_else(PoisonError::into_inner);
        let edited = self.edited.lock().unwrap_or_else(PoisonError::into_inner);
        matched.union(&edited).count()
    }

    /// The entries of the checks in `checks` that no finding matched.
    pub fn stale(&self, checks: &[&str]) -> Vec<Entry> {
        let matched = self.matched.lock().unwrap_or_else(PoisonError::into_inner);
        let edited = self.edited.lock().unwrap_or_else(PoisonError::into_inner);
        self.entries
            .iter()
            .filter(|entry| checks.contains(&entry.check.as_str()))
            .filter(|entry| !matched.contains(&entry.id) && !edited.contains(&entry.id))
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
        Entry::new(check, "r", id)
    }

    fn region(file: &str, start_line: u32, end_line: u32) -> Region {
        Region {
            file: file.to_owned(),
            start_line,
            end_line,
            functions: Vec::new(),
        }
    }

    fn in_functions(file: &str, start_line: u32, functions: &[&str]) -> Region {
        Region {
            functions: functions.iter().map(|name| (*name).to_owned()).collect(),
            ..region(file, start_line, start_line + 19)
        }
    }

    fn clone_entry(id: &str, instances: Vec<Region>) -> Entry {
        Entry {
            instances,
            ..Entry::new("dupes", "code-duplication", id)
        }
    }

    fn loaded(entries: Vec<Entry>) -> Loaded {
        Loaded {
            path: "b.json".to_owned(),
            ids: saved_ids(&entries),
            entries,
            matched: Mutex::default(),
            edited: Mutex::default(),
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

    #[test]
    fn an_edited_clone_matches_a_saved_group_that_holds_half_of_each_copy() {
        let saved = vec![region("a.rs", 10, 29), region("b.rs", 50, 69)];
        let grown = [region("a.rs", 12, 37), region("b.rs", 52, 77)];
        assert!(covers(&saved, &grown));
        let shrunk = [region("a.rs", 15, 20), region("b.rs", 55, 60)];
        assert!(covers(&saved, &shrunk));
        // A new function after the saved copy: most lines are new.
        let appended = [region("a.rs", 10, 50), region("b.rs", 50, 90)];
        assert!(!covers(&saved, &appended));
        let other_file = [region("a.rs", 10, 29), region("c.rs", 50, 69)];
        assert!(!covers(&saved, &other_file));
        let third_copy = [
            region("a.rs", 10, 29),
            region("b.rs", 50, 69),
            region("c.rs", 1, 20),
        ];
        assert!(!covers(&saved, &third_copy));
        assert!(!covers(&saved, &[region("a.rs", 10, 29)]));
    }

    #[test]
    fn a_copy_in_functions_matches_by_the_functions_and_not_by_the_lines() {
        let saved = vec![
            in_functions("a.rs", 10, &["Map::parse"]),
            in_functions("a.rs", 400, &["Map::parse_lines"]),
        ];
        // An edit above moved both copies far from the saved lines.
        let moved = [
            in_functions("a.rs", 170, &["Map::parse"]),
            in_functions("a.rs", 560, &["Map::parse_lines"]),
        ];
        assert!(covers(&saved, &moved));
        // The copy grew into a function that the saved copy did not touch.
        let grown = [
            in_functions("a.rs", 10, &["Map::parse", "Map::new_helper"]),
            in_functions("a.rs", 400, &["Map::parse_lines"]),
        ];
        assert!(!covers(&saved, &grown));
        let other_file = [
            in_functions("a.rs", 10, &["Map::parse"]),
            in_functions("b.rs", 400, &["Map::parse_lines"]),
        ];
        assert!(!covers(&saved, &other_file));
    }

    #[test]
    fn an_edited_clone_marks_the_saved_group_matched_but_not_an_exact_match() {
        let copies = vec![region("a.rs", 10, 29), region("b.rs", 50, 69)];
        let baseline = loaded(vec![
            clone_entry("dup:1", copies.clone()),
            clone_entry("dup:2", copies.clone()),
            entry("dupes", "dup:3"),
        ]);
        assert!(baseline.absorbs("dup:1"));
        assert_eq!(
            baseline.absorbs_edited_clones(std::slice::from_ref(&copies)),
            [true]
        );
        assert_eq!(baseline.matched(), 2);
        assert_eq!(baseline.stale(&["dupes"]), [entry("dupes", "dup:3")]);
        let empty = loaded(vec![clone_entry("dup:1", copies.clone())]);
        assert!(empty.absorbs("dup:1"));
        assert_eq!(empty.absorbs_edited_clones(&[copies]), [false]);
    }

    #[test]
    fn a_saved_group_matches_more_groups_only_within_its_lines() {
        let saved = vec![
            in_functions("a.rs", 10, &["first"]),
            in_functions("b.rs", 10, &["second"]),
        ];
        let baseline = loaded(vec![clone_entry("dup:1", saved)]);
        let near = vec![
            in_functions("a.rs", 11, &["first"]),
            in_functions("b.rs", 11, &["second"]),
        ];
        let added = vec![
            in_functions("a.rs", 40, &["first"]),
            in_functions("b.rs", 40, &["second"]),
        ];
        assert_eq!(
            baseline.absorbs_edited_clones(&[added, near]),
            [false, true]
        );
        assert_eq!(baseline.matched(), 1);
        // An edit split the saved copy of 20 lines into two parts.
        let split = loaded(vec![clone_entry(
            "dup:1",
            vec![
                in_functions("a.rs", 10, &["first"]),
                in_functions("b.rs", 10, &["second"]),
            ],
        )]);
        let part = |start: u32, end_line: u32| {
            [("a.rs", "first"), ("b.rs", "second")]
                .map(|(file, name)| Region {
                    end_line,
                    ..in_functions(file, start, &[name])
                })
                .to_vec()
        };
        assert_eq!(
            split.absorbs_edited_clones(&[part(10, 18), part(20, 30)]),
            [true, true]
        );
        // An edit joined two saved clones of the same functions.
        let joined = loaded(vec![
            clone_entry("dup:1", part(10, 29)),
            clone_entry("dup:2", part(31, 36)),
        ]);
        assert_eq!(joined.absorbs_edited_clones(&[part(10, 36)]), [true]);
        assert!(joined.stale(&["dupes"]).is_empty());
    }

    #[test]
    fn an_equal_fingerprint_with_more_copies_is_new() {
        let copies = vec![region("a.rs", 1, 20), region("b.rs", 1, 20)];
        let baseline = loaded(vec![clone_entry("dup:1", copies.clone())]);
        let mut third = copies.clone();
        third.push(region("c.rs", 1, 20));
        assert!(!baseline.absorbs_entry(&clone_entry("dup:1", third)));
        assert!(baseline.absorbs_entry(&clone_entry("dup:1", copies)));
        // An entry from an older version has no copies, so the id decides.
        let old = loaded(vec![entry("dupes", "dup:2")]);
        assert!(old.absorbs_entry(&clone_entry(
            "dup:2",
            vec![
                region("a.rs", 1, 2),
                region("b.rs", 1, 2),
                region("c.rs", 1, 2)
            ]
        )));
    }
}
