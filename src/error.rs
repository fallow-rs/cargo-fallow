//! The errors that stop a run with exit code 2.

use std::io;

use fallow_report_protocol::ErrorOutput;
use fallow_report_protocol::exit_code::EXIT_INVALID_INPUT;

use crate::cli::Format;
use crate::render;

/// The stable machine-readable code of a [`CliError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// A flag or a flag value is not valid.
    InvalidArgument,
    /// The `--manifest-path` file does not exist.
    ManifestNotFound,
    /// `cargo metadata` failed.
    MetadataFailed,
    /// The report could not be written.
    OutputFailed,
    /// The `mutants` command cannot run: the package does not build, the tests
    /// fail with no mutant, or a file operation or a process failed.
    MutationFailed,
    /// A table under `[workspace.metadata.fallow]` is not valid.
    InvalidConfig,
    /// A check did not finish before its time limit.
    TimedOut,
    /// `visibility --precise` cannot decide: a part of the load is missing, or
    /// the worker stopped early and the run is a gate.
    PreciseFailed,
    /// `mutants --inspect --no-build` found no coverage map for this source in
    /// the cache.
    NoCachedMap,
}

impl ErrorCode {
    /// The wire spelling of the code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "CARGO_FALLOW_INVALID_ARGUMENT",
            Self::ManifestNotFound => "CARGO_FALLOW_MANIFEST_NOT_FOUND",
            Self::MetadataFailed => "CARGO_FALLOW_METADATA_FAILED",
            Self::OutputFailed => "CARGO_FALLOW_OUTPUT_FAILED",
            Self::MutationFailed => "CARGO_FALLOW_MUTATION_FAILED",
            Self::InvalidConfig => "CARGO_FALLOW_INVALID_CONFIG",
            Self::TimedOut => "CARGO_FALLOW_TIMED_OUT",
            Self::PreciseFailed => "CARGO_FALLOW_PRECISE_FAILED",
            Self::NoCachedMap => "CARGO_FALLOW_NO_CACHED_MAP",
        }
    }
}

/// An error that stops the run. The exit code is always 2.
#[derive(Debug, Clone)]
pub struct CliError {
    /// The message for the user.
    pub message: String,
    /// The stable code.
    pub code: ErrorCode,
    /// A remediation hint, when there is one.
    pub help: Option<String>,
}

impl CliError {
    /// An error with a code and a message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code,
            help: None,
        }
    }

    /// Adds a remediation hint.
    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// The error for a failed write of the report.
    pub fn output(err: &io::Error) -> Self {
        Self::new(
            ErrorCode::OutputFailed,
            format!("cannot write the report: {err}"),
        )
    }

    /// Writes the error. JSON goes to stdout as an error document. The other
    /// formats write `error: <message>` to stderr.
    pub fn emit(&self, format: Format) {
        // A failed write of the error changes nothing: the exit code stays 2.
        let _ = match format {
            Format::Json => self.emit_json(),
            Format::Human | Format::Sarif | Format::Codeclimate => self.emit_text(),
        };
    }

    fn emit_json(&self) -> io::Result<()> {
        let document = ErrorOutput::new(self.message.clone(), EXIT_INVALID_INPUT)
            .with_code(Some(self.code.as_str().to_owned()))
            .with_help(self.help.clone());
        let text = serde_json::to_string_pretty(&document).map_err(io::Error::other)?;
        render::write_stdout(&text)
    }

    fn emit_text(&self) -> io::Result<()> {
        let mut text = format!("error: {}", self.message);
        if let Some(help) = &self.help {
            text.push_str("\nhelp: ");
            text.push_str(help);
        }
        render::write_stderr(&text)
    }
}
