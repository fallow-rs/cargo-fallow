//! The summary for a person.

use std::fmt::Write;

use std::collections::BTreeMap;

use fallow_report_protocol::{GateFileReason, GateOutcome, GateStatus};

use crate::gates::RustGateName;

/// The gate block of a `check` run, with the files of each gate.
pub fn gates_block(gates: &BTreeMap<RustGateName, GateOutcome>) -> String {
    let mut text = String::from("Gates:");
    for (name, outcome) in gates {
        // Writing to a String does not fail.
        let _ = write!(text, "\n  {}: {}", name.as_str(), gate_line(outcome));
        // The findings already name the files of the other gates. A file that
        // does not parse has no finding, so only this gate lists its files.
        if *name != RustGateName::ParseError {
            continue;
        }
        for file in &outcome.files {
            let _ = write!(text, "\n    {}", file.path);
            if file.reason == Some(GateFileReason::NestingTooDeep) {
                text.push_str(" (nests too deep to parse)");
            }
        }
    }
    text
}

pub(super) fn gate_line(outcome: &GateOutcome) -> String {
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

pub(super) const fn plural<'a>(count: usize, one: &'a str, many: &'a str) -> &'a str {
    if count == 1 { one } else { many }
}

#[cfg(test)]
mod tests {
    use fallow_report_protocol::GateFile;

    use super::*;

    #[test]
    fn too_deep_file_names_its_reason() {
        let files = ["a.rs", "b.rs"].map(|path| GateFile {
            path: path.to_owned(),
            error_count: 1,
            panicked: true,
            reason: (path == "b.rs").then_some(GateFileReason::NestingTooDeep),
        });
        let gates = BTreeMap::from([(
            RustGateName::ParseError,
            GateOutcome::with_files(GateStatus::Fail, true, files.to_vec()),
        )]);
        let text = gates_block(&gates);
        assert!(
            text.ends_with("\n    a.rs\n    b.rs (nests too deep to parse)"),
            "{text}"
        );
    }
}
