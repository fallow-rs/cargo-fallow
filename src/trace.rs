//! The shared parts of `--trace` on `visibility`, `deps` and `dupes`.
//!
//! A trace shows why the check keeps or reports one item: each place in code
//! that uses it, and each fact that keeps it. A trace is not a report. It has
//! no gate, so the run exits 0 when the item exists.

use fallow_report_protocol::exit_code::EXIT_OK;

use crate::cli::Format;
use crate::error::{CliError, ErrorCode};
use crate::render;

/// The version of the shape of each trace document. A change that removes a
/// member or changes its type increments it.
pub const SCHEMA_VERSION: u32 = 1;

/// Stops a trace in a format other than human text and JSON. `command` is
/// the name of the command, for the message.
pub fn check_format(format: Format, command: &str) -> Result<(), CliError> {
    if matches!(format, Format::Human | Format::Json) {
        return Ok(());
    }
    Err(CliError::new(
        ErrorCode::InvalidArgument,
        format!("`{command} --trace` has only human and JSON output"),
    )
    .with_help("Use `--format human` or `--format json`."))
}

/// Writes a trace document in `format`: `human` for a person, or the JSON of
/// `document`. Returns the exit code.
pub fn write(
    format: Format,
    human: &str,
    document: &impl serde::Serialize,
) -> Result<u8, CliError> {
    let text = match format {
        Format::Json => crate::section::pretty(document)?,
        _ => human.to_owned(),
    };
    render::write_stdout(text.trim_end()).map_err(|err| CliError::output(&err))?;
    Ok(EXIT_OK)
}

/// The text of a one-based line, or `?` when the line is not known.
pub fn line_text(line: Option<u32>) -> String {
    line.map_or_else(|| "?".to_owned(), |line| line.to_string())
}
