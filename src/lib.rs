//! `cargo fallow`: codebase analysis for Rust workspaces.
//!
//! The `cargo-fallow` binary is a thin wrapper around [`run`]. The library
//! holds every module, so the `missing_docs` lint checks the public API.
//! The public API is [`run`] only. The modules are internal.

// Only the workspace loader, the mutation runner and the git reader of the
// structure checks spawn a process. See `.clippy.toml`.
#![cfg_attr(not(test), deny(clippy::disallowed_methods))]

mod attrs;
mod check;
mod cli;
mod deps;
mod error;
mod finding;
mod gates;
mod graph;
mod health;
mod modules;
mod mutation;
mod parse;
mod render;
mod report;
mod section;
mod structure;
mod visibility;
mod workspace;

use std::ffi::OsString;
use std::io;
use std::process::ExitCode;

use fallow_report_protocol::exit_code::{EXIT_INVALID_INPUT, EXIT_OK};

use crate::cli::Parsed;
use crate::error::CliError;

/// Runs the CLI with the process arguments and returns the exit code.
///
/// `args` starts with the binary name. Cargo runs the binary as
/// `cargo-fallow fallow <args>`, and a direct call runs it as
/// `cargo-fallow <args>`. Both forms give the same result.
pub fn run(args: Vec<OsString>) -> ExitCode {
    let args = cli::strip_cargo_subcommand(args);
    let code = match cli::parse(&args) {
        Parsed::Run(options) => check::run(&options),
        Parsed::Mutants(options) => run_mutants(&options),
        Parsed::Deps(options) => run_deps(&options),
        Parsed::Visibility(options) => run_visibility(&options),
        Parsed::PreciseWorker(args) => visibility::precise::run_worker_command(&args),
        Parsed::Structure(command) => structure::run(&command),
        Parsed::Watchdog {
            roots,
            sweep,
            remove,
        } => {
            mutation::guard::run_watchdog(&roots, sweep.as_deref(), remove);
            EXIT_OK
        }
        Parsed::Exit(code) => code,
    };
    ExitCode::from(code)
}

/// Runs the `visibility` command and returns the process exit code.
fn run_visibility(options: &cli::VisibilityOptions) -> u8 {
    visibility_report(options).unwrap_or_else(|err| {
        err.emit(options.format);
        EXIT_INVALID_INPUT
    })
}

fn visibility_report(options: &cli::VisibilityOptions) -> Result<u8, CliError> {
    let progress = render::Progress::new(options.quiet);
    progress.step("Reading cargo metadata");
    let workspace = workspace::load(options.manifest_path.as_deref())?;
    let options = &visibility::config::resolve(&workspace, options)?;
    progress.step("Building the module graph");
    let production = graph::Production {
        bins: options.production.clone(),
    };
    let module_graph = graph::build(&workspace, &production);
    progress.step("Checking the visibility");
    let Some(limits) = options.precise else {
        return visibility::fast_report(&module_graph, options, &workspace)
            .section()?
            .write(options.format);
    };
    let check_options =
        visibility::check_options(options, workspace.root.clone().into_std_path_buf());
    let mut analysis = visibility::analyze(&module_graph, &check_options);
    progress.step("Deciding the undecided candidates with rust-analyzer");
    let run = visibility::precise::Run {
        manifest: workspace.root.join("Cargo.toml").into_std_path_buf(),
        root: workspace.root.as_std_path(),
        target_dir: workspace.target_dir.as_std_path(),
        limits,
    };
    let summary = visibility::precise::decide(&module_graph, &mut analysis, &run)?;
    visibility::refine(&module_graph, &check_options, &mut analysis);
    visibility::skip_generated(&module_graph, &mut analysis);
    // A gate must not pass on a part of the decisions.
    if options.fail_on_issues && summary.incomplete() {
        return Err(error::CliError::new(
            error::ErrorCode::PreciseFailed,
            format!(
                "`--precise` did not decide each candidate. {}",
                summary.human_line()
            ),
        )
        .with_help(
            "Raise `--precise-timeout` or `--precise-memory-limit`, or run without \
             `--precise`.",
        ));
    }
    if summary.left > 0 {
        render::warn(&summary.human_line());
    }
    let precise = Some(summary);
    analysis.retain_kinds(&options.kinds);
    let report = visibility::report::VisibilityReport::new(visibility::report::Inputs {
        graph: &module_graph,
        analysis: &analysis,
        precise,
        enforced: options.fail_on_issues,
    });
    report.section()?.write(options.format)
}

/// Runs the `deps` command and returns the process exit code.
fn run_deps(options: &deps::Options) -> u8 {
    deps_report(options).unwrap_or_else(|err| {
        err.emit(options.format);
        EXIT_INVALID_INPUT
    })
}

fn deps_report(options: &deps::Options) -> Result<u8, CliError> {
    deps::section(&deps::run(options)?)?.write(options.format)
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
    render::mutants::report_section(&report)?.write(options.format)
}

fn inspect_report(options: &mutation::run::Options, id: &str) -> Result<u8, CliError> {
    if matches!(
        options.format,
        cli::Format::Sarif | cli::Format::Codeclimate
    ) {
        return Err(CliError::new(
            error::ErrorCode::InvalidArgument,
            "`--inspect` has no SARIF or CodeClimate output",
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
