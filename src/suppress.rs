//! Suppression comments: one model for each check.
//!
//! - `// fallow-ignore-next-line <kind>` hides the findings of the next code
//!   line. Attribute lines, comment lines and blank lines between the comment
//!   and the code do not count, so the comment can stand above `#[derive]`.
//! - `// fallow-ignore-file <kind>` in the first ten lines hides the findings
//!   of the file.
//! - In a `Cargo.toml`, the same markers start with `#`.
//!
//! A kind is a check name (`visibility`), a rule name (`unused-dependency`)
//! or an alias (`code-duplication` for `dupes`). A list takes commas or
//! spaces, and text after ` -- ` is a reason. A marker with no kind hides each
//! finding. A marker with a kind that hides no finding of a check that ran is
//! stale: the check reports a `stale-suppression` warning.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use ra_ap_parser::LexedStr;
use ra_ap_syntax::SyntaxKind;

use crate::parse::Edition;

/// The text that each marker starts with.
const MARKER_TEXT: &str = "fallow-ignore-";

/// The marker that hides the findings of the next code line.
const NEXT_LINE: &str = "fallow-ignore-next-line";

/// The marker that hides the findings of the file.
const FILE: &str = "fallow-ignore-file";

/// The lines at the top of a file that can hold a file marker.
const FILE_MARKER_LINES: u32 = 10;

/// The rule of a stale marker.
pub const STALE_RULE: &str = "stale-suppression";

/// The kinds that each check owns, apart from its name and the names of the
/// boundary rules in the configuration.
const KINDS: &[(&str, &[&str])] = &[
    (
        "deps",
        &[
            "unused-dependency",
            "unused-dev-dependency",
            "unused-build-dependency",
            "unused-workspace-dependency",
            "misplaced-dependency",
            "unused-optional-dependency",
            "feature-only-dependency",
            "redundant-ignore",
            "unused-feature",
            "feature-unification-drift",
        ],
    ),
    ("files", &["unused-file"]),
    (
        "visibility",
        &[
            "dead-public",
            "unnecessary-public",
            "unnecessary-restricted-visibility",
        ],
    ),
    ("boundaries", &[]),
    ("size", &["grown-file", "new-large-file"]),
    ("complexity", &["complexity-crap"]),
    ("dupes", &["code-duplication"]),
];

/// One marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    /// The line of the comment. It starts at 1.
    pub line: u32,
    /// The last line that the marker covers. A file marker covers each line.
    pub covers: Option<u32>,
    /// The kinds, normalized. Empty means each kind.
    pub kinds: Vec<String>,
}

/// The markers of one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Markers {
    /// The markers, in line order.
    pub markers: Vec<Marker>,
}

impl Markers {
    /// The markers in the comments of a Rust source file.
    pub fn of_rust(text: &str) -> Self {
        if !text.contains(MARKER_TEXT) {
            return Self::default();
        }
        let lexed = LexedStr::new(Edition::CURRENT, text);
        let mut markers = Vec::new();
        for index in 0..lexed.len() {
            if lexed.kind(index) != SyntaxKind::COMMENT {
                continue;
            }
            let comment = lexed.text(index);
            // Only the first line of a block comment holds the kinds.
            let body = comment
                .trim_start_matches('/')
                .trim_start_matches('*')
                .trim_start_matches('!')
                .lines()
                .next()
                .unwrap_or_default()
                .trim_end()
                .trim_end_matches("*/");
            if !body.trim_start().starts_with(MARKER_TEXT) {
                continue;
            }
            let line = line_of(text, lexed.text_range(index).start);
            markers.extend(marker(body, line, &|_| next_item_line(&lexed, text, index)));
        }
        Self { markers }
    }

    /// The markers in the `#` comment lines of a `Cargo.toml` file.
    pub fn of_toml(text: &str) -> Self {
        let markers = text
            .lines()
            .zip(1_u32..)
            .filter_map(|(content, line)| {
                let body = content.trim_start().strip_prefix('#')?;
                marker(body, line, &|after| next_toml_line(text, after))
            })
            .collect();
        Self { markers }
    }
}

