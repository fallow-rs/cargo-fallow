//! The markdown output, for a pull request body or a job summary.
//!
//! The document has a title with the issue count, a gate table and one table
//! for each check. A table shows at most [`ROW_CAP`] findings, and a line
//! after it gives the count of the rest.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use fallow_report_protocol::{GateOutcome, GateStatus};

use super::compact::one_line;
use crate::finding::{Finding, Location};
use crate::gates::RustGateName;

/// The largest number of rows in the table of one check.
pub const ROW_CAP: usize = 50;

/// The largest document, in bytes. A GitHub comment holds at most 65536
/// characters, so the tables stop under that size.
pub const SIZE_CAP: usize = 60_000;

/// The findings of one check.
pub struct Group<'a> {
    /// The check name.
    pub check: &'a str,
    /// The findings, in report order.
    pub findings: &'a [Finding],
}

/// The parts of one markdown document.
pub struct Document<'a> {
    /// The command, for example `check` or `deps`.
    pub command: &'a str,
    /// More lines under the title, for example the health score.
    pub notes: Vec<String>,
    /// The gate outcomes of the run.
    pub gates: &'a BTreeMap<RustGateName, GateOutcome>,
    /// The findings of each check, in report order.
    pub groups: Vec<Group<'a>>,
}

/// The markdown text of a document.
pub fn render(document: &Document<'_>) -> String {
    let total: usize = document
        .groups
        .iter()
        .map(|group| group.findings.len())
        .sum();
    let mut text = if total == 0 {
        format!("## cargo fallow {}: no issues found\n", document.command)
    } else {
        format!(
            "## cargo fallow {}: {total} {}\n",
            document.command,
            if total == 1 { "issue" } else { "issues" }
        )
    };
    for note in &document.notes {
        let _ = write!(text, "\n{}\n", escape(note));
    }
    if !document.gates.is_empty() {
        text.push_str("\n| Gate | Status | Enforced |\n| --- | --- | --- |\n");
        for (name, outcome) in document.gates {
            let _ = writeln!(
                text,
                "| `{}` | {} | {} |",
                name.as_str(),
                status(outcome.status),
                if outcome.enforced { "yes" } else { "no" }
            );
        }
    }
    for group in &document.groups {
        table(&mut text, group);
    }
    text
}

fn table(text: &mut String, group: &Group<'_>) {
    let count = group.findings.len();
    let _ = write!(text, "\n### {} ({count})\n\n", group.check);
    if count == 0 {
        text.push_str("No issues.\n");
        return;
    }
    text.push_str("| Location | Rule | Message |\n| --- | --- | --- |\n");
    let mut shown = 0;
    for finding in group.findings.iter().take(ROW_CAP) {
        let row = format!(
            "| {} | {} | {} |\n",
            code(&place(&finding.location)),
            code(&finding.rule),
            escape(&finding.message)
        );
        if text.len() + row.len() > SIZE_CAP {
            break;
        }
        text.push_str(&row);
        shown += 1;
    }
    if count > shown {
        let rest = count - shown;
        let _ = write!(
            text,
            "\n{rest} more {}. Use `--format json` for the full list.\n",
            if rest == 1 { "finding" } else { "findings" }
        );
    }
}

const fn status(status: GateStatus) -> &'static str {
    match status {
        GateStatus::Pass => "pass",
        GateStatus::Warn => "warn",
        GateStatus::Fail => "**fail**",
        GateStatus::Skipped => "skipped",
    }
}

/// `path:line`, or `path` without a line.
fn place(location: &Location) -> String {
    let path = one_line(&location.path);
    match location.line {
        Some(line) => format!("{path}:{line}"),
        None => path,
    }
}

/// The text as inline code. The fence is one backtick longer than the
/// longest backtick run of the text, so the text cannot close the span.
fn code(text: &str) -> String {
    let text = cell_code(&one_line(text));
    let fence = "`".repeat(longest_run(&text) + 1);
    if text.starts_with('`') || text.ends_with('`') {
        format!("{fence} {text} {fence}")
    } else {
        format!("{fence}{text}{fence}")
    }
}

/// Code text that stays in its table cell. The table scanner reads `\\`
/// and then `\\|` as escapes, so each backslash doubles and each `|` gets a
/// backslash. A code span shows a backslash as written.
fn cell_code(text: &str) -> String {
    text.replace('\\', "\\\\").replace('|', "\\|")
}

