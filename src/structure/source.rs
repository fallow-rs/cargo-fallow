//! The source files of the workspace for the size, complexity and duplication
//! checks.
//!
//! The walk in [`crate::modules`] starts at the root file of each target and
//! follows the `mod` declarations. Each file is parsed once, through the
//! bounded parse, and gives plain data: its line count, its functions and, for
//! the duplication check, its normalized tokens.

use std::fs;
use std::path::Path;

use ra_ap_syntax::ast::{self, HasAttrs, HasModuleItem, HasName};
use ra_ap_syntax::{AstNode, SyntaxKind, TextSize};
use serde::Serialize;

use crate::attrs::{path_attribute, plain_name};
use crate::gates::parse_edition;
use crate::modules::{ModuleDecl, SkippedFile, TargetRoot, walk};
use crate::parse::{ParseOutcome, with_parsed};
use crate::structure::complexity::{Function, functions};
use crate::structure::config::Mode;
use crate::structure::dupes::tokens::{Token, tokens};
use crate::workspace::Workspace;

/// What a load computes for each file.
#[derive(Debug, Clone, Copy, Default)]
pub struct Needs {
    /// The functions with their size and complexity.
    pub functions: bool,
    /// The token stream for the duplication check, with this mode and the
    /// import setting.
    pub tokens: Option<(Mode, bool)>,
}

/// One source file.
#[derive(Debug, Clone)]
pub struct File {
    /// The path relative to the workspace root, with `/` separators.
    pub path: String,
    /// The line count, the same as `wc -l` plus a last line without a newline.
    pub lines: u32,
    /// True when the first lines hold a generator header.
    pub generated: bool,
    /// True when the first lines hold a generator header or an `@generated`
    /// comment. The complexity and duplication checks skip such a file.
    pub skip_metrics: bool,
    /// The functions, when the load asked for them.
    pub functions: Vec<Function>,
    /// The tokens, when the load asked for them.
    pub tokens: Vec<Token>,
    /// The lines of the `fallow-ignore-next-line` comments, with their kind.
    pub suppressions: Vec<(u32, String)>,
}

impl File {
    /// True when a comment on `line` or on a line in `lines` suppresses `kind`.
    pub fn suppressed(&self, kind: &str, lines: std::ops::RangeInclusive<u32>) -> bool {
        self.suppressions
            .iter()
            .any(|(line, found)| found == kind && lines.contains(line))
    }
}

/// The source files of the workspace.
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    /// The files, sorted by path.
    pub files: Vec<File>,
    /// The files that a declaration reached, but that did not parse.
    pub skipped: Vec<SkippedFile>,
}

/// Walks the module tree of each target of each member.
pub fn load(workspace: &Workspace, needs: Needs) -> Loaded {
    let root = workspace.root.as_std_path();
    let roots: Vec<TargetRoot> = workspace
        .members
        .iter()
        .flat_map(|member| &member.targets)
        .map(|target| TargetRoot {
            path: target.src_path.clone(),
            edition: parse_edition(&target.edition),
        })
        .collect();
    let walked = walk(root, &roots, |visit| {
        load_file(root, &visit.path, visit.edition, needs)
    });
    Loaded {
        files: walked.files,
        skipped: walked.skipped,
    }
}

fn load_file(
    root: &Path,
    path: &str,
    edition: crate::parse::Edition,
    needs: Needs,
) -> Result<(File, Vec<ModuleDecl>), String> {
    let source = fs::read_to_string(root.join(path))
        .map_err(|err| format!("cannot read the file: {err}"))?;
    let lines = count_lines(&source);
    let generated = is_generated(&source);
    let skip_metrics = generated || has_generated_marker(&source);
    let (functions, tokens, suppressions, modules) = with_parsed(&source, edition, |file, text| {
        let index = LineIndex::new(text);
        let functions = if needs.functions && !skip_metrics {
            functions(file.syntax(), &index)
        } else {
            Vec::new()
        };
        let tokens = needs
            .tokens
            .filter(|_| !skip_metrics)
            .map(|(mode, ignore_imports)| tokens(file.syntax(), &index, mode, ignore_imports))
            .unwrap_or_default();
        (
            functions,
            tokens,
            suppressions(file.syntax(), &index),
            declarations(file),
        )
    })
    .map_err(parse_reason)?;
    Ok((
        File {
            path: path.to_owned(),
            lines,
            generated,
            skip_metrics,
            functions,
            tokens,
            suppressions,
        },
        modules,
    ))
}

