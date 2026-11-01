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
        Parsed::Mutants(options) => run_mutants(&options),
        Parsed::Watchdog(roots) => {
            mutation::guard::run_watchdog(&roots);
            EXIT_OK
        }
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

/// Runs the `mutants` command and returns the process exit code.
fn run_mutants(options: &mutation::run::Options) -> u8 {
    let result = match &options.inspect {
        Some(id) => inspect_report(options, id),
        None => mutants_report(options),
    };
    result.unwrap_or_else(|err| {
        err.emit(options.format);
        EXIT_INVALID_INPUT
    })
}

fn mutants_report(options: &mutation::run::Options) -> Result<u8, CliError> {
    let report = mutation::run::run(options)?;
    if report.gate_has_no_verdict() {
        return Err(CliError::new(
            error::ErrorCode::MutationFailed,
            "free memory stayed under the floor, so the run stopped before the gate could judge \
             each mutant",
        )
        .with_help("Run the command again with more free memory. The journal keeps the results."));
    }
    let text = match options.format {
        cli::Format::Human => render::mutants::human(&report),
        cli::Format::Json => to_json(&report)?,
        cli::Format::Sarif => {
            render::mutants::sarif(&report).map_err(|err| CliError::output(&err))?
        }
    };
    render::write_stdout(text.trim_end()).map_err(|err| CliError::output(&err))?;
    Ok(if report.fails_run() {
        EXIT_GATE_FAILED
    } else {
        EXIT_OK
    })
}

fn inspect_report(options: &mutation::run::Options, id: &str) -> Result<u8, CliError> {
    if options.format == cli::Format::Sarif {
        return Err(CliError::new(
            error::ErrorCode::InvalidArgument,
            "`--inspect` has no SARIF output",
        )
        .with_help("Use `--format human` or `--format json`."));
    }
    let report = mutation::run::inspect(options, id)?;
    let text = match options.format {
        cli::Format::Json => to_json(&report)?,
        _ => render::mutants::inspect_human(&report),
    };
    render::write_stdout(text.trim_end()).map_err(|err| CliError::output(&err))?;
    Ok(EXIT_OK)
}

fn to_json(value: &impl serde::Serialize) -> Result<String, CliError> {
    serde_json::to_string_pretty(value).map_err(|err| CliError::output(&io::Error::other(err)))
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
