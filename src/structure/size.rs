//! File and function size.
//!
//! The rule is the same as `scripts/check-file-size.mjs` in fallow, for `.rs`
//! files: a file over the line limit must not grow, and a new file must not be
//! over it. A file in a `tests`, `test`, `snapshots` or `__snapshots__` folder,
//! a `tests.rs` file, a `*_test.rs` or `*_tests.rs` file, and a file with a
//! generator header are not checked, because test files grow with each test.
//!
//! The growth rule needs an old version: `--base <ref>` compares HEAD with the
//! merge base, and `--staged` compares the index with HEAD. Without one, the
//! check only reports the hotspots: the files over the line limit and the
//! functions over the function line limit.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use fallow_report_protocol::{GateOutcome, GateStatus};
use serde::Serialize;

use crate::error::{CliError, ErrorCode};
use crate::render::Progress;
use crate::structure::config::{self, matches_prefix};
use crate::structure::git::{self, Change, Changes};
use crate::structure::report::{Action, Gate, Report, count_gate, gates_text, plural};
use crate::structure::sarif::{self, Item, Place};
use crate::structure::source::{self, Needs, count_lines, is_generated};
use crate::structure::{Output, SizeArgs};
use crate::workspace;

/// The folders whose files the growth rule skips.
const SKIPPED_FOLDERS: [&str; 4] = ["tests", "test", "snapshots", "__snapshots__"];
/// The number of hotspots that the human output lists.
const HUMAN_HOTSPOTS: usize = 20;

/// True for a `.rs` file that the rule checks: not in a test or snapshot
/// folder, and not a test file.
pub fn is_checked_path(path: &str) -> bool {
    let mut segments: Vec<&str> = path.split('/').collect();
    let Some(name) = segments.pop() else {
        return false;
    };
    let rust = std::path::Path::new(name)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"));
    if !rust
        || segments
            .iter()
            .any(|segment| SKIPPED_FOLDERS.contains(segment))
    {
        return false;
    }
    !(name == "tests.rs" || name.ends_with("_test.rs") || name.ends_with("_tests.rs"))
}

/// The kind of a growth finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// A file over the limit has more lines than its old version.
    GrownFile,
    /// A new file is over the limit.
    NewLargeFile,
}

/// A file that the growth rule rejects.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    /// The kind.
    pub kind: Kind,
    /// The file.
    pub path: String,
    /// The line count now.
    pub lines: u32,
    /// The line count of the old version, or `None` for a new file.
    pub previous: Option<u32>,
    /// The path of the old version, when a rename changed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    /// The message.
    pub message: String,
    /// The next steps.
    pub actions: Vec<Action>,
}

/// A file over the line limit.
#[derive(Debug, Clone, Serialize)]
pub struct LargeFile {
    /// The file.
    pub path: String,
    /// The line count.
    pub lines: u32,
}

/// A function over the function line limit.
#[derive(Debug, Clone, Serialize)]
pub struct LargeFunction {
    /// The file.
    pub path: String,
    /// The function name.
    pub name: String,
    /// The line of the name.
    pub line: u32,
    /// The lines of the function.
    pub line_count: u32,
}

/// The summary of a size run.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    /// The file line limit.
    pub max_file_lines: u32,
    /// The function line limit.
    pub max_function_lines: u32,
    /// The old version of the growth rule, or `None` without `--base` and
    /// `--staged`.
    pub compared_with: Option<String>,
    /// The changed `.rs` files that the growth rule checked.
    pub checked_changes: usize,
    /// The parsed files.
    pub files: usize,
    /// The checked files over the line limit, largest first.
    pub large_files: Vec<LargeFile>,
    /// The functions over the function line limit, largest first.
    pub large_functions: Vec<LargeFunction>,
    /// The files that a declaration reached but that did not parse.
    pub skipped_files: Vec<String>,
}