/// The marker in one comment body, when it has one.
fn marker(body: &str, line: u32, next: &dyn Fn(u32) -> Option<u32>) -> Option<Marker> {
    let body = body.trim_start();
    let (covers, rest) = if let Some(rest) = body.strip_prefix(NEXT_LINE) {
        (Some(next(line).unwrap_or(line)), rest)
    } else {
        let rest = body.strip_prefix(FILE)?;
        if line > FILE_MARKER_LINES {
            return None;
        }
        (None, rest)
    };
    // The marker must end at a word boundary: `fallow-ignore-filex` is no marker.
    if rest.chars().next().is_some_and(|c| !c.is_whitespace()) {
        return None;
    }
    // A `--` token starts the reason.
    let kinds = rest
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|kind| !kind.is_empty())
        .take_while(|kind| *kind != "--")
        .map(normalize)
        .collect();
    Some(Marker {
        line,
        covers,
        kinds,
    })
}

/// A kind in lower case, with `-` for `_`.
fn normalize(kind: &str) -> String {
    kind.to_lowercase().replace('_', "-")
}

/// The one-based line of a byte offset.
fn line_of(text: &str, offset: usize) -> u32 {
    let lines = text
        .get(..offset)
        .map_or(0, |prefix| prefix.matches('\n').count());
    u32::try_from(lines + 1).unwrap_or(u32::MAX)
}

/// The line of the first token after the comment at `comment` that is not
/// trivia and not part of an attribute: the line of the item or statement
/// that the comment stands above.
fn next_item_line(lexed: &LexedStr<'_>, text: &str, comment: usize) -> Option<u32> {
    let mut index = comment + 1;
    loop {
        index = next_solid(lexed, index);
        if index >= lexed.len() || lexed.kind(index) == SyntaxKind::EOF {
            return None;
        }
        if lexed.kind(index) == SyntaxKind::POUND {
            let mut open = next_solid(lexed, index + 1);
            if open < lexed.len() && lexed.kind(open) == SyntaxKind::BANG {
                open = next_solid(lexed, open + 1);
            }
            if open < lexed.len() && lexed.kind(open) == SyntaxKind::L_BRACK {
                index = after_brackets(lexed, open)?;
                continue;
            }
        }
        return Some(line_of(text, lexed.text_range(index).start));
    }
}

/// The index after the bracket group that starts at `open`.
fn after_brackets(lexed: &LexedStr<'_>, open: usize) -> Option<usize> {
    let mut depth = 0_usize;
    for index in open..lexed.len() {
        match lexed.kind(index) {
            SyntaxKind::L_BRACK => depth += 1,
            SyntaxKind::R_BRACK => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            SyntaxKind::EOF => return None,
            _ => {}
        }
    }
    None
}

/// True for whitespace and comments, also doc comments.
fn is_trivia(lexed: &LexedStr<'_>, index: usize) -> bool {
    matches!(
        lexed.kind(index),
        SyntaxKind::WHITESPACE | SyntaxKind::COMMENT
    ) || lexed.text(index).starts_with("//")
        || lexed.text(index).starts_with("/*")
}

/// The index of the first token at or after `index` that is not trivia.
fn next_solid(lexed: &LexedStr<'_>, mut index: usize) -> usize {
    while index < lexed.len() && is_trivia(lexed, index) {
        index += 1;
    }
    index
}

/// The first line after `after` in a TOML file that is not blank and not a
/// comment.
fn next_toml_line(text: &str, after: u32) -> Option<u32> {
    text.lines()
        .zip(1_u32..)
        .skip(usize::try_from(after).unwrap_or(usize::MAX))
        .find(|(content, _)| {
            let content = content.trim_start();
            !(content.is_empty() || content.starts_with('#'))
        })
        .map(|(_, line)| line)
}

/// The markers of the workspace files that the checks read, with the markers
/// that hid a finding.
#[derive(Debug, Default)]
pub struct Registry {
    root: PathBuf,
    /// The boundary rule names of the configuration. They are kinds of
    /// `boundaries`.
    boundary_rules: BTreeSet<String>,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    files: BTreeMap<String, Arc<Markers>>,
    /// The markers that hid a finding, as `(path, line, check)`.
    used: BTreeSet<(String, u32, String)>,
    /// The number of hidden findings of each check.
    hidden: BTreeMap<String, usize>,
    /// The unknown kinds that a warning named already, as `(path, line, kind)`.
    warned: BTreeSet<(String, u32, String)>,
}

/// A marker that hides no finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stale {
    /// The file of the marker.
    pub path: String,
    /// The line of the marker.
    pub line: u32,
    /// The kinds of the marker that belong to the check.
    pub kinds: Vec<String>,
}

