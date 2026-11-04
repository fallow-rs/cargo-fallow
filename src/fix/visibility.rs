//! The edits of `fix` for the `visibility` findings.
//!
//! Each edit replaces the visibility keyword of one item at the range of its
//! `change-visibility` action: `pub` becomes `pub(crate)`, or a restricted
//! keyword goes. The range has one-based lines and columns in characters, so
//! the edit counts characters, not bytes. An edit applies only when the text
//! at the range is still the written keyword.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::visibility::report::{CHANGE_ACTION, FindingRow, Range, VisibilityReport};

/// One planned edit. The field order is the wire order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Edit {
    /// Always `change-visibility`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// The finding kind, for example `unnecessary_public`.
    pub rule: &'static str,
    /// The package of the item.
    pub package: String,
    /// The item path.
    pub item: String,
    /// The file, relative to the workspace root.
    pub path: String,
    /// The one-based line of the keyword.
    pub line: u32,
    /// The one-based column of the keyword, in characters.
    pub column: u32,
    /// The written keyword.
    pub from: String,
    /// The new keyword. An empty text removes the keyword.
    pub to: String,
    /// True when the command wrote the edit.
    pub applied: bool,
    /// The range of the keyword. It is not part of the report.
    #[serde(skip)]
    pub range: Range,
}

/// The edits for the findings with a `change-visibility` action that `fix`
/// applies. A file that more than one crate holds gives one edit.
pub fn plan(report: &VisibilityReport) -> Vec<Edit> {
    let mut edits: Vec<Edit> = report.findings.iter().filter_map(edit).collect();
    edits.sort_by(|a, b| (&a.path, a.line, a.column).cmp(&(&b.path, b.line, b.column)));
    edits.dedup_by(|a, b| (&a.path, a.line, a.column) == (&b.path, b.line, b.column));
    edits
}

fn edit(row: &FindingRow) -> Option<Edit> {
    let action = row
        .actions
        .iter()
        .find(|action| action.kind == CHANGE_ACTION && action.auto_fixable)?;
    let range = action.range?;
    Some(Edit {
        kind: CHANGE_ACTION,
        rule: row.kind,
        package: row.package.clone(),
        item: row.item.clone(),
        path: row.file.clone(),
        line: range.line,
        column: range.column,
        from: action.from.clone(),
        to: action.to.clone(),
        applied: false,
        range,
    })
}

/// The edits of each file, in file order.
pub fn by_file(edits: &[Edit]) -> BTreeMap<&str, Vec<&Edit>> {
    let mut grouped: BTreeMap<&str, Vec<&Edit>> = BTreeMap::new();
    for edit in edits {
        grouped.entry(edit.path.as_str()).or_default().push(edit);
    }
    grouped
}

/// Applies the edits of one file to its text and returns the new text.
pub fn apply(text: &str, edits: &[&Edit]) -> Result<String, String> {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let mut spans: Vec<(usize, usize, &Edit)> = edits
        .iter()
        .map(|edit| {
            let start = offset(text, &starts, edit.range.line, edit.range.column);
            let end = offset(text, &starts, edit.range.end_line, edit.range.end_column);
            match (start, end) {
                (Some(start), Some(end)) if text.get(start..end) == Some(edit.from.as_str()) => {
                    Ok((start, end, *edit))
                }
                _ => Err(format!(
                    "the text at line {} column {} is not `{}`",
                    edit.line, edit.column, edit.from
                )),
            }
        })
        .collect::<Result<_, _>>()?;
    // The last edit goes first, so each earlier offset stays valid.
    spans.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    let mut out = text.to_owned();
    for (start, end, edit) in spans {
        // A removed keyword takes the space after it too.
        let end = if edit.to.is_empty() {
            end + text[end..]
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .map(char::len_utf8)
                .sum::<usize>()
        } else {
            end
        };
        out.replace_range(start..end, &edit.to);
    }
    Ok(out)
}

/// The byte offset of a one-based line and column in characters.
fn offset(text: &str, starts: &[usize], line: u32, column: u32) -> Option<usize> {
    let start = *starts.get(usize::try_from(line).ok()?.checked_sub(1)?)?;
    let skip = usize::try_from(column).ok()?.checked_sub(1)?;
    let rest = text.get(start..)?;
    if skip == 0 {
        return Some(start);
    }
    rest.char_indices()
        .take_while(|(_, c)| *c != '\n')
        .map(|(at, c)| at + c.len_utf8())
        .nth(skip - 1)
        .map(|at| start + at)
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "a test fails loudly when an edit does not apply"
)]
mod tests {
    use super::*;

    fn edit(line: u32, column: u32, from: &str, to: &str) -> Edit {
        let width = u32::try_from(from.chars().count()).unwrap_or(0);
        Edit {
            kind: CHANGE_ACTION,
            rule: "unnecessary_public",
            package: "app".to_owned(),
            item: "app::f".to_owned(),
            path: "src/lib.rs".to_owned(),
            line,
            column,
            from: from.to_owned(),
            to: to.to_owned(),
            applied: false,
            range: Range {
                line,
                column,
                end_line: line,
                end_column: column + width,
            },
        }
    }

    #[test]
    fn edit_counts_characters_after_a_raw_string_with_wide_text() {
        let text = "const S: &str = r#\"é\"#; pub fn f() {}\n";
        let column = u32::try_from(text[..text.find("pub").expect("a keyword")].chars().count())
            .expect("a small column")
            + 1;
        let out = apply(text, &[&edit(1, column, "pub", "pub(crate)")]).expect("the edit applies");
        assert_eq!(out, "const S: &str = r#\"é\"#; pub(crate) fn f() {}\n");
    }

    #[test]
    fn removal_takes_the_space_and_two_edits_on_one_line_apply() {
        let text = "pub(crate) struct A; pub(super) struct B;\n";
        let out = apply(
            text,
            &[
                &edit(1, 1, "pub(crate)", ""),
                &edit(1, 22, "pub(super)", ""),
            ],
        )
        .expect("the edits apply");
        assert_eq!(out, "struct A; struct B;\n");
    }

    #[test]
    fn changed_text_at_the_range_is_an_error() {
        let text = "fn f() {}\n";
        assert!(apply(text, &[&edit(1, 1, "pub", "pub(crate)")]).is_err());
    }
}
