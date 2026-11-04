//! The scope of a run: the findings that the report keeps.
//!
//! The analysis always reads the whole workspace. The scope only removes
//! findings from the report, before the gates count them. A full scope keeps
//! each finding. Two flags narrow the scope: `--changed-since` keeps the
//! findings in the changed files, and `--baseline` leaves out the findings of
//! an earlier run.

use std::path::PathBuf;

use clap::Args;
use serde_json::Value;

use crate::baseline::{self, Entry, Loaded, Recorder};
use crate::diff::ChangedFiles;
use crate::error::CliError;
use crate::finding::Finding;
use crate::gates::RustGateName;
use crate::render;
use crate::section::{Section, to_value};
use crate::workspace::Workspace;

/// The flag that scopes a run to the changed files.
pub const CHANGED_SINCE_FLAG: &str = "--changed-since";

/// The scope flags of `check` and of each static command.
#[derive(Debug, Clone, Default, Args)]
pub struct ScopeArgs {
    /// Report only the findings in the files that changed since the fork point
    /// of this ref and `HEAD`. Uncommitted and untracked files count. The
    /// analysis still reads the whole workspace.
    #[arg(long, value_name = "REF")]
    pub changed_since: Option<String>,
    /// Leave out each finding whose id is in this baseline file, from
    /// `--save-baseline`.
    #[arg(long, value_name = "PATH", conflicts_with = "save_baseline")]
    pub baseline: Option<PathBuf>,
    /// Write the id, the check and the rule of each finding to this file, and
    /// exit 0. A later run with `--baseline` leaves out these findings.
    #[arg(long, value_name = "PATH", conflicts_with = "changed_since")]
    pub save_baseline: Option<PathBuf>,
    /// Exit 1 when the baseline has an entry with no finding in the run.
    #[arg(long, requires = "baseline")]
    pub fail_on_stale_baseline: bool,
}

/// The line of the human output that names the changed files.
pub fn changed_line(changed: &ChangedFiles) -> String {
    let count = changed.files.len();
    let base: String = changed.base.chars().take(12).collect();
    format!(
        "Scope: {count} {} changed since `{}` (fork point {base})\n",
        if count == 1 { "file" } else { "files" },
        changed.reference,
    )
}

/// The line of the human output that names the baseline.
pub fn baseline_line(summary: &baseline::Summary) -> String {
    format!(
        "Baseline: `{}`, {} {} left out, {} stale\n",
        summary.path,
        summary.matched,
        if summary.matched == 1 {
            "finding"
        } else {
            "findings"
        },
        summary.stale.len(),
    )
}

/// The findings that a run keeps.
#[derive(Debug, Default)]
pub struct Scope {
    /// The changed files of `--changed-since`, or `None` for each file.
    pub changed: Option<ChangedFiles>,
    /// The baseline of `--baseline`.
    baseline: Option<Loaded>,
    /// The entries for `--save-baseline`.
    recorder: Option<Recorder>,
    /// True when a stale baseline entry fails the run.
    fail_on_stale: bool,
    /// True when a flag of the command narrows the findings, for example
    /// `visibility --kind`. A baseline entry then can be stale with no change
    /// of the code, so the run does not judge stale entries.
    narrowed: bool,
}

impl Scope {
    /// The scope of the flags.
    pub fn load(workspace: &Workspace, args: &ScopeArgs) -> Result<Self, CliError> {
        Ok(Self {
            changed: args
                .changed_since
                .as_deref()
                .map(|reference| ChangedFiles::load(workspace, CHANGED_SINCE_FLAG, reference))
                .transpose()?,
            baseline: args.baseline.as_deref().map(Loaded::read).transpose()?,
            recorder: args.save_baseline.clone().map(Recorder::new),
            fail_on_stale: args.fail_on_stale_baseline,
            narrowed: false,
        })
    }

    /// The scope of a run in which a flag narrows the findings.
    #[must_use]
    pub const fn narrowed(mut self, narrowed: bool) -> Self {
        self.narrowed = narrowed;
        self
    }

    /// True when the run can judge stale baseline entries: it has no file
    /// filter and no flag that narrows the findings.
    const fn judges_stale(&self) -> bool {
        self.is_full() && !self.narrowed
    }

    /// True when the run removes the findings of a baseline.
    pub const fn has_baseline(&self) -> bool {
        self.baseline.is_some()
    }

    /// True when no file filter narrows the scope. A baseline does not
    /// count: it removes known findings, and the rest of the report still
    /// covers the whole workspace.
    pub const fn is_full(&self) -> bool {
        self.changed.is_none()
    }

    /// True when the scope keeps a finding at `path`.
    pub fn keeps(&self, path: &str) -> bool {
        self.changed
            .as_ref()
            .is_none_or(|changed| changed.keeps(path))
    }

