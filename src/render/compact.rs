//! The compact output: one line for each finding, for grep and agents.
//!
//! A line is `path:line:col: rule: message`. A finding with no column has
//! `path:line: rule: message`, and a finding with no line has
//! `path: rule: message`. A run with no finding writes no line.

use crate::finding::{Finding, Location};

/// The lines of the findings, in report order.
pub fn lines(findings: &[Finding]) -> String {
    findings.iter().map(line).collect::<Vec<_>>().join("\n")
}

fn line(finding: &Finding) -> String {
    format!(
        "{}: {}: {}",
        place(&finding.location),
        one_line(&finding.rule),
        one_line(&finding.message)
    )
}

/// `path:line:col`, `path:line` or `path`.
fn place(location: &Location) -> String {
    let path = one_line(&location.path);
    match (location.line, location.column) {
        (Some(line), Some(column)) => format!("{path}:{line}:{column}"),
        (Some(line), None) => format!("{path}:{line}"),
        _ => path,
    }
}

/// The text with each line break, tab and other control character as a
/// space, so one finding is one line.
pub fn one_line(text: &str) -> String {
    text.split(|c: char| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::{Category, Draft, Impact, Severity};

    fn finding(location: Location, message: &str) -> Finding {
        Finding::new(Draft {
            check: "deps",
            rule: "unused-dependency",
            severity: Severity::Error,
            message: message.to_owned(),
            location,
            identity: &["x"],
            category: Category::BugRisk,
            impact: Impact::Major,
        })
    }

    #[test]
    fn a_line_has_the_place_the_rule_and_the_message() {
        let full = finding(Location::at("a b/c.rs", 3, 7), "Remove it.");
        let no_column = finding(
            Location {
                path: "Cargo.toml".to_owned(),
                line: Some(9),
                column: None,
                end_line: None,
            },
            "two\nlines",
        );
        let no_line = finding(
            Location {
                path: "src/orphan.rs".to_owned(),
                line: None,
                column: None,
                end_line: None,
            },
            "m",
        );
        assert_eq!(
            lines(&[full, no_column, no_line]),
            "a b/c.rs:3:7: unused-dependency: Remove it.\n\
             Cargo.toml:9: unused-dependency: two lines\n\
             src/orphan.rs: unused-dependency: m"
        );
        assert_eq!(lines(&[]), "");
        assert_eq!(one_line("a\tb\u{2028}c\u{85}d\x0be"), "a b c d e");
        // A path with a line break cannot start a second line.
        let forged = finding(Location::at("a.rs\nb.rs:1:1: fake", 2, 3), "m");
        assert_eq!(
            lines(&[forged]),
            "a.rs b.rs:1:1: fake:2:3: unused-dependency: m"
        );
    }
}
