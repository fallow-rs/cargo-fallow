//! `cargo fallow`: codebase analysis for Rust workspaces.
//!
//! The `cargo-fallow` binary is a thin wrapper around [`run`]. The library
//! holds every module, so the `missing_docs` lint checks the public API.
//! The public API is [`run`] only. The modules are internal.

// Only the workspace loader and the mutation runner spawn a process. See
// `.clippy.toml`.
#![cfg_attr(not(test), deny(clippy::disallowed_methods))]

mod cli;
mod error;
mod gates;
mod mutation;
mod parse;
mod render;
mod report;
mod workspace;

use std::ffi::OsString;
use std::io;
use std::process::ExitCode;

use fallow_report_protocol::exit_code::{EXIT_GATE_FAILED, EXIT_INVALID_INPUT, EXIT_OK};

use crate::cli::{Options, Parsed};
use crate::error::CliError;

/// Runs the CLI with the process arguments and returns the exit code.
///
/// `args` starts with the binary name. Cargo runs the binary as
/// `cargo-fallow fallow <args>`, and a direct call runs it as
/// `cargo-fallow <args>`. Both forms give the same result.
pub fn run(args: Vec<OsString>) -> ExitCode {
    let args = cli::strip_cargo_subcommand(args);
    let code = match cli::parse(&args) {
        Parsed::Run(options) => run_check(&options),
        Parsed::Spike(options) => run_spike(&options),
        Parsed::Exit(code) => code,
    };
    ExitCode::from(code)
}

/// Runs `check` and returns the process exit code.
fn run_check(options: &Options) -> u8 {
    match check(options) {
        Ok(code) => code,
        Err(err) => {
            err.emit(options.format);
            EXIT_INVALID_INPUT
        }
    }
}

/// Runs the hidden `schema-spike` command and returns the process exit code.
/// The report goes to stdout as JSON. A measured failure of the crate is part of
/// the report, not an error.
fn run_spike(options: &mutation::spike::Options) -> u8 {
    let result = mutation::spike::run(options).and_then(|report| {
        let text = serde_json::to_string_pretty(&report)
            .map_err(|err| CliError::output(&io::Error::other(err)))?;
        render::write_stdout(&text).map_err(|err| CliError::output(&err))
    });
    match result {
        Ok(()) => EXIT_OK,
        Err(err) => {
            err.emit(cli::Format::Json);
            EXIT_INVALID_INPUT
        }
    }
}

fn check(options: &Options) -> Result<u8, CliError> {
    let progress = render::Progress::new(options.quiet);
    progress.step("Reading cargo metadata");
    let workspace = workspace::load(options.manifest_path.as_deref())?;
    progress.step("Parsing target root files");
    let gates::Evaluation {
        outcomes,
        unreadable,
    } = gates::evaluate(&workspace, options);
    for file in &unreadable {
        render::warn(&format!(
            "cannot read `{}`, so the parse-error gate does not judge it: {}",
            file.path, file.error
        ));
    }
    let fails = outcomes.fails_run();
    let report = report::CheckReport::new(workspace, outcomes);
    render::write_report(&report, options.format).map_err(|err| CliError::output(&err))?;
    Ok(if fails { EXIT_GATE_FAILED } else { EXIT_OK })
}
