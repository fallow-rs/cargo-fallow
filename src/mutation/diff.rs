//! The changed lines of `--in-diff`.
//!
//! The base is the fork point of the ref and `HEAD`, so a later commit on the ref
//! does not count as a change of the branch. The diff compares the base with the
//! working tree, so an uncommitted edit counts. An untracked file counts as changed
//! on each line. Each failure of git stops the run: a run with the wrong scope looks
//! like a clean result.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{CliError, ErrorCode};
use crate::mutation::coverage::LineRange;
use crate::mutation::files::CrateFile;
use crate::mutation::plan::lines_of;
use crate::mutation::process::new_command;
use crate::render;

/// The last line of a range that covers a whole file.
const WHOLE_FILE: LineRange = (1, u32::MAX);

/// The changed lines of a working tree.
#[derive(Debug, Clone)]
pub struct Changed {
    /// The ref of the flag.
    pub reference: String,
    /// The commit that the diff starts from.
    pub base: String,
    files: BTreeMap<String, Vec<LineRange>>,
}

impl Changed {
    /// Asks git for the changed lines under `root`.
    pub fn load(root: &Path, reference: &str) -> Result<Self, CliError> {
        let bad_ref = |detail: String| {
            CliError::new(
                ErrorCode::InvalidArgument,
                format!("`--in-diff {reference}`: {detail}"),
            )
            .with_help(
                "Pass a branch, a tag or a commit of the repository. In a shallow clone, \
                 fetch the ref and its history first.",
            )
        };
        // `--end-of-options` keeps a ref that starts with `-` out of the flags of
        // git. The later calls only get the full commit hash of this call.
        let commit = git(
            root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &format!("{reference}^{{commit}}"),
            ],
        )?
        .map_err(|_| bad_ref("git does not know this ref here".to_owned()))?;
        let base = git(root, &["merge-base", commit.trim(), "HEAD"])?
            .map_err(|error| bad_ref(format!("no common commit with `HEAD`: {error}")))?;
        let base = base.trim().to_owned();
        let diff = git(
            root,
            &[
                "-c",
                "core.quotePath=false",
                "diff",
                "--unified=0",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--find-renames",
                "--relative",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                &base,
                "--",
            ],
        )?
        .map_err(|error| failed("read the diff", &error))?;
        let untracked = git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?
            .map_err(|error| failed("list the untracked files", &error))?;
        let parsed = parse_unified(&diff);
        if !parsed.quoted.is_empty() {
            render::warn(&format!(
                "git quotes these paths, so `--in-diff` does not see their changes: {}",
                parsed.quoted.join(", ")
            ));
        }
        let mut files = parsed.files;
        for path in untracked.split('\0').filter(|path| !path.is_empty()) {
            files.insert(path.to_owned(), vec![WHOLE_FILE]);
        }
        Ok(Self {
            reference: reference.to_owned(),
            base,
            files,
        })
    }

    /// The files with a changed line.
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// True when a line of the closed range changed.
    pub fn touches(&self, file: &str, (first, last): LineRange) -> bool {
        self.files.get(file).is_some_and(|ranges| {
            ranges
                .iter()
                .any(|(start, end)| *start <= last && first <= *end)
        })
    }

    /// Removes each site with no changed line. Returns the mutants that went.
    pub fn retain_sites(&self, files: &mut [CrateFile]) -> usize {
        let mut removed = 0;
        for file in files {
            let CrateFile {
                path,
                original,
                found,
                ..
            } = file;
            found.sites.retain(|site| {
                let keep = self.touches(path, lines_of(original, &site.range));
                if !keep {
                    removed += site.mutants.len();
                }
                keep
            });
        }
        removed
    }
}

fn failed(action: &str, error: &str) -> CliError {
    CliError::new(
        ErrorCode::MutationFailed,
        format!("cannot {action}: {error}"),
    )
}

/// Runs git in `dir`. The outer error means that git does not run at all, and the
/// inner error holds the message of a call that failed. A hook of the caller can set
/// the `GIT_*` variables, and they must not choose another repository.
fn git(dir: &Path, args: &[&str]) -> Result<Result<String, String>, CliError> {
    let mut command: Command = new_command("git");
    let output = command
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| failed("run git", &err.to_string()))?;
    if output.status.success() {
        return Ok(Ok(String::from_utf8_lossy(&output.stdout).into_owned()));
    }
    let message = String::from_utf8_lossy(&output.stderr);
    Ok(Err(message.lines().next().unwrap_or_default().to_owned()))
}

