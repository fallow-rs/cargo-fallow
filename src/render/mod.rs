//! The output of a run.

pub mod deps;
mod human;
pub mod mutants;
mod sarif;

use std::collections::BTreeMap;
use std::io::{self, Write};

use fallow_report_protocol::{GateFile, GateOutcome};

use crate::gates::RustGateName;

/// The human gate block of the parse gate, with the files that do not parse.
pub fn parse_human(gates: &BTreeMap<RustGateName, GateOutcome>) -> String {
    human::gates_block(gates)
}

/// The SARIF log of the parse gate.
pub fn parse_sarif(gates: &BTreeMap<RustGateName, GateOutcome>) -> serde_json::Value {
    sarif::gate_log(gates)
}

/// The message of a file that does not parse.
pub const fn parse_message(file: &GateFile) -> &'static str {
    sarif::file_message(RustGateName::ParseError, file)
}

/// The empty SARIF log of a run with no result.
pub fn empty_sarif() -> serde_json::Value {
    sarif::document(&[], &[], &[])
}

/// The gate outcome as one line for a person, for the reports of other commands.
pub fn gate_text(outcome: &fallow_report_protocol::GateOutcome) -> String {
    human::gate_line(outcome)
}

/// A SARIF log with one run, for the reports of other commands. Each gate that
/// did not pass becomes a tool execution notification.
pub fn sarif_document(
    rules: &[serde_json::Value],
    outcomes: &std::collections::BTreeMap<
        crate::gates::RustGateName,
        fallow_report_protocol::GateOutcome,
    >,
    results: &[serde_json::Value],
) -> serde_json::Value {
    let flagged: Vec<_> = outcomes
        .iter()
        .filter_map(|(name, outcome)| sarif::level(outcome).map(|level| (*name, outcome, level)))
        .collect();
    sarif::document(rules, &flagged, results)
}

/// Writes one block of text and a newline to stdout. A closed pipe counts as
/// success, so `cargo fallow | head` does not fail.
pub fn write_stdout(text: &str) -> io::Result<()> {
    ignore_broken_pipe(writeln!(io::stdout().lock(), "{text}"))
}

/// Writes one block of text and a newline to stderr.
pub fn write_stderr(text: &str) -> io::Result<()> {
    ignore_broken_pipe(writeln!(io::stderr().lock(), "{text}"))
}

/// Writes `warning: <message>` to stderr. `--quiet` does not hide it.
pub fn warn(message: &str) {
    // A warning is optional. A failed write must not stop the run.
    let _ = write_stderr(&format!("warning: {message}"));
}

fn ignore_broken_pipe(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        other => other,
    }
}

/// Progress lines on stderr. `--quiet` hides them.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    quiet: bool,
}

impl Progress {
    /// A progress writer that writes nothing when `quiet` is true.
    pub const fn new(quiet: bool) -> Self {
        Self { quiet }
    }

    /// Writes one progress line.
    pub fn step(self, message: &str) {
        if self.quiet {
            return;
        }
        // Progress is optional. A failed write must not stop the run.
        let _ = write_stderr(message);
    }
}
