//! The `scope` and `allow` keys of a source rule.
//!
//! - `scope = "production"` skips the files of test targets, the files below
//!   a test module declaration and the items with a test attribute. A file
//!   that the source walk does not reach counts as production code.
//! - `allow` gives a file a count. The rule counts each place in the file
//!   where a pattern matches, in file order. One place counts once, also when
//!   two patterns match there. Each place after the first `max` places is a
//!   finding. A file with fewer places than `max` is an `allow-entry`
//!   finding, so the count stays exact. A `require` pattern that the file
//!   does not contain is an `allow-entry` finding too.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use ra_ap_syntax::{AstNode, WalkEvent};

use super::{Finding, Kind, Lexed, invalid, lex, line_col, package_of, source_finding};
use crate::attrs::{AttrClass, attributes_of};
use crate::error::CliError;
use crate::parse::{Edition, with_parsed};
use crate::render::warn;
use crate::structure::config::{AllowedFile, Matching, SourceRule, SourceScope};
use crate::structure::report::Action;
use crate::structure::source::{self, Needs};
use crate::workspace::Workspace;

/// The files of test code: the files of test targets and the files below a
/// test module declaration. A file that other code also reaches is not in the
/// set.
pub fn test_files(workspace: &Workspace) -> BTreeSet<String> {
    source::load(workspace, Needs::default())
        .files
        .into_iter()
        .filter(|file| file.test_only)
        .map(|file| file.path)
        .collect()
}

/// The `allow` entries of the rule, by path. An entry must name a file that
/// the rule reads, one time, and a `require` pattern must not be empty.
pub fn allowed<'r>(
    rule: &'r SourceRule,
    files: &BTreeSet<String>,
) -> Result<BTreeMap<String, &'r AllowedFile>, CliError> {
    let mut counts = BTreeMap::new();
    for entry in &rule.allow {
        let path = normalize(&entry.path);
        if !files.contains(&path) {
            return Err(invalid(&format!(
                "rule `{}` allows `{}`, which is not a file that the rule reads",
                rule.rule, entry.path
            )));
        }
        if let Some(pattern) = entry.require.iter().find(|pattern| lex(pattern).is_empty()) {
            return Err(invalid(&format!(
                "rule `{}` requires the empty pattern `{pattern}` in `{}`",
                rule.rule, entry.path
            )));
        }
        if counts.insert(path, entry).is_some() {
            return Err(invalid(&format!(
                "rule `{}` allows `{}` more than one time",
                rule.rule, entry.path
            )));
        }
    }
    Ok(counts)
}

