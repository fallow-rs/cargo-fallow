//! The managed block of `AGENTS.md`: a short task map for coding agents.
//!
//! The block sits between two marker lines, with a checksum line after the
//! start marker. A block whose checksum matches is a generated block, so a
//! new install replaces it. A block that someone edited stays, unless the
//! user gives `--force`. A marker counts only as a whole line, so a marker
//! that the prose quotes does not count. The markers differ from the markers
//! of the fallow block for JavaScript, so both blocks can live in one file.

use std::fs;
use std::io::{self, Write as _};
use std::ops::Range;
use std::path::Path;

use crate::hooks::files::{self, Outcome, ScriptState, read_error, write_error};

/// The start marker of the block.
pub const START: &str = "<!-- cargo-fallow:agents:start -->";

/// The end marker of the block.
pub const END: &str = "<!-- cargo-fallow:agents:end -->";

/// The text of the block between the markers.
const BODY: &str = "## cargo-fallow

This repository uses `cargo fallow` for the analysis of the Rust code. Run it
with `--format json --quiet`, and read the JSON on stdout. Exit code 1 means
that an enforced gate failed. Exit code 2 means an error.

| Task | Command |
| --- | --- |
| Check the changed files | `cargo fallow check --changed-since main --format json --quiet` |
| Check the workspace | `cargo fallow check --format json --quiet` |
| Explain a rule | `cargo fallow explain <rule> --format json` |
| Preview the fixes | `cargo fallow fix --dry-run --format json --quiet` |

A gate runs `cargo fallow check` before each `git commit` and `git push` of
an agent. It blocks only on a new finding in the changed files. When it
blocks, fix each finding that it lists, then run the command again. Do not
skip the gate. The `cargo-fallow` skill has the details.";

/// The file of the block, relative to the install root.
pub const FILE: &str = "AGENTS.md";

/// The state of the block in a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The current block.
    Current,
    /// A generated block of another version.
    Outdated,
    /// A block that someone edited, or markers that are not one pair.
    Edited,
    /// No marker.
    Absent,
}

impl State {
    /// The name in the output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Outdated => "outdated",
            Self::Edited => "edited",
            Self::Absent => "absent",
        }
    }
}

/// The block with its markers and its checksum, with `\n` line ends.
fn block() -> String {
    files::with_checksum(&format!("{START}\n{BODY}\n{END}\n"), START)
}

/// The line end of a text: `\r\n` when the text has one.
fn newline(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// The byte range of the block in `text`, from the start of the start line to
/// the end of the end line. `Err` when the markers are not one start line
/// before one end line.
fn find(text: &str) -> Result<Option<Range<usize>>, ()> {
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        match line.trim() {
            START => starts.push(offset),
            END => ends.push(offset + line.len()),
            _ => {}
        }
        offset += line.len();
    }
    match (starts.as_slice(), ends.as_slice()) {
        ([], []) => Ok(None),
        ([start], [end]) if start < end => Ok(Some(*start..*end)),
        _ => Err(()),
    }
}

/// The state of the block in `text`.
pub fn state(text: &str) -> State {
    match find(text) {
        Ok(None) => State::Absent,
        Err(()) => State::Edited,
        Ok(Some(range)) => {
            let found = text[range].replace("\r\n", "\n");
            if found == block() {
                State::Current
            } else if matches!(files::classify(&found, START), ScriptState::Generated(_)) {
                State::Outdated
            } else {
                State::Edited
            }
        }
    }
}

/// The text of the file with the block, and the outcome.
fn upsert(text: &str, force: bool) -> (String, Outcome) {
    let line_end = newline(text);
    let block = block().replace('\n', line_end);
    match (find(text), state(text)) {
        (Ok(None), _) => {
            let mut next = text.to_owned();
            if !next.is_empty() {
                if !next.ends_with('\n') {
                    next.push_str(line_end);
                }
                next.push_str(line_end);
            }
            next.push_str(&block);
            (next, Outcome::Updated)
        }
        (Ok(Some(_)), State::Current) => (text.to_owned(), Outcome::Unchanged),
        (Ok(Some(range)), state) if force || state == State::Outdated => (
            format!("{}{block}{}", &text[..range.start], &text[range.end..]),
            Outcome::Updated,
        ),
        _ => (text.to_owned(), Outcome::Kept),
    }
}

/// The text of the file without the block, or `None` when the file keeps
/// it: no block, broken markers, or an edited block without `force`.
fn remove(text: &str, force: bool) -> Option<String> {
    let range = find(text).ok()??;
    if state(text) == State::Edited && !force {
        return None;
    }
    let before = text[..range.start].trim_end_matches(['\r', '\n']);
    let after = text[range.end..].trim_start_matches(['\r', '\n']);
    let line_end = newline(text);
    Some(match (before.is_empty(), after.is_empty()) {
        (true, _) => after.to_owned(),
        (false, true) => format!("{before}{line_end}"),
        (false, false) => format!("{before}{line_end}{line_end}{after}"),
    })
}

/// Reads the file, or `None` when it does not exist.
fn read(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(read_error(path, &err)),
    }
}