impl Registry {
    /// A registry for the files under `root`.
    pub fn new(root: &Path, boundary_rules: impl IntoIterator<Item = String>) -> Self {
        Self {
            root: root.to_path_buf(),
            boundary_rules: boundary_rules
                .into_iter()
                .map(|rule| normalize(&rule))
                .collect(),
            state: Mutex::default(),
        }
    }

    /// True when a marker hides the finding of `check` and `rule` at `lines`
    /// of `path`. `lines` is `None` for a finding of the whole file: then only
    /// a file marker hides it.
    pub fn hides(
        &self,
        check: &str,
        rule: &str,
        path: &str,
        lines: Option<&std::ops::RangeInclusive<u32>>,
    ) -> bool {
        let markers = self.markers(path);
        let rule = normalize(rule);
        let hit = markers.markers.iter().find(|marker| {
            let kind = marker.kinds.is_empty()
                || marker
                    .kinds
                    .iter()
                    .any(|kind| *kind == rule || kind == check || is_alias_of(kind, check));
            // A next-line marker covers the lines after it up to the code
            // line. It hides a finding whose first or last line it covers: a
            // marker in the middle of a long range hides nothing.
            let place = match (lines, marker.covers) {
                (_, None) => true,
                (Some(lines), Some(covers)) => {
                    let span = marker.line + 1..=covers;
                    span.contains(lines.start()) || span.contains(lines.end())
                }
                (None, Some(_)) => false,
            };
            kind && place
        });
        let Some(hit) = hit.map(|marker| marker.line) else {
            return false;
        };
        let mut state = self.lock();
        state.used.insert((path.to_owned(), hit, check.to_owned()));
        *state.hidden.entry(check.to_owned()).or_default() += 1;
        true
    }

    /// The number of findings of `check` that a marker hid.
    pub fn hidden(&self, check: &str) -> usize {
        self.lock().hidden.get(check).copied().unwrap_or(0)
    }

    /// The markers in `paths` with a kind of `check` that hid no finding. A
    /// marker with no kind is never stale: no one check owns it. The function
    /// also warns once for each kind that no check owns.
    pub fn stale<'a>(&self, check: &str, paths: impl IntoIterator<Item = &'a str>) -> Vec<Stale> {
        let mut stale = Vec::new();
        for path in paths {
            let markers = self.markers(path);
            if markers.markers.is_empty() {
                continue;
            }
            let mut state = self.lock();
            for marker in &markers.markers {
                let mine: Vec<String> = marker
                    .kinds
                    .iter()
                    .filter(|kind| self.owner(kind) == Some(check))
                    .cloned()
                    .collect();
                let unknown = marker
                    .kinds
                    .iter()
                    .filter(|kind| self.owner(kind).is_none() && kind.as_str() != STALE_RULE);
                for kind in unknown {
                    if state
                        .warned
                        .insert((path.to_owned(), marker.line, kind.clone()))
                    {
                        crate::render::warn(&format!(
                            "`{path}:{}`: the suppression kind `{kind}` names no check or rule",
                            marker.line
                        ));
                    }
                }
                let used = state
                    .used
                    .contains(&(path.to_owned(), marker.line, check.to_owned()));
                if !mine.is_empty() && !used {
                    stale.push(Stale {
                        path: path.to_owned(),
                        line: marker.line,
                        kinds: mine,
                    });
                }
            }
            drop(state);
        }
        stale.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        stale.dedup();
        stale
    }

    /// The check that owns a kind.
    /// The check that owns a kind. A check name or a rule name wins over a
    /// boundary rule with the same name.
    fn owner(&self, kind: &str) -> Option<&'static str> {
        KINDS
            .iter()
            .find_map(|(check, kinds)| (*check == kind || kinds.contains(&kind)).then_some(*check))
            .or_else(|| self.boundary_rules.contains(kind).then_some("boundaries"))
    }

    /// The markers of a file. The read and the lex run outside the lock.
    fn markers(&self, path: &str) -> Arc<Markers> {
        if let Some(found) = self.lock().files.get(path) {
            return Arc::clone(found);
        }
        let text = fs::read_to_string(self.root.join(path)).unwrap_or_default();
        let toml = Path::new(path)
            .extension()
            .is_some_and(|extension| extension == "toml");
        let parsed = Arc::new(if toml {
            Markers::of_toml(&text)
        } else {
            Markers::of_rust(&text)
        });
        Arc::clone(self.lock().files.entry(path.to_owned()).or_insert(parsed))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The static name of a check, for the shared finding shape.
pub fn check_name(name: &str) -> Option<&'static str> {
    KINDS
        .iter()
        .map(|(check, _)| *check)
        .find(|check| *check == name)
}