/// The length of the longest backtick run of the text.
fn longest_run(text: &str) -> usize {
    text.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

/// The text on one line, so it stays in its cell. A backtick run that a run
/// of the same length closes is a code span, the CommonMark rule, and only its
/// `|` is escaped. Outside a code span, each character that markdown or HTML
/// reads is escaped, also a backtick run that no run closes.
fn escape(text: &str) -> String {
    let chars: Vec<char> = one_line(text).chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut index = 0;
    while let Some(&c) = chars.get(index) {
        if c != '`' {
            plain(&mut out, c);
            index += 1;
            continue;
        }
        let run = run_at(&chars, index);
        match closing_run(&chars, index + run, run) {
            Some(close) => {
                let inside: String = chars[index + run..close].iter().collect();
                let fence: String = chars[index..index + run].iter().collect();
                out.push_str(&fence);
                out.push_str(&cell_code(&inside));
                out.push_str(&fence);
                index = close + run;
            }
            None => {
                for _ in 0..run {
                    out.push_str("\\`");
                }
                index += run;
            }
        }
    }
    out
}

/// The length of the backtick run that starts at `start`.
fn run_at(chars: &[char], start: usize) -> usize {
    chars[start..].iter().take_while(|&&c| c == '`').count()
}

/// The start of the next backtick run of exactly `length`, from `from` on.
fn closing_run(chars: &[char], from: usize, length: usize) -> Option<usize> {
    let mut index = from;
    while index < chars.len() {
        if chars[index] == '`' {
            let run = run_at(chars, index);
            if run == length {
                return Some(index);
            }
            index += run;
        } else {
            index += 1;
        }
    }
    None
}

/// Adds one character outside a code span, escaped when markdown or HTML
/// reads it.
fn plain(out: &mut String, c: char) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '|' | '*' | '_' | '[' | ']' | '\\' | '~' | '#' => {
            out.push('\\');
            out.push(c);
        }
        _ => out.push(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::{Category, Draft, Impact, Severity};

    fn finding(index: usize) -> Finding {
        Finding::new(Draft {
            check: "complexity",
            rule: "complexity",
            severity: Severity::Warning,
            message: format!("`f{index}` is a | b\nlong"),
            location: Location::at("src/a b.rs", 3, 1),
            identity: &["x"],
            category: Category::Complexity,
            impact: Impact::Minor,
        })
    }

    #[test]
    fn a_table_escapes_its_cells_and_caps_its_rows() {
        let findings: Vec<Finding> = (0..ROW_CAP + 2).map(finding).collect();
        let gates = BTreeMap::from([(
            RustGateName::HealthFindings,
            GateOutcome::new(GateStatus::Fail, true),
        )]);
        let text = render(&Document {
            command: "complexity",
            notes: Vec::new(),
            gates: &gates,
            groups: vec![Group {
                check: "complexity",
                findings: &findings,
            }],
        });
        assert!(
            text.starts_with("## cargo fallow complexity: 52 issues\n"),
            "{text}"
        );
        assert!(
            text.contains("| `health-findings` | **fail** | yes |"),
            "{text}"
        );
        assert!(
            text.contains("| `src/a b.rs:3` | `complexity` | `f0` is a \\| b long |"),
            "{text}"
        );
        assert_eq!(text.matches("| `src/a b.rs:3` |").count(), ROW_CAP);
        assert!(text.ends_with("\n2 more findings. Use `--format json` for the full list.\n"));
    }

    #[test]
    fn a_cell_escapes_markdown_and_html_outside_code_spans() {
        assert_eq!(
            escape("`a<b>|c` uses <impl Foo> & *x* [y](z) a_b"),
            "`a<b>\\|c` uses &lt;impl Foo&gt; &amp; \\*x\\* \\[y\\](z) a\\_b"
        );
    }

    #[test]
    fn a_backtick_cannot_open_or_close_a_span_of_the_tool() {
        // An unclosed run is plain text, so the markup after it stays escaped.
        assert_eq!(escape("a`<img src=x>"), "a\\`&lt;img src=x&gt;");
        // A run closes only with a run of the same length.
        assert_eq!(escape("``x` <b>``"), "``x` <b>``");
        assert_eq!(escape("``x` <b>"), "\\`\\`x\\` &lt;b&gt;");
        assert_eq!(code("a`b"), "``a`b``");
        assert_eq!(code("`a"), "`` `a ``");
        assert_eq!(code("a\nb|c"), "`a b\\|c`");
    }

    /// True when the cell text has a `|` that the table scanner reads as the
    /// end of the cell: a `|` after an even number of backslashes.
    fn splits(cell: &str) -> bool {
        let mut backslashes = 0;
        for c in cell.chars() {
            match c {
                '\\' => backslashes += 1,
                '|' if backslashes % 2 == 0 => return true,
                _ => backslashes = 0,
            }
            if c != '\\' {
                backslashes = 0;
            }
        }
        false
    }

    #[test]
    fn a_backslash_before_a_pipe_cannot_end_the_cell() {
        for text in ["`a\\|<img>`", "a\\|<img>", "`a\\\\|b`", "x | y"] {
            assert!(!splits(&escape(text)), "{text}: {}", escape(text));
            assert!(!splits(&code(text)), "{text}: {}", code(text));
        }
    }

    #[test]
    fn the_tables_stop_under_the_size_of_a_comment() {
        let long: Vec<Finding> = (0..ROW_CAP)
            .map(|index| {
                let mut finding = finding(index);
                finding.message = "x".repeat(2_000);
                finding
            })
            .collect();
        let groups = std::iter::repeat_with(|| Group {
            check: "complexity",
            findings: &long,
        })
        .take(8)
        .collect();
        let text = render(&Document {
            command: "check",
            notes: Vec::new(),
            gates: &BTreeMap::new(),
            groups,
        });
        assert!(text.len() < 65_536, "{}", text.len());
        assert!(text.contains("more findings. Use `--format json`"));
    }

    #[test]
    fn an_empty_run_says_so() {
        let text = render(&Document {
            command: "deps",
            notes: Vec::new(),
            gates: &BTreeMap::new(),
            groups: vec![Group {
                check: "deps",
                findings: &[],
            }],
        });
        assert_eq!(
            text,
            "## cargo fallow deps: no issues found\n\n### deps (0)\n\nNo issues.\n"
        );
    }
}