pub fn run(args: &SizeArgs) -> Result<Output, CliError> {
    if args.fail_on_growth && args.base.is_none() && !args.staged {
        return Err(CliError::new(
            ErrorCode::InvalidArgument,
            "`--fail-on-growth` needs an old version to compare with",
        )
        .with_help("Add `--base <ref>` or `--staged`."));
    }
    let progress = Progress::new(args.common.quiet);
    progress.step("Reading cargo metadata");
    let workspace = workspace::load(args.common.manifest_path.as_deref())?;
    let settings: config::Size = config::section(&workspace, "size")?;
    let root = workspace.root.as_std_path();
    let changes = match (&args.base, args.staged) {
        (Some(base), _) => Some(git::since_base(root, base)?),
        (None, true) => Some(git::staged(root)?),
        (None, false) => None,
    };
    progress.step("Measuring the files");
    let loaded = source::load(
        &workspace,
        Needs {
            functions: true,
            tokens: None,
        },
    );
    let mut large_files: Vec<LargeFile> = loaded
        .files
        .iter()
        .filter(|file| is_checked_path(&file.path) && !file.generated)
        .filter(|file| !matches_prefix(&file.path, &settings.exclude))
        .filter(|file| file.lines > settings.max_file_lines)
        .map(|file| LargeFile {
            path: file.path.clone(),
            lines: file.lines,
        })
        .collect();
    large_files.sort_by(|a, b| b.lines.cmp(&a.lines).then_with(|| a.path.cmp(&b.path)));
    let mut large_functions: Vec<LargeFunction> = loaded
        .files
        .iter()
        .filter(|file| !matches_prefix(&file.path, &settings.exclude))
        .flat_map(|file| {
            file.functions
                .iter()
                .filter(|function| function.line_count() > settings.max_function_lines)
                .map(|function| LargeFunction {
                    path: file.path.clone(),
                    name: function.name.clone(),
                    line: function.line,
                    line_count: function.line_count(),
                })
        })
        .collect();
    large_functions.sort_by(|a, b| {
        b.line_count
            .cmp(&a.line_count)
            .then_with(|| (&a.path, a.line).cmp(&(&b.path, b.line)))
    });
    let (findings, checked) = changes.as_ref().map_or_else(
        || (Vec::new(), 0),
        |changes| growth(root, changes, &settings),
    );
    let summary = Summary {
        max_file_lines: settings.max_file_lines,
        max_function_lines: settings.max_function_lines,
        compared_with: changes.as_ref().map(|changes| changes.label.clone()),
        checked_changes: checked,
        files: loaded.files.len(),
        large_files,
        large_functions,
        skipped_files: loaded
            .skipped
            .iter()
            .map(|file| file.path.clone())
            .collect(),
    };
    let mut report = Report::new("size", summary, findings);
    let outcome = if changes.is_some() {
        count_gate(report.findings.len(), args.fail_on_growth)
    } else {
        GateOutcome::new(GateStatus::Skipped, false)
    };
    report.gate_outcomes.insert(Gate::FileGrowth, outcome);
    Ok(Output {
        human: human(&report),
        json: serde_json::to_value(&report)
            .map_err(|err| CliError::output(&std::io::Error::other(err)))?,
        sarif: sarif_log(&report, args.fail_on_growth),
        fails: report.fails_run(),
    })
}

/// The growth findings and the number of checked changes. The rule reads an
/// old blob only for a file over the limit, because a partial clone fetches
/// each missing blob over the network.
fn growth(
    root: &std::path::Path,
    changes: &Changes,
    settings: &config::Size,
) -> (Vec<Finding>, usize) {
    let checked: Vec<&Change> = changes
        .changes
        .iter()
        .filter(|change| is_checked_path(&change.path))
        .filter(|change| !matches_prefix(&change.path, &settings.exclude))
        .collect();
    let mut findings: Vec<Finding> = checked
        .iter()
        .filter_map(|change| {
            let text = git::blob(root, &change.new_spec)?;
            let lines = count_lines(&text);
            if lines <= settings.max_file_lines || is_generated(&text) {
                return None;
            }
            let previous = change
                .old_specs
                .iter()
                .filter_map(|spec| git::blob(root, spec))
                .map(|old| count_lines(&old))
                .max();
            if previous.is_some_and(|old| lines <= old) {
                return None;
            }
            Some(finding(
                change,
                lines,
                previous,
                &changes.label,
                settings.max_file_lines,
            ))
        })
        .collect();
    findings.sort_by(|a, b| a.path.cmp(&b.path));
    (findings, checked.len())
}

fn finding(change: &Change, lines: u32, previous: Option<u32>, label: &str, limit: u32) -> Finding {
    let path = change.path.clone();
    let renamed = (change.old_path != change.path).then(|| change.old_path.clone());
    let (kind, message) = match previous {
        None => (
            Kind::NewLargeFile,
            format!(
                "{path}: new file with {lines} lines. The limit for a new file is {limit} lines."
            ),
        ),
        Some(old) => {
            let source = renamed.as_ref().map_or_else(
                || label.to_owned(),
                |old_path| format!("{label} ({old_path})"),
            );
            (
                Kind::GrownFile,
                format!(
                    "{path}: {lines} lines, {source} has {old}. A file over {limit} lines must not grow."
                ),
            )
        }
    };
    Finding {
        kind,
        path,
        lines,
        previous,
        old_path: renamed,
        message,
        actions: vec![Action::manual(
            "split-file",
            "Move the new code to a new module, or remove lines from the file in the same change",
        )],
    }
}

