//! The managed block of `AGENTS.md`: a short task map for coding agents.
//!
//! The block sits between two markers. A new install replaces the text
//! between them, so a second run changes nothing. The markers differ from
//! the markers of the fallow block for JavaScript, so both blocks can live in
//! one file.

use std::fs;
use std::io;
use std::path::Path;

use crate::hooks::files::{Outcome, read_error, write_error};

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
| Apply the fixes | `cargo fallow fix --yes --format json --quiet` |

A gate runs `cargo fallow check` before each `git commit` and `git push` of
an agent. It blocks only on a new finding in the changed files. When it
blocks, fix each finding that it lists, then run the command again. Do not
skip the gate. The `cargo-fallow` skill has the details.";

/// The file of the block, relative to the install root.
pub const FILE: &str = "AGENTS.md";

/// The block with its markers.
fn block() -> String {
    format!("{START}\n{BODY}\n{END}\n")
}

/// The text of the file with the block, and the outcome. A file whose markers
/// are not one start before one end stays as it is.
fn upsert(text: &str) -> (String, Outcome) {
    let block = block();
    match (text.find(START), text.find(END)) {
        (None, None) => {
            let mut next = text.to_owned();
            if !next.is_empty() {
                next.push_str(if next.ends_with('\n') { "\n" } else { "\n\n" });
            }
            next.push_str(&block);
            (next, Outcome::Updated)
        }
        (Some(start), Some(end))
            if start < end
                && text.matches(START).count() == 1
                && text.matches(END).count() == 1 =>
        {
            let after = &text[end + END.len()..];
            let after = after.strip_prefix('\n').unwrap_or(after);
            let next = format!("{}{block}{after}", &text[..start]);
            let outcome = if next == text {
                Outcome::Unchanged
            } else {
                Outcome::Updated
            };
            (next, outcome)
        }
        _ => (text.to_owned(), Outcome::Kept),
    }
}

/// The text of the file without the block, or `None` when the file has no
/// block that the command can remove.
fn remove(text: &str) -> Option<String> {
    let start = text.find(START)?;
    let end = text.find(END)?;
    if start > end || text.matches(START).count() != 1 || text.matches(END).count() != 1 {
        return None;
    }
    let before = text[..start].trim_end_matches('\n');
    let after = text[end + END.len()..].trim_start_matches('\n');
    Some(match (before.is_empty(), after.is_empty()) {
        (true, _) => after.to_owned(),
        (false, true) => format!("{before}\n"),
        (false, false) => format!("{before}\n\n{after}"),
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

/// Adds or refreshes the block in `AGENTS.md` under `root`.
pub fn install(root: &Path, dry_run: bool) -> Result<Outcome, String> {
    let path = crate::hooks::settings::safe_path(root, FILE)?;
    let current = read(&path)?;
    let (next, outcome) = upsert(current.as_deref().unwrap_or_default());
    let outcome = if current.is_none() {
        Outcome::Created
    } else {
        outcome
    };
    if !dry_run && matches!(outcome, Outcome::Created | Outcome::Updated) {
        fs::write(&path, next).map_err(|err| write_error(&path, &err))?;
    }
    Ok(outcome)
}

/// Removes the block from `AGENTS.md` under `root`. A file that only held
/// the block goes.
pub fn uninstall(root: &Path, dry_run: bool) -> Result<Outcome, String> {
    let path = crate::hooks::settings::safe_path(root, FILE)?;
    let Some(current) = read(&path)? else {
        return Ok(Outcome::NotPresent);
    };
    let Some(next) = remove(&current) else {
        return Ok(if current.contains(START) || current.contains(END) {
            Outcome::Kept
        } else {
            Outcome::NotPresent
        });
    };
    if !dry_run {
        if next.trim().is_empty() {
            fs::remove_file(&path).map_err(|err| write_error(&path, &err))?;
        } else {
            fs::write(&path, next).map_err(|err| write_error(&path, &err))?;
        }
    }
    Ok(Outcome::Removed)
}

/// True when `AGENTS.md` under `root` holds the current block.
pub fn installed(root: &Path) -> bool {
    fs::read_to_string(root.join(FILE)).is_ok_and(|text| text.contains(&block()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_goes_after_the_text_and_a_second_run_changes_nothing() {
        let (first, outcome) = upsert("# Guide\n\nText.\n");
        assert_eq!(outcome, Outcome::Updated);
        assert!(
            first.starts_with("# Guide\n\nText.\n\n<!-- cargo-fallow"),
            "{first}"
        );
        assert_eq!(upsert(&first), (first.clone(), Outcome::Unchanged));
        assert_eq!(remove(&first).as_deref(), Some("# Guide\n\nText.\n"));
    }

    #[test]
    fn an_old_block_is_replaced_in_place() {
        let text = format!("a\n\n{START}\nold\n{END}\n\nb\n");
        let (next, outcome) = upsert(&text);
        assert_eq!(outcome, Outcome::Updated);
        assert!(
            next.starts_with("a\n\n<!-- cargo-fallow:agents:start -->\n## cargo-fallow"),
            "{next}"
        );
        assert!(next.ends_with(&format!("{END}\n\nb\n")), "{next}");
        assert!(!next.contains("old"));
    }

    #[test]
    fn broken_markers_keep_the_file() {
        for text in [
            format!("{END}\n{START}\n"),
            format!("{START}\n{START}\n{END}\n"),
        ] {
            assert_eq!(upsert(&text), (text.clone(), Outcome::Kept));
            assert_eq!(remove(&text), None);
        }
    }

    #[test]
    fn the_fallow_block_stays() {
        let fallow = "<!-- fallow:setup-hooks:start -->\nfallow\n<!-- fallow:setup-hooks:end -->\n";
        let (next, _) = upsert(fallow);
        assert!(next.starts_with(fallow));
        assert_eq!(remove(&next).as_deref(), Some(fallow));
    }
}
