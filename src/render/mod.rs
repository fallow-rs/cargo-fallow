//! The output of a run.

mod human;
mod json;
pub mod mutants;
mod sarif;

use std::io::{self, Write};

use crate::cli::Format;
use crate::report::CheckReport;

/// Writes the report to stdout in the given format.
pub fn write_report(report: &CheckReport, format: Format) -> io::Result<()> {
    let text = match format {
        Format::Human => human::render(report),
        Format::Json => json::render(report)?,
        Format::Sarif => sarif::render(report)?,
    };
    write_stdout(&text)
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