/// The reason text of a skipped file for a parse outcome.
pub fn parse_reason(outcome: ParseOutcome) -> String {
    match outcome {
        ParseOutcome::TooDeep => "the file nests too deep for the parser".to_owned(),
        ParseOutcome::Panicked => "the parser stopped on the file".to_owned(),
        ParseOutcome::Failed { error_count } => format!(
            "the file does not parse ({error_count} {})",
            if error_count == 1 { "error" } else { "errors" }
        ),
        ParseOutcome::Parsed => "the file parsed".to_owned(),
    }
}

/// A file that a check skips, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Skipped {
    /// The path relative to the workspace root.
    pub path: String,
    /// The reason.
    pub reason: String,
}

impl Loaded {
    /// The files that the walk reached but could not use.
    pub fn skipped_files(&self) -> Vec<Skipped> {
        self.skipped
            .iter()
            .map(|file| Skipped {
                path: file.path.clone(),
                reason: file.reason.clone(),
            })
            .collect()
    }
}

/// True for whitespace and for each kind of comment, also doc comments. The
/// tree gives a doc comment its own kind, which `is_trivia` does not cover.
pub fn is_space_or_comment(kind: SyntaxKind) -> bool {
    kind.is_trivia()
        || matches!(
            kind,
            SyntaxKind::DOC_COMMENT | SyntaxKind::INNER_DOC_COMMENT | SyntaxKind::OUTER_DOC_COMMENT
        )
}

/// Counts lines the way `wc -l` does, plus a last line without a newline.
pub fn count_lines(text: &str) -> u32 {
    let newlines = text.bytes().filter(|&byte| byte == b'\n').count();
    let partial = usize::from(!text.is_empty() && !text.ends_with('\n'));
    u32::try_from(newlines + partial).unwrap_or(u32::MAX)
}

/// The number of first lines that can hold a generator header.
const GENERATED_HEADER_LINES: usize = 10;

/// True when one of the first lines is a generator header line:
/// `AUTOGENERATED FILE. DO NOT EDIT.` or `Generated from|by <x>. Do not edit.`
/// after a comment opener. A comment that only names the words does not match.
pub fn is_generated(text: &str) -> bool {
    text.split('\n')
        .take(GENERATED_HEADER_LINES)
        .any(is_generated_header)
}

/// True when a comment in the first lines holds `@generated`, the marker that
/// code generators for Rust write.
pub fn has_generated_marker(text: &str) -> bool {
    text.split('\n').take(GENERATED_HEADER_LINES).any(|line| {
        let line = line.trim_start();
        (line.starts_with("//") || line.starts_with("/*") || line.starts_with('*'))
            && line.contains("@generated")
    })
}

fn is_generated_header(line: &str) -> bool {
    let line = line.trim();
    let rest = if let Some(block) = line.strip_prefix("/*") {
        block.trim_start_matches('*')
    } else if let Some(rest) = ["//", "*", "#"]
        .iter()
        .find_map(|opener| line.strip_prefix(opener))
    {
        rest
    } else {
        return false;
    };
    let rest = rest.trim();
    let rest = rest.strip_suffix("*/").map_or(rest, str::trim_end);
    if rest == "AUTOGENERATED FILE. DO NOT EDIT." {
        return true;
    }
    let Some(tail) = rest
        .strip_prefix("Generated from ")
        .or_else(|| rest.strip_prefix("Generated by "))
    else {
        return false;
    };
    tail.strip_suffix(". Do not edit.")
        .is_some_and(|source| source.chars().next().is_some_and(|c| !c.is_whitespace()))
}

/// The line and column of each offset in a text. Both start at 1, and the
/// column counts characters.
pub struct LineIndex<'a> {
    text: &'a str,
    starts: Vec<usize>,
    /// True for each line with only ASCII text. On such a line the column is
    /// the byte offset, so a long line needs no count of its characters.
    ascii: Vec<bool>,
}

impl<'a> LineIndex<'a> {
    /// The index of `text`.
    pub fn new(text: &'a str) -> Self {
        let starts: Vec<usize> = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        let ascii = starts
            .iter()
            .enumerate()
            .map(|(line, &start)| {
                let end = starts.get(line + 1).copied().unwrap_or(text.len());
                text.as_bytes()[start..end].is_ascii()
            })
            .collect();
        Self {
            text,
            starts,
            ascii,
        }
    }