fn human(report: &Report<Summary, Finding>) -> String {
    let summary = &report.summary;
    let mut text = format!(
        "cargo fallow size: {} {}, limits {} lines for a file and {} for a function\n",
        summary.files,
        plural(summary.files, "file", "files"),
        summary.max_file_lines,
        summary.max_function_lines,
    );
    match &summary.compared_with {
        Some(label) => {
            let _ = writeln!(
                text,
                "Growth rule: {} changed {} against {label}",
                summary.checked_changes,
                plural(summary.checked_changes, "file", "files"),
            );
            if report.findings.is_empty() {
                text.push_str("No file over the limit grew\n");
            }
            for finding in &report.findings {
                let _ = writeln!(text, "  {}", finding.message);
            }
        }
        None => text.push_str("Growth rule: not run. Add `--base <ref>` or `--staged`.\n"),
    }
    hotspots(
        &mut text,
        "Files over the limit",
        summary
            .large_files
            .iter()
            .map(|file| format!("{} ({} lines)", file.path, file.lines)),
    );
    hotspots(
        &mut text,
        "Functions over the limit",
        summary.large_functions.iter().map(|function| {
            format!(
                "{}:{} {} ({} lines)",
                function.path, function.line, function.name, function.line_count
            )
        }),
    );
    for path in &summary.skipped_files {
        let _ = writeln!(text, "  skipped {path}: the file does not parse");
    }
    text.push_str(&gates_text(&report.gate_outcomes));
    text
}

fn hotspots(text: &mut String, title: &str, lines: impl ExactSizeIterator<Item = String>) {
    let count = lines.len();
    if count == 0 {
        return;
    }
    let _ = writeln!(text, "{title}: {count}");
    for line in lines.take(HUMAN_HOTSPOTS) {
        let _ = writeln!(text, "  {line}");
    }
    if count > HUMAN_HOTSPOTS {
        let _ = writeln!(
            text,
            "  and {} more. Use `--format json` for the full list.",
            count - HUMAN_HOTSPOTS
        );
    }
}

fn sarif_log(report: &Report<Summary, Finding>, enforced: bool) -> serde_json::Value {
    let rules = BTreeMap::from([
        (
            "grown-file".to_owned(),
            "A file over the line limit grew.".to_owned(),
        ),
        (
            "new-large-file".to_owned(),
            "A new file is over the line limit.".to_owned(),
        ),
    ]);
    let items: Vec<Item> = report
        .findings
        .iter()
        .map(|finding| Item {
            rule: match finding.kind {
                Kind::GrownFile => "grown-file",
                Kind::NewLargeFile => "new-large-file",
            }
            .to_owned(),
            level: sarif::level(enforced),
            message: finding.message.clone(),
            place: Place {
                path: finding.path.clone(),
                line: 1,
                column: 1,
                end_line: None,
            },
            related: Vec::new(),
        })
        .collect();
    sarif::log(&rules, &items, &report.gate_outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_paths_skip_tests_snapshots_and_other_extensions() {
        assert!(is_checked_path("crates/a/src/lib.rs"));
        assert!(is_checked_path("src/main.RS"));
        assert!(!is_checked_path("crates/a/tests/it.rs"));
        assert!(!is_checked_path("crates/a/src/test/x.rs"));
        assert!(!is_checked_path("crates/a/src/snapshots/x.rs"));
        assert!(!is_checked_path("src/tests.rs"));
        assert!(!is_checked_path("src/parse_test.rs"));
        assert!(!is_checked_path("src/parse_tests.rs"));
        assert!(!is_checked_path("src/x.mjs"));
        assert!(is_checked_path("src/attests.rs"));
        assert!(is_checked_path("src/tests_util.rs"));
    }

    fn change(path: &str, old_path: &str) -> Change {
        Change {
            path: path.to_owned(),
            old_path: old_path.to_owned(),
            new_spec: String::new(),
            old_specs: Vec::new(),
        }
    }

    #[test]
    fn messages_match_the_script() {
        let grown = finding(
            &change("a.rs", "a.rs"),
            2010,
            Some(2001),
            "merge base abc",
            2000,
        );
        assert_eq!(
            grown.message,
            "a.rs: 2010 lines, merge base abc has 2001. A file over 2000 lines must not grow."
        );
        let renamed = finding(&change("b.rs", "a.rs"), 2010, Some(2001), "HEAD", 2000);
        assert_eq!(
            renamed.message,
            "b.rs: 2010 lines, HEAD (a.rs) has 2001. A file over 2000 lines must not grow."
        );
        let new = finding(&change("c.rs", "c.rs"), 2500, None, "HEAD", 2000);
        assert_eq!(new.kind, Kind::NewLargeFile);
        assert_eq!(
            new.message,
            "c.rs: new file with 2500 lines. The limit for a new file is 2000 lines."
        );
    }
}