/// The content of a unified diff.
#[derive(Debug, Default, PartialEq, Eq)]
struct ParsedDiff {
    files: BTreeMap<String, Vec<LineRange>>,
    quoted: Vec<String>,
}

/// Reads a diff from `git diff --unified=0`. The hunk header gives the number of
/// content lines, so a content line that looks like a header stays content.
fn parse_unified(text: &str) -> ParsedDiff {
    let mut parsed = ParsedDiff::default();
    let mut current: Option<&str> = None;
    let mut content_lines = 0_u64;
    for line in text.split('\n') {
        if content_lines > 0 {
            content_lines -= 1;
            continue;
        }
        if let Some(path) = line.strip_prefix("+++ ") {
            current = path.strip_prefix("b/");
            if path.starts_with('"') {
                parsed.quoted.push(path.to_owned());
            }
        } else if let Some((old, new)) = hunk_counts(line) {
            content_lines = u64::from(old.1) + u64::from(new.1);
            if let Some(path) = current
                && new.1 > 0
            {
                let last = new.0.saturating_add(new.1 - 1);
                parsed
                    .files
                    .entry(path.to_owned())
                    .or_default()
                    .push((new.0, last));
            }
        }
    }
    parsed
}

/// The `(start, count)` pairs of a hunk header: `@@ -1,2 +3,4 @@ context`.
fn hunk_counts(line: &str) -> Option<((u32, u32), (u32, u32))> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    Some((start_and_count(old)?, start_and_count(new)?))
}

/// A missing count means one line.
fn start_and_count(text: &str) -> Option<(u32, u32)> {
    match text.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((text.parse().ok()?, 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(diff: &str, file: &str) -> Vec<LineRange> {
        parse_unified(diff)
            .files
            .get(file)
            .cloned()
            .unwrap_or_default()
    }

    #[test]
    fn a_hunk_gives_its_new_lines() {
        let diff = "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1,2 +1,3 @@ fn f()\n-a\n-b\n+c\n+d\n+e\n@@ -9 +10 @@\n-f\n+g\n";
        assert_eq!(ranges(diff, "x.rs"), [(1, 3), (10, 10)]);
    }

    #[test]
    fn a_deletion_changes_no_line() {
        let diff = "--- a/x.rs\n+++ b/x.rs\n@@ -3,2 +2,0 @@\n-a\n-b\n";
        assert_eq!(parse_unified(diff), ParsedDiff::default());
    }

    #[test]
    fn a_removed_file_has_no_target() {
        let diff = "--- a/x.rs\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n";
        assert_eq!(parse_unified(diff), ParsedDiff::default());
    }

    #[test]
    fn a_content_line_that_looks_like_a_header_stays_content() {
        let diff = "--- a/x.rs\n+++ b/x.rs\n@@ -0,0 +1,2 @@\n+++ b/other.rs\n+@@ -1 +1 @@\n@@ -5 +7 @@\n-a\n+b\n";
        let parsed = parse_unified(diff);
        assert_eq!(parsed.files.len(), 1, "{parsed:?}");
        assert_eq!(ranges(diff, "x.rs"), [(1, 2), (7, 7)]);
    }

    #[test]
    fn the_no_newline_marker_is_not_a_header() {
        let diff = "--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n\\ No newline at end of file\n@@ -8 +8 @@\n-c\n+d\n";
        assert_eq!(ranges(diff, "x.rs"), [(1, 1), (8, 8)]);
    }

    #[test]
    fn a_quoted_path_is_named_and_has_no_lines() {
        let diff = "--- \"a/x\\ty.rs\"\n+++ \"b/x\\ty.rs\"\n@@ -1 +1 @@\n-a\n+b\n";
        let parsed = parse_unified(diff);
        assert!(parsed.files.is_empty(), "{parsed:?}");
        assert_eq!(parsed.quoted.len(), 1, "{parsed:?}");
    }

    #[test]
    fn a_range_touches_only_an_overlap() {
        let changed = Changed {
            reference: "main".to_owned(),
            base: "c".to_owned(),
            files: BTreeMap::from([("x.rs".to_owned(), vec![(5, 7)])]),
        };
        assert!(!changed.touches("x.rs", (1, 4)));
        assert!(changed.touches("x.rs", (1, 5)));
        assert!(changed.touches("x.rs", (6, 6)));
        assert!(changed.touches("x.rs", (7, 9)));
        assert!(!changed.touches("x.rs", (8, 9)));
        assert!(!changed.touches("y.rs", (5, 7)));
    }
}