/// Writes the file through a temporary file and a rename, so a failed write
/// does not leave a cut file.
fn write(path: &Path, text: &str) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut file =
        tempfile::NamedTempFile::new_in(parent).map_err(|err| write_error(path, &err))?;
    file.write_all(text.as_bytes())
        .map_err(|err| write_error(path, &err))?;
    file.persist(path)
        .map(drop)
        .map_err(|err| write_error(path, &err.error))
}

/// Adds or refreshes the block in `AGENTS.md` under `root`.
pub fn install(root: &Path, force: bool, dry_run: bool) -> Result<Outcome, String> {
    let path = crate::hooks::settings::safe_path(root, FILE)?;
    let current = read(&path)?;
    let (next, outcome) = upsert(current.as_deref().unwrap_or_default(), force);
    let outcome = if current.is_none() {
        Outcome::Created
    } else {
        outcome
    };
    if !dry_run && matches!(outcome, Outcome::Created | Outcome::Updated) {
        write(&path, &next)?;
    }
    Ok(outcome)
}

/// Removes the block from `AGENTS.md` under `root`. A file that only held
/// the block goes. An edited block stays, unless `force` is true.
pub fn uninstall(root: &Path, force: bool, dry_run: bool) -> Result<Outcome, String> {
    let path = crate::hooks::settings::safe_path(root, FILE)?;
    let Some(current) = read(&path)? else {
        return Ok(Outcome::NotPresent);
    };
    let Some(next) = remove(&current, force) else {
        return Ok(if state(&current) == State::Absent {
            Outcome::NotPresent
        } else {
            Outcome::Kept
        });
    };
    if next.trim().is_empty() {
        if !dry_run {
            fs::remove_file(&path).map_err(|err| write_error(&path, &err))?;
        }
        return Ok(Outcome::Removed);
    }
    if !dry_run {
        write(&path, &next)?;
    }
    Ok(Outcome::Updated)
}

/// The state of the block in `AGENTS.md` under `root`.
pub fn file_state(root: &Path) -> State {
    fs::read_to_string(root.join(FILE)).map_or(State::Absent, |text| state(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_goes_after_the_text_and_a_second_run_changes_nothing() {
        let (first, outcome) = upsert("# Guide\n\nText.\n", false);
        assert_eq!(outcome, Outcome::Updated);
        assert!(
            first.starts_with("# Guide\n\nText.\n\n<!-- cargo-fallow"),
            "{first}"
        );
        assert_eq!(state(&first), State::Current);
        assert_eq!(upsert(&first, false), (first.clone(), Outcome::Unchanged));
        assert_eq!(remove(&first, false).as_deref(), Some("# Guide\n\nText.\n"));
    }

    #[test]
    fn an_old_generated_block_is_replaced_in_place() {
        let old = files::with_checksum(&format!("{START}\nold\n{END}\n"), START);
        let text = format!("a\n\n{old}\nb\n");
        assert_eq!(state(&text), State::Outdated);
        let (next, outcome) = upsert(&text, false);
        assert_eq!(outcome, Outcome::Updated);
        assert!(next.starts_with(&format!("a\n\n{START}\n")), "{next}");
        assert!(next.ends_with(&format!("{END}\n\nb\n")), "{next}");
        assert!(!next.contains("\nold\n"));
    }

    #[test]
    fn an_edited_block_stays_without_force() {
        let edited = block().replace("## cargo-fallow", "## cargo-fallow (mine)");
        assert_eq!(state(&edited), State::Edited);
        assert_eq!(upsert(&edited, false), (edited.clone(), Outcome::Kept));
        assert_eq!(remove(&edited, false), None);
        assert_eq!(upsert(&edited, true).1, Outcome::Updated);
        assert_eq!(remove(&edited, true).as_deref(), Some(""));
    }

    #[test]
    fn a_quoted_marker_is_not_a_marker() {
        let text = format!("Wrap text in `{START}` and `{END}`.\n");
        assert_eq!(state(&text), State::Absent);
        let (next, outcome) = upsert(&text, false);
        assert_eq!(outcome, Outcome::Updated);
        assert!(next.starts_with(&text), "{next}");
    }

    #[test]
    fn broken_markers_keep_the_file() {
        for text in [
            format!("{END}\n{START}\n"),
            format!("{START}\n{START}\n{END}\n"),
        ] {
            assert_eq!(upsert(&text, false), (text.clone(), Outcome::Kept));
            assert_eq!(remove(&text, true), None);
        }
    }

    #[test]
    fn a_crlf_file_keeps_its_line_ends() {
        let (next, _) = upsert("# Guide\r\n", false);
        assert!(!next.replace("\r\n", "").contains('\n'), "{next:?}");
        assert_eq!(state(&next), State::Current);
        assert_eq!(remove(&next, false).as_deref(), Some("# Guide\r\n"));
    }

    #[test]
    fn the_fallow_block_stays() {
        let fallow = "<!-- fallow:setup-hooks:start -->\nfallow\n<!-- fallow:setup-hooks:end -->\n";
        let (next, _) = upsert(fallow, false);
        assert!(next.starts_with(fallow));
        assert_eq!(remove(&next, false).as_deref(), Some(fallow));
    }
}