/// A path relative to the workspace root, with `/` and without `./`.
fn normalize(path: &str) -> String {
    let path = path.replace('\\', "/");
    path.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// The offsets of the matches of one pattern outside the skipped test code.
fn matches(code: &Code, tokens: Option<&Lexed>, pattern: &str, wanted: &[String]) -> Vec<usize> {
    let offsets: Vec<usize> = match tokens {
        Some(tokens) => tokens.find_all(wanted),
        None => code.text.match_indices(pattern).map(|(at, _)| at).collect(),
    };
    offsets
        .into_iter()
        .filter(|&at| !code.in_test(at))
        .collect()
}

/// The findings of one file. `allow` is the `allow` entry of the file.
fn judge(
    workspace: &Workspace,
    rule: &SourceRule,
    path: &str,
    code: &Code,
    patterns: &[(String, Vec<String>)],
    allow: Option<&AllowedFile>,
) -> Vec<Finding> {
    let tokens = (rule.matching == Matching::Tokens).then(|| Lexed::new(&code.text));
    let max = allow.map(|entry| entry.max);
    let mut found = Vec::new();
    let mut places: BTreeMap<usize, &str> = BTreeMap::new();
    for (pattern, wanted) in patterns {
        let mut offsets = matches(code, tokens.as_ref(), pattern, wanted).into_iter();
        // A file outside `allow` gives one finding for each pattern: the first
        // match. A file in `allow` counts each place.
        if max.is_some() {
            for at in offsets {
                places.entry(at).or_insert(pattern.as_str());
            }
        } else if let Some(at) = offsets.next() {
            let (line, col) = line_col(&code.text, at);
            found.push(source_finding(workspace, rule, path, pattern, line, col));
        }
    }
    let Some(max) = max else {
        return found;
    };
    let count = places.len();
    if let Some(entry) = allow {
        found.extend(entry_findings(
            workspace,
            rule,
            path,
            entry,
            count,
            |pattern| !matches(code, tokens.as_ref(), pattern, &lex(pattern)).is_empty(),
        ));
    }
    let skip = usize::try_from(max).unwrap_or(usize::MAX);
    for (index, (at, pattern)) in places.into_iter().enumerate().skip(skip) {
        let (line, col) = line_col(&code.text, at);
        let mut finding = source_finding(workspace, rule, path, pattern, line, col);
        if rule.message.is_none() {
            let _ = write!(
                finding.message,
                " (match {} of the rule, over the allowed {max})",
                index + 1
            );
        }
        found.push(finding);
    }
    found
}

/// The `allow-entry` findings of one allowed file: a count under `max`, and
/// each `require` pattern that `contains` does not find.
fn entry_findings(
    workspace: &Workspace,
    rule: &SourceRule,
    path: &str,
    entry: &AllowedFile,
    count: usize,
    contains: impl Fn(&str) -> bool,
) -> Vec<Finding> {
    let mut found = Vec::new();
    if count < usize::try_from(entry.max).unwrap_or(usize::MAX) {
        found.push(entry_finding(
            workspace,
            rule,
            path,
            None,
            format!(
                "{path} has {count} {} of the rule `{}`, fewer than its `max = {}`",
                if count == 1 { "match" } else { "matches" },
                rule.rule,
                entry.max
            ),
            Action::manual(
                "lower-max",
                format!(
                    "Set `max = {count}` for `{path}` in the rule, so a new match fails the rule"
                ),
            ),
        ));
    }
    for pattern in entry.require.iter().filter(|pattern| !contains(pattern)) {
        found.push(entry_finding(
            workspace,
            rule,
            path,
            Some(pattern),
            format!(
                "{path} does not contain {pattern}, which the rule `{}` requires",
                rule.rule
            ),
            Action::manual(
                "add-required",
                format!("Use `{pattern}` in `{path}`, or remove the entry of the file"),
            ),
        ));
    }
    found
}

fn entry_finding(
    workspace: &Workspace,
    rule: &SourceRule,
    path: &str,
    pattern: Option<&str>,
    message: String,
    action: Action,
) -> Finding {
    Finding {
        rule: rule.rule.clone(),
        kind: Kind::AllowEntry,
        from: package_of(workspace, path),
        to: rule.owner.clone().unwrap_or_default(),
        path: path.to_owned(),
        line: 1,
        col: 1,
        pattern: pattern.map(str::to_owned),
        dependency_kinds: Vec::new(),
        origin: None,
        message,
        actions: vec![action],
    }
}

/// The editions that a parse tries, newest first.
const EDITIONS: [Edition; 4] = [
    Edition::Edition2024,
    Edition::Edition2021,
    Edition::Edition2018,
    Edition::Edition2015,
];

/// The text of one file and the byte ranges of its test items.
pub struct Code {
    /// The text. In the production scope, the text has LF line endings.
    pub text: String,
    tests: Vec<Range<usize>>,
}

impl Code {
    /// True when the offset is in a test item.
    pub fn in_test(&self, offset: usize) -> bool {
        self.tests.iter().any(|range| range.contains(&offset))
    }
}

/// Judges each file of the rule and returns the findings and the number of
/// judged files. The files are read and parsed on parallel threads. The
/// findings and the warnings keep the order of `files`.
pub fn judge_files(
    workspace: &Workspace,
    rule: &SourceRule,
    files: &BTreeSet<String>,
    patterns: &[(String, Vec<String>)],
    allowed: &BTreeMap<String, &AllowedFile>,
) -> (Vec<Finding>, usize) {
    let root = workspace.root.as_std_path();
    let files: Vec<&String> = files.iter().collect();
    let judged = parallel_map(&files, |path| -> Result<Vec<Finding>, String> {
        let code = read(root, path, rule)?;
        let allow = allowed.get(path.as_str()).copied();
        Ok(judge(workspace, rule, path, &code, patterns, allow))
    });
    let mut found = Vec::new();
    let mut count = 0;
    for result in judged {
        match result {
            Ok(findings) => {
                count += 1;
                found.extend(findings);
            }
            Err(message) => warn(&message),
        }
    }
    (found, count)
}

/// Maps each item on parallel threads and keeps the order of `items`.
fn parallel_map<T: Sync, R: Send>(items: &[T], map: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let workers = thread::available_parallelism()
        .map_or(1, usize::from)
        .clamp(1, crate::graph::MAX_WORKERS)
        .min(items.len().max(1));
    let next = AtomicUsize::new(0);
    let mut results: Vec<(usize, R)> = thread::scope(|scope| {
        let handles: Vec<_> = std::iter::repeat_with(|| {
            scope.spawn(|| {
                let mut done = Vec::new();
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else {
                        break done;
                    };
                    done.push((index, map(item)));
                }
            })
        })
        .take(workers)
        .collect();
        handles
            .into_iter()
            .flat_map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}

/// Reads one file for the rule. Returns the warning when the rule cannot
/// judge the file: the file cannot be read, or the production scope needs a
/// parse and the file does not parse.
fn read(root: &Path, path: &str, rule: &SourceRule) -> Result<Code, String> {
    let Ok(text) = fs::read_to_string(root.join(path)) else {
        return Err(format!(
            "cannot read `{path}`, so the rule `{}` does not judge it",
            rule.rule
        ));
    };
    if rule.scope == SourceScope::All {
        return Ok(Code {
            text,
            tests: Vec::new(),
        });
    }
    // The file can belong to a package of any edition, so each edition gets a
    // try. The test attributes do not depend on the edition.
    let mut parsed = Err(crate::parse::ParseOutcome::Parsed);
    for edition in EDITIONS {
        parsed = with_parsed(&text, edition, |file, normalized| {
            (normalized.to_owned(), test_ranges(file.syntax()))
        });
        if parsed.is_ok() {
            break;
        }
    }
    parsed
        .map(|(text, tests)| Code { text, tests })
        .map_err(|outcome| {
            format!(
                "the rule `{}` does not judge `{path}`: {}",
                rule.rule,
                source::parse_reason(outcome)
            )
        })
}

/// The byte ranges of the outermost items with a test attribute.
fn test_ranges(root: &ra_ap_syntax::SyntaxNode) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut walk = root.preorder();
    while let Some(event) = walk.next() {
        let WalkEvent::Enter(node) = event else {
            continue;
        };
        if attributes_of(&node) == AttrClass::Test {
            let range = node.text_range();
            ranges.push(usize::from(range.start())..usize::from(range.end()));
            walk.skip_subtree();
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(source: &str) -> Vec<Range<usize>> {
        with_parsed(source, Edition::CURRENT, |file, _| {
            test_ranges(file.syntax())
        })
        .unwrap_or_default()
    }

    #[test]
    fn a_test_item_and_an_inner_test_attribute_give_a_range() {
        let source = "fn a() {}\n#[cfg(test)]\nmod t { fn b() {} }\n";
        assert_eq!(ranges(source), vec![10..42]);
        assert_eq!(ranges("#![cfg(test)]\nfn a() {}\n"), vec![0..24]);
        assert_eq!(
            ranges("#[cfg(not(test))]\nfn a() {}\n"),
            Vec::<Range<usize>>::new()
        );
    }

    #[test]
    fn the_parallel_map_keeps_the_order_of_the_items() {
        let items: Vec<usize> = (0..1000).collect();
        let mapped = parallel_map(&items, |item| item * 2);
        assert_eq!(
            mapped,
            items.iter().map(|item| item * 2).collect::<Vec<_>>()
        );
        assert!(parallel_map(&[] as &[usize], |item| *item).is_empty());
    }

    #[test]
    fn an_allowed_path_gets_slashes_and_loses_dot_parts() {
        assert_eq!(normalize("./crates\\a//b.rs"), "crates/a/b.rs");
    }
}