    /// True when the scope keeps a finding with one of these places.
    pub fn keeps_any<'a>(&self, paths: impl Iterator<Item = &'a str>) -> bool {
        self.changed
            .as_ref()
            .is_none_or(|changed| changed.keeps_any(paths))
    }

    /// True when the baseline does not hold the entry. The entry then goes to
    /// the baseline file of `--save-baseline`.
    pub fn keeps_entry(&self, entry: Entry) -> bool {
        if self
            .baseline
            .as_ref()
            .is_some_and(|baseline| baseline.absorbs(&entry.id))
        {
            return false;
        }
        if let Some(recorder) = &self.recorder {
            recorder.record(entry);
        }
        true
    }

    /// True when the baseline does not hold the entry that `entry` builds.
    /// A run without a baseline and without `--save-baseline` builds no
    /// entry.
    pub fn keeps_entry_of(&self, entry: impl FnOnce() -> Entry) -> bool {
        if self.baseline.is_none() && self.recorder.is_none() {
            return true;
        }
        self.keeps_entry(entry())
    }

    /// True when the baseline does not hold the finding.
    pub fn keeps_finding(&self, finding: &Finding) -> bool {
        if self.baseline.is_none() && self.recorder.is_none() {
            return true;
        }
        self.keeps_entry(Entry::of(finding))
    }

    /// Keeps the items whose finding in the shared shape the baseline does not
    /// hold. `shared` has one finding for each item, in the same order.
    pub fn retain_pairs<T>(&self, items: &mut Vec<T>, shared: &mut Vec<Finding>) {
        debug_assert_eq!(
            items.len(),
            shared.len(),
            "one shared finding for each item"
        );
        if self.baseline.is_none() && self.recorder.is_none() {
            return;
        }
        let keep: Vec<bool> = shared
            .iter()
            .map(|finding| self.keeps_finding(finding))
            .collect();
        let mut flags = keep.iter();
        items.retain(|_| flags.next().copied().unwrap_or(true));
        let mut flags = keep.iter();
        shared.retain(|_| flags.next().copied().unwrap_or(true));
    }

    /// The `baseline` member of a report on `checks`, or `None` without
    /// `--baseline`.
    pub fn baseline_summary(&self, checks: &[&str]) -> Option<baseline::Summary> {
        self.baseline.as_ref().map(|baseline| baseline::Summary {
            path: baseline.path.clone(),
            matched: baseline.matched(),
            stale: if self.judges_stale() {
                baseline.stale(checks)
            } else {
                Vec::new()
            },
        })
    }

    /// The `stale-baseline` gate of a report with this baseline member.
    pub fn stale_gate(&self, summary: &baseline::Summary) -> fallow_report_protocol::GateOutcome {
        baseline::stale_gate(summary.stale.len(), self.judges_stale(), self.fail_on_stale)
    }

    /// True when the run writes a baseline file. Such a run exits 0.
    pub const fn saves(&self) -> bool {
        self.recorder.is_some()
    }

    /// Writes the baseline file of `--save-baseline`. `kind` is the command.
    pub fn save(&self, kind: &str) -> Result<(), CliError> {
        self.recorder
            .as_ref()
            .map_or(Ok(()), |recorder| recorder.write(kind))
    }

    /// The section of one command with the scope in its JSON and human
    /// output. The `kind` of the JSON report names the check.
    pub fn annotate(&self, mut section: Section) -> Result<Section, CliError> {
        let check = section.json["kind"].as_str().unwrap_or_default().to_owned();
        let check = check.as_str();
        let mut lines = String::new();
        if let Some(changed) = &self.changed {
            insert(&mut section.json, "changed_since", to_value(changed)?);
            lines.push_str(&changed_line(changed));
        }
        if let Some(summary) = self.baseline_summary(&[check]) {
            let gate = self.stale_gate(&summary);
            if let Some(map) = section
                .json
                .get_mut("gate_outcomes")
                .and_then(Value::as_object_mut)
            {
                map.insert(
                    RustGateName::StaleBaseline.as_str().to_owned(),
                    to_value(&gate)?,
                );
            }
            lines.push_str(&baseline_line(&summary));
            if gate.fails_run() {
                lines.push_str("The `stale-baseline` gate fails: remove the stale entries with `--save-baseline`.\n");
            }
            section.human = format!(
                "{}{}\n",
                section.human.trim_end(),
                render::gate_block_line(RustGateName::StaleBaseline, &gate)
            );
            if let Some(note) = render::gate_notification(RustGateName::StaleBaseline, &gate) {
                add_notification(&mut section.sarif, note);
            }
            section.gates.insert(RustGateName::StaleBaseline, gate);
            insert(&mut section.json, "baseline", to_value(&summary)?);
        }
        section.human.insert_str(0, &lines);
        if self.saves() {
            self.save(check)?;
            for gate in section.gates.values_mut() {
                gate.enforced = false;
            }
            unenforce(&mut section.json);
        }
        Ok(section)
    }
}

/// Sets `enforced` to false in each gate of a JSON report. A run that saves a
/// baseline exits 0.
pub fn unenforce(json: &mut Value) {
    if let Some(map) = json.get_mut("gate_outcomes").and_then(Value::as_object_mut) {
        for gate in map.values_mut() {
            if let Some(gate) = gate.as_object_mut() {
                gate.insert("enforced".to_owned(), Value::Bool(false));
            }
        }
    }
}

/// Adds a gate notification to the first run of a SARIF log.
fn add_notification(sarif: &mut Value, note: Value) {
    let Some(invocation) = sarif
        .pointer_mut("/runs/0/invocations/0")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    match invocation.get_mut("toolExecutionNotifications") {
        Some(Value::Array(notes)) => notes.push(note),
        _ => {
            invocation.insert(
                "toolExecutionNotifications".to_owned(),
                Value::Array(vec![note]),
            );
        }
    }
}

/// Adds a member to a JSON object. A value that is not an object stays.
fn insert(json: &mut Value, key: &str, value: Value) {
    if let Value::Object(map) = json {
        map.insert(key.to_owned(), value);
    }
}
