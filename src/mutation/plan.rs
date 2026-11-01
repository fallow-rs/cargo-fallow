//! Builds the list of mutants of one package.

use std::ops::Range;

use crate::mutation::coverage::LineRange;
use crate::mutation::files::CrateFile;
use crate::mutation::id::id_text;
use crate::mutation::report::MutantRow;
use crate::mutation::rewrites::{FileRewrites, Rewrite};
use crate::mutation::schema::mutated_text;
use crate::mutation::status::{Status, Tier};

/// The longest text of the `original` field, in bytes.
const ORIGINAL_LIMIT: usize = 120;

/// One mutant with its row of the report.
#[derive(Debug, Clone)]
pub struct Planned {
    /// The stable id.
    pub id: u64,
    /// The index of the file.
    pub file: usize,
    /// The range to replace in a rebuild.
    pub range: Range<usize>,
    /// The new text of the range in a rebuild.
    pub text: String,
    /// True when the mutant is in a constant context. A coverage map says nothing
    /// about such code, because the compiler can evaluate it.
    pub constant: bool,
    /// The row of the report.
    pub row: MutantRow,
}

/// The mutants of the schema tier, by file and offset.
pub fn schema_mutants(files: &[CrateFile]) -> Vec<Planned> {
    let mut planned = Vec::new();
    for (index, file) in files.iter().enumerate() {
        for site in &file.found.sites {
            for mutant in &site.mutants {
                planned.push(Planned {
                    id: mutant.id,
                    file: index,
                    range: site.range.clone(),
                    text: mutated_text(&file.original, site, mutant),
                    constant: false,
                    row: row(
                        file,
                        &RowInput {
                            tier: Tier::Schema,
                            id: mutant.id,
                            operator: mutant.operator,
                            range: &site.range,
                            replacement: mutant.replacement,
                            under_cfg: site.under_cfg,
                        },
                    ),
                });
            }
        }
    }
    planned
}

/// The mutants of the rebuild tier, by file and offset.
pub fn rebuild_mutants(files: &[CrateFile], rewrites: &[FileRewrites]) -> Vec<Planned> {
    let mut planned = Vec::new();
    for (index, file) in files.iter().enumerate() {
        for rewrite in &rewrites[index].rewrites {
            planned.push(rebuild_mutant(file, index, rewrite));
        }
    }
    planned
}

fn rebuild_mutant(file: &CrateFile, index: usize, rewrite: &Rewrite) -> Planned {
    Planned {
        id: rewrite.id,
        file: index,
        range: rewrite.range.clone(),
        text: rewrite.replacement.clone(),
        constant: rewrite.constant,
        row: row(
            file,
            &RowInput {
                tier: Tier::Rebuild,
                id: rewrite.id,
                operator: rewrite.operator,
                range: &rewrite.range,
                replacement: &rewrite.replacement,
                under_cfg: rewrite.under_cfg,
            },
        ),
    }
}

struct RowInput<'a> {
    tier: Tier,
    id: u64,
    operator: &'static str,
    range: &'a Range<usize>,
    replacement: &'a str,
    under_cfg: bool,
}

fn row(file: &CrateFile, input: &RowInput<'_>) -> MutantRow {
    let (line, column) = line_and_column(&file.original, input.range.start);
    MutantRow {
        id: id_text(input.id),
        tier: input.tier,
        operator: input.operator,
        file: file.path.clone(),
        line,
        column,
        original: short_text(&file.original[input.range.clone()]),
        replacement: input.replacement.to_owned(),
        status: Status::Pending,
        reason: None,
        killed_by: None,
        retried: false,
        removed_from_schema: false,
        under_cfg: file.under_cfg || input.under_cfg,
        duration_ms: 0,
        tests_run: 0,
        doctests_run: 0,
        no_line_data: false,
    }
}

/// The line and the byte column of an offset of the original text. The schema
/// build can move code, so a report never reads a position from a build.
fn line_and_column(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let column = offset - before.rfind('\n').map_or(0, |at| at + 1) + 1;
    (line, column)
}

/// The first and the last line of a range of the original text.
pub fn lines_of(text: &str, range: &Range<usize>) -> LineRange {
    let (first, _) = line_and_column(text, range.start);
    let (last, _) = line_and_column(text, range.end.saturating_sub(1).max(range.start));
    let line = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
    (line(first), line(last))
}

/// One line of at most [`ORIGINAL_LIMIT`] bytes.
fn short_text(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim_end();
    let mut end = line.len().min(ORIGINAL_LIMIT);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_and_column_start_at_one() {
        assert_eq!(line_and_column("ab\ncd", 0), (1, 1));
        assert_eq!(line_and_column("ab\ncd", 1), (1, 2));
        assert_eq!(line_and_column("ab\ncd", 3), (2, 1));
        assert_eq!(line_and_column("ab\ncd", 4), (2, 2));
    }

    #[test]
    fn a_range_gives_its_first_and_last_line() {
        let text = "a\nb c\nd\n";
        assert_eq!(lines_of(text, &(0..1)), (1, 1));
        assert_eq!(lines_of(text, &(2..5)), (2, 2));
        assert_eq!(lines_of(text, &(2..7)), (2, 3));
        // The newline at the end of a range is still on the line of the range.
        assert_eq!(lines_of(text, &(2..6)), (2, 2));
        assert_eq!(lines_of(text, &(2..2)), (2, 2));
    }

    #[test]
    fn short_text_keeps_one_line_and_a_char_boundary() {
        assert_eq!(short_text("a == b\n  && c"), "a == b");
        assert_eq!(short_text(""), "");
        let long = "é".repeat(100);
        let short = short_text(&long);
        assert!(short.len() <= ORIGINAL_LIMIT);
        assert!(short.chars().all(|c| c == 'é'));
    }
}