    /// The line and the column of `offset`.
    pub fn position(&self, offset: TextSize) -> (u32, u32) {
        let offset = usize::from(offset);
        let line = self.starts.partition_point(|&start| start <= offset);
        let start = self
            .starts
            .get(line.saturating_sub(1))
            .copied()
            .unwrap_or(0);
        let column = if self
            .ascii
            .get(line.saturating_sub(1))
            .copied()
            .unwrap_or(false)
        {
            offset.saturating_sub(start)
        } else {
            self.text
                .get(start..offset)
                .map_or(0, |part| part.chars().count())
        };
        (
            u32::try_from(line).unwrap_or(u32::MAX),
            u32::try_from(column + 1).unwrap_or(u32::MAX),
        )
    }

    /// The line of the last character before `end`.
    pub fn end_line(&self, end: TextSize) -> u32 {
        self.position(end.checked_sub(TextSize::from(1)).unwrap_or(end))
            .0
    }
}

/// The comment that suppresses a finding on the next line.
const SUPPRESS_PREFIX: &str = "fallow-ignore-next-line";

/// The `// fallow-ignore-next-line <kind>` comments, with their line.
fn suppressions(root: &ra_ap_syntax::SyntaxNode, index: &LineIndex<'_>) -> Vec<(u32, String)> {
    root.descendants_with_tokens()
        .filter_map(ra_ap_syntax::NodeOrToken::into_token)
        .filter(|token| token.kind() == SyntaxKind::COMMENT)
        .filter_map(|token| {
            let text = token.text();
            let at = text.find(SUPPRESS_PREFIX)?;
            let kind = text[at + SUPPRESS_PREFIX.len()..]
                .split_whitespace()
                .next()?
                .trim_end_matches("*/")
                .to_owned();
            Some((index.position(token.text_range().start()).0, kind))
        })
        .collect()
}

/// The `mod name;` declarations of a file, also in inline modules.
fn declarations(file: &ast::SourceFile) -> Vec<ModuleDecl> {
    let mut found = Vec::new();
    collect_declarations(file.items(), &mut Vec::new(), &mut found);
    found
}

fn collect_declarations(
    items: impl Iterator<Item = ast::Item>,
    inline: &mut Vec<String>,
    found: &mut Vec<ModuleDecl>,
) {
    for item in items {
        let ast::Item::Module(module) = item else {
            continue;
        };
        let Some(name) = module.name() else {
            continue;
        };
        let name = plain_name(&name);
        if let Some(list) = module.item_list() {
            inline.push(name);
            collect_declarations(list.items(), inline, found);
            inline.pop();
            continue;
        }
        found.push(ModuleDecl {
            name,
            inline_path: inline.clone(),
            path_attr: module.attrs().find_map(|attr| path_attribute(&attr)),
            under_cfg: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_count_matches_wc_and_counts_a_last_partial_line() {
        assert_eq!(count_lines(""), 0);
        assert_eq!(count_lines("a"), 1);
        assert_eq!(count_lines("a\n"), 1);
        assert_eq!(count_lines("a\nb"), 2);
        assert_eq!(count_lines("\n\n"), 2);
    }

    #[test]
    fn generated_header_needs_the_exact_line_in_the_first_ten_lines() {
        assert!(is_generated(
            "// AUTOGENERATED FILE. DO NOT EDIT.\nfn a() {}\n"
        ));
        assert!(is_generated("/* Generated by build.rs. Do not edit. */\n"));
        assert!(is_generated(
            "//! x\n// Generated from schema.json. Do not edit.\n"
        ));
        assert!(!is_generated(
            "// This file is not generated. Do not edit it by hand.\n"
        ));
        assert!(!is_generated("// Generated by . Do not edit.\n"));
        assert!(!is_generated("//* AUTOGENERATED FILE. DO NOT EDIT.\n"));
        assert!(!is_generated("/// AUTOGENERATED FILE. DO NOT EDIT.\n"));
        assert!(is_generated("/** AUTOGENERATED FILE. DO NOT EDIT. */\n"));
        let late = format!("{}// AUTOGENERATED FILE. DO NOT EDIT.\n", "\n".repeat(10));
        assert!(!is_generated(&late));
    }

    #[test]
    fn the_generated_marker_must_be_in_a_comment_in_the_first_lines() {
        assert!(has_generated_marker(
            "// This file is @generated by x.\nfn a() {}\n"
        ));
        assert!(!has_generated_marker("const A: &str = \"@generated\";\n"));
        let late = format!("{}// @generated\n", "\n".repeat(10));
        assert!(!has_generated_marker(&late));
    }

    #[test]
    fn positions_start_at_one_and_count_characters() {
        let index = LineIndex::new("ab\n\u{e9}x\n");
        assert_eq!(index.position(TextSize::from(0)), (1, 1));
        assert_eq!(index.position(TextSize::from(5)), (2, 2));
        assert_eq!(index.end_line(TextSize::from(3)), 1);
    }
}
