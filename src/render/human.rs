//! The summary for a person.

use std::fmt::Write;

use fallow_report_protocol::{GateFileReason, GateOutcome, GateStatus};

use crate::report::CheckReport;

/// The report as a short summary.
pub fn render(report: &CheckReport) -> String {
    let members = report.workspace.members.len();
    let targets = report.target_count();
    let mut text = format!(
        "cargo fallow check: {members} {}, {targets} {}\n",
        plural(members, "member", "members"),
        plural(targets, "target", "targets"),
    );
    if report.total_issues == 0 {
        text.push_str("No issues found\n");
    }
    text.push_str("Gates:");
    for (name, outcome) in report.gate_outcomes.iter().flatten() {
        // Writing to a String does not fail.
        let _ = write!(text, "\n  {}: {}", name.as_str(), gate_line(outcome));
        for file in &outcome.files {
            let _ = write!(text, "\n    {}", file.path);
            if file.reason == Some(GateFileReason::NestingTooDeep) {
                text.push_str(" (nests too deep to parse)");
            }
        }
    }
    text
}

fn gate_line(outcome: &GateOutcome) -> String {
    let status = match outcome.status {
        GateStatus::Pass => "pass",
        GateStatus::Warn => "warn",
        GateStatus::Fail => "fail",
        GateStatus::Skipped => "skipped",
    };
    let mut line = status.to_owned();
    if !outcome.files.is_empty() {
        let count = outcome.files.len();
        let _ = write!(line, " ({count} {})", plural(count, "file", "files"));
    }
    if !outcome.enforced {
        line.push_str(", not enforced");
    }
    line
}

const fn plural<'a>(count: usize, one: &'a str, many: &'a str) -> &'a str {
    if count == 1 { one } else { many }
}

#[cfg(test)]
mod tests {
    use fallow_report_protocol::GateFile;

    use super::*;
    use crate::gates::{GateOutcomes, RustGateName};
    use crate::report::CheckReport;
    use crate::workspace::Workspace;

    #[test]
    fn too_deep_file_names_its_reason() {
        let files = ["a.rs", "b.rs"].map(|path| GateFile {
            path: path.to_owned(),
            error_count: 1,
            panicked: true,
            reason: (path == "b.rs").then_some(GateFileReason::NestingTooDeep),
        });
        let mut outcomes = GateOutcomes::default();
        outcomes.insert(
            RustGateName::ParseError,
            GateOutcome::with_files(GateStatus::Fail, true, files.to_vec()),
        );
        let workspace = Workspace {
            root: "/w".into(),
            target_dir: "/w/target".into(),
            members: Vec::new(),
        };
        let text = render(&CheckReport::new(workspace, outcomes));
        assert!(
            text.ends_with("\n    a.rs\n    b.rs (nests too deep to parse)"),
            "{text}"
        );
    }
}
