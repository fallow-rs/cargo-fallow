//! The journal of one run: one JSON line for each mutant with a result.
//!
//! A stopped run leaves the journal, and the next run with the same source and
//! the same options reads it and runs only the other mutants. Each line reaches
//! the disk before the worker takes the next mutant. A crash can tear only the
//! last line.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::mutation::report::MutantRow;
use crate::mutation::status::{Status, Tier};
use crate::render;

/// One line of the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    id: String,
    tier: Tier,
    status: Status,
    reason: Option<String>,
    killed_by: Option<String>,
    retried: bool,
    duration_ms: u64,
    tests_run: u64,
    doctests_run: u64,
    no_line_data: bool,
}

impl Entry {
    fn of(row: &MutantRow) -> Self {
        Self {
            id: row.id.clone(),
            tier: row.tier,
            status: row.status,
            reason: row.reason.clone(),
            killed_by: row.killed_by.clone(),
            retried: row.retried,
            duration_ms: row.duration_ms,
            tests_run: row.tests_run,
            doctests_run: row.doctests_run,
            no_line_data: row.no_line_data,
        }
    }

    /// A `runtime_error` can come from the state of the machine, for example a
    /// guard kill at low memory, so that mutant runs again.
    const fn is_final(&self) -> bool {
        !matches!(self.status, Status::RuntimeError | Status::Pending)
    }
}

/// An open journal.
pub struct Journal {
    path: PathBuf,
    file: Mutex<File>,
    done: BTreeMap<String, Entry>,
}

impl Journal {
    /// Opens the journal and reads the results of an earlier run. A line with
    /// another shape does not count.
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = fs::read_to_string(path).unwrap_or_default();
        let done = text
            .lines()
            .filter_map(|line| serde_json::from_str::<Entry>(line).ok())
            .filter(Entry::is_final)
            .map(|entry| (entry.id.clone(), entry))
            .collect();
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        if !text.is_empty() && !text.ends_with('\n') {
            // The torn line of a crash must not swallow the next line.
            file.write_all(b"\n")?;
        }
        Ok(Self {
            path: path.to_path_buf(),
            file: Mutex::new(file),
            done,
        })
    }

    /// Writes the result of an earlier run into `row`. Returns false when the
    /// journal has no result for this mutant in this tier.
    pub fn restore(&self, row: &mut MutantRow) -> bool {
        let found = self.done.get(&row.id);
        let Some(entry) = found.filter(|entry| entry.tier == row.tier) else {
            return false;
        };
        row.status = entry.status;
        row.reason.clone_from(&entry.reason);
        row.killed_by.clone_from(&entry.killed_by);
        row.retried = entry.retried;
        row.duration_ms = entry.duration_ms;
        row.tests_run = entry.tests_run;
        row.doctests_run = entry.doctests_run;
        row.no_line_data = entry.no_line_data;
        true
    }

    /// Adds the result of one mutant. A failed write only costs time after a
    /// stop, so it gives a warning and the run continues.
    pub fn record(&self, row: &MutantRow) {
        if let Err(err) = self.try_record(row) {
            render::warn(&format!("cannot write the journal: {err}"));
        }
    }

    fn try_record(&self, row: &MutantRow) -> io::Result<()> {
        let mut line = serde_json::to_vec(&Entry::of(row))?;
        line.push(b'\n');
        let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        file.write_all(&line)?;
        file.sync_data()
    }

    /// Removes the journal after a run where each mutant has a result. A failed
    /// removal only makes the next run read old results of this same source.
    pub fn finish(&self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, tier: Tier) -> MutantRow {
        MutantRow {
            id: id.to_owned(),
            tier,
            operator: "o",
            file: "f".to_owned(),
            line: 1,
            column: 1,
            original: String::new(),
            replacement: String::new(),
            status: Status::Pending,
            reason: None,
            killed_by: None,
            retried: false,
            removed_from_schema: false,
            under_cfg: false,
            duration_ms: 0,
            tests_run: 0,
            doctests_run: 0,
            no_line_data: false,
        }
    }

    fn open(path: &Path) -> Journal {
        let Ok(journal) = Journal::open(path) else {
            panic!("the journal opens");
        };
        journal
    }

    fn path() -> (tempfile::TempDir, PathBuf) {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let path = dir.path().join("sub/journal.jsonl");
        (dir, path)
    }

    #[test]
    fn a_result_comes_back_in_the_next_run() {
        let (_dir, path) = path();
        let mut done = row("a", Tier::Schema);
        done.status = Status::Killed;
        done.killed_by = Some("t".to_owned());
        done.tests_run = 3;
        open(&path).record(&done);
        let journal = open(&path);
        let mut restored = row("a", Tier::Schema);
        assert!(journal.restore(&mut restored));
        assert_eq!(restored.status, Status::Killed);
        assert_eq!(restored.killed_by.as_deref(), Some("t"));
        assert_eq!(restored.tests_run, 3);
        assert!(!journal.restore(&mut row("b", Tier::Schema)));
    }

    #[test]
    fn another_tier_and_a_runtime_error_run_again() {
        let (_dir, path) = path();
        let journal = open(&path);
        let mut killed = row("a", Tier::Schema);
        killed.status = Status::Killed;
        journal.record(&killed);
        let mut error = row("b", Tier::Schema);
        error.status = Status::RuntimeError;
        journal.record(&error);
        let journal = open(&path);
        assert!(!journal.restore(&mut row("a", Tier::Rebuild)));
        assert!(!journal.restore(&mut row("b", Tier::Schema)));
    }

    #[test]
    fn a_torn_line_counts_as_no_result_and_does_not_swallow_the_next_one() {
        let (_dir, path) = path();
        let mut done = row("a", Tier::Schema);
        done.status = Status::Survived;
        open(&path).record(&done);
        let Ok(text) = fs::read_to_string(&path) else {
            panic!("read the journal");
        };
        let Ok(()) = fs::write(&path, format!("{text}{{\"id\":\"torn")) else {
            panic!("write the journal");
        };
        let mut next = row("b", Tier::Schema);
        next.status = Status::Timeout;
        open(&path).record(&next);
        let journal = open(&path);
        assert!(journal.restore(&mut row("a", Tier::Schema)));
        assert!(journal.restore(&mut row("b", Tier::Schema)));
    }

    #[test]
    fn finish_removes_the_file() {
        let (_dir, path) = path();
        let journal = open(&path);
        journal.finish();
        assert!(!path.exists());
    }
}
