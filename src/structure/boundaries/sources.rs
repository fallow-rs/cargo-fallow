//! The `scope` and `allow` keys of a source rule.
//!
//! - `scope = "production"` skips the files of test targets, the files below
//!   a test module declaration and the items with a test attribute. A file
//!   that the source walk does not reach counts as production code.
//! - `allow` gives a file a count. The rule counts each place in the file
//!   where a pattern matches, in file order. One place counts once, also when
//!   two patterns match there. Each place after the first `max` places is a
//!   finding.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::ops::Range;
use std::path::Path;

use ra_ap_syntax::{AstNode, WalkEvent};

use super::{Finding, Lexed, invalid, line_col, source_finding};
use crate::attrs::{AttrClass, attributes_of};
use crate::error::CliError;
use crate::parse::{Edition, with_parsed};
use crate::render::warn;
use crate::structure::config::{Matching, SourceRule, SourceScope};
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

/// The `allow` counts of the rule, by path. An entry must name a file that
/// the rule reads, one time.
pub fn allowed(
    rule: &SourceRule,
    files: &BTreeSet<String>,
) -> Result<BTreeMap<String, u32>, CliError> {
    let mut counts = BTreeMap::new();
    for entry in &rule.allow {
        let path = normalize(&entry.path);
        if !files.contains(&path) {
            return Err(invalid(&format!(
                "rule `{}` allows `{}`, which is not a file that the rule reads",
                rule.rule, entry.path
            )));
        }
        if counts.insert(path, entry.max).is_some() {
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

/// The findings of one file. `max` is the `allow` count of the file.
pub fn judge(
    workspace: &Workspace,
    rule: &SourceRule,
    path: &str,
    code: &Code,
    patterns: &[(String, Vec<String>)],
    max: Option<u32>,
) -> Vec<Finding> {
    let tokens = (rule.matching == Matching::Tokens).then(|| Lexed::new(&code.text));
    let mut found = Vec::new();
    let mut places: BTreeMap<usize, &str> = BTreeMap::new();
    for (pattern, wanted) in patterns {
        let offsets: Vec<usize> = match &tokens {
            Some(tokens) => tokens.find_all(wanted),
            None => code
                .text
                .match_indices(pattern.as_str())
                .map(|(at, _)| at)
                .collect(),
        };
        let mut offsets = offsets.into_iter().filter(|&at| !code.in_test(at));
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

/// Reads one file for the rule. Returns `None` with a warning when the rule
/// cannot judge the file: the file cannot be read, or the production scope
/// needs a parse and the file does not parse.
pub fn read(root: &Path, path: &str, rule: &SourceRule) -> Option<Code> {
    let Ok(text) = fs::read_to_string(root.join(path)) else {
        warn(&format!(
            "cannot read `{path}`, so the rule `{}` does not judge it",
            rule.rule
        ));
        return None;
    };
    if rule.scope == SourceScope::All {
        return Some(Code {
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
    match parsed {
        Ok((text, tests)) => Some(Code { text, tests }),
        Err(outcome) => {
            warn(&format!(
                "the rule `{}` does not judge `{path}`: {}",
                rule.rule,
                source::parse_reason(outcome)
            ));
            None
        }
    }
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
    fn an_allowed_path_gets_slashes_and_loses_dot_parts() {
        assert_eq!(normalize("./crates\\a//b.rs"), "crates/a/b.rs");
    }
}