/// True when `kind` is an alias of `check`.
fn is_alias_of(kind: &str, check: &str) -> bool {
    kind == "code-duplication" && check == "dupes"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(markers: &Markers) -> Vec<(u32, Option<u32>, Vec<String>)> {
        markers
            .markers
            .iter()
            .map(|marker| (marker.line, marker.covers, marker.kinds.clone()))
            .collect()
    }

    #[test]
    fn rust_markers_cover_attributes_and_take_a_list_and_a_reason() {
        let text = "// fallow-ignore-file unused-file\n\
                    // fallow-ignore-next-line visibility, dead_public -- kept for the API\n\
                    #[derive(Debug)]\n\
                    /// doc\n\
                    pub struct A;\n\
                    /* fallow-ignore-next-line size */\n\
                    fn b() {}\n\
                    const S: &str = \"// fallow-ignore-next-line size\";\n";
        assert_eq!(
            kinds(&Markers::of_rust(text)),
            [
                (1, None, vec!["unused-file".to_owned()]),
                (
                    2,
                    Some(5),
                    vec!["visibility".to_owned(), "dead-public".to_owned()]
                ),
                (6, Some(7), vec!["size".to_owned()]),
            ]
        );
    }

    #[test]
    fn reason_block_lines_and_attributes_with_strings_do_not_change_the_kinds() {
        let text = "// fallow-ignore-next-line size --\nfn a() {}\n\
                    /* fallow-ignore-next-line visibility\n   more words here */\n\
                    #[doc = \"// not a comment\n#[x]\"]\n#[inline]\npub fn b() {}\n";
        assert_eq!(
            kinds(&Markers::of_rust(text)),
            [
                (1, Some(2), vec!["size".to_owned()]),
                (3, Some(8), vec!["visibility".to_owned()]),
            ]
        );
    }

    #[test]
    fn a_marker_in_a_long_range_does_not_hide_it_and_use_counts_for_each_check() {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let text = "fn a() {}\n// fallow-ignore-next-line size, visibility\npub fn b() {}\n";
        let Ok(()) = fs::write(dir.path().join("lib.rs"), text) else {
            panic!("write a file");
        };
        let registry = Registry::new(dir.path(), vec!["size".to_owned()]);
        assert!(!registry.hides("size", "grown-file", "lib.rs", Some(&(1..=900))));
        assert!(registry.hides("visibility", "dead_public", "lib.rs", Some(&(3..=3))));
        assert!(registry.stale("visibility", ["lib.rs"]).is_empty());
        // A boundary rule named `size` does not take the kind from `size`.
        assert_eq!(registry.stale("size", ["lib.rs"]).len(), 1);
    }

    #[test]
    fn a_file_marker_below_line_ten_and_a_longer_word_are_no_markers() {
        let late = format!("{}// fallow-ignore-file size\n", "\n".repeat(10));
        assert!(Markers::of_rust(&late).markers.is_empty());
        assert!(
            Markers::of_rust("// fallow-ignore-filex size\n")
                .markers
                .is_empty()
        );
    }

    #[test]
    fn toml_markers_skip_blank_and_comment_lines() {
        let text =
            "[dependencies]\n# fallow-ignore-next-line unused-dependency\n\n# why\nserde = \"1\"\n";
        assert_eq!(
            kinds(&Markers::of_toml(text)),
            [(2, Some(5), vec!["unused-dependency".to_owned()])]
        );
    }

    #[test]
    fn registry_hides_a_finding_and_reports_the_markers_that_hide_none() {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let text = "// fallow-ignore-next-line visibility\npub fn a() {}\n\
                    // fallow-ignore-next-line size\nfn b() {}\n\
                    // fallow-ignore-next-line nonsense\nfn c() {}\n";
        let Ok(()) = fs::write(dir.path().join("lib.rs"), text) else {
            panic!("write a file");
        };
        let registry = Registry::new(dir.path(), Vec::new());
        assert!(registry.hides("visibility", "dead_public", "lib.rs", Some(&(2..=2))));
        assert!(!registry.hides("visibility", "dead_public", "lib.rs", Some(&(6..=6))));
        assert_eq!(registry.hidden("visibility"), 1);
        assert!(registry.stale("visibility", ["lib.rs"]).is_empty());
        let stale = registry.stale("size", ["lib.rs"]);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].line, 3);
    }
}
