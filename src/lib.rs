//! `cargo fallow`: codebase analysis for Rust workspaces.
//!
//! The `cargo-fallow` binary is a thin wrapper around [`run`]. The library
//! holds every module, so the `missing_docs` lint checks the public API.
//! The public API is [`run`] only. The modules are internal.

// Only the workspace loader, the mutation runner, the git reader of the
// structure checks and the base export of the hook gate spawn a process. See
// `.clippy.toml`.
#![cfg_attr(not(test), deny(clippy::disallowed_methods))]

mod agent;
mod api;
mod attrs;
mod baseline;
mod check;
mod cli;
mod deps;
mod diff;
mod error;
mod explain;
mod files;
mod finding;
mod fix;
mod gates;
mod graph;
mod health;
mod hooks;
mod hotspots;
mod init;
mod modules;
mod mutation;
mod parse;
mod render;
mod report;
mod rules;
mod schema;
mod scope;
mod section;
mod structure;
mod suppress;
mod trace;
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
    let parsed = cli::parse(&args);
    if let Some(code) = refuse_badge(&parsed) {
        return ExitCode::from(code);
    }
    let code = match parsed {
        Parsed::Run(options) => check::run(&options),
        Parsed::Mutants(options) => run_mutants(&options),
        Parsed::Deps(options) => run_deps(&options),
        Parsed::Init(args) => init::run(&args).unwrap_or_else(|err| {
            err.emit(args.format());
            EXIT_INVALID_INPUT
        }),
        Parsed::Explain(args) => explain::run(&args).unwrap_or_else(|err| {
            err.emit(args.format());
            EXIT_INVALID_INPUT
        }),
        Parsed::Fix(args) => fix::run(&args).unwrap_or_else(|err| {
            err.emit(args.format());
            EXIT_INVALID_INPUT
        }),
        Parsed::Files(args) => run_files(&args),
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
        Parsed::Hooks(command) => hooks::run(&command),
        Parsed::Agent(command) => agent::run(&command),
        Parsed::Schema => match render::write_stdout(schema::text().trim_end()) {
            Ok(()) => EXIT_OK,
            Err(err) => {
                CliError::output(&err).emit(cli::Format::Human);
                EXIT_INVALID_INPUT
            }
        },
        Parsed::Exit(code) => code,
    };
    ExitCode::from(code)
}

/// Stops a command other than `check` that asks for `--format badge`, before
/// it runs: only `check` has the health score.
fn refuse_badge(parsed: &Parsed) -> Option<u8> {
    let format = match parsed {
        Parsed::Mutants(options) => options.format,
        Parsed::Deps(options) => options.format,
        Parsed::Visibility(options) => options.format,
        Parsed::Files(args) => args.format(),
        Parsed::Fix(args) => args.format(),
        Parsed::Explain(args) => args.format(),
        Parsed::Init(args) => args.format(),
        Parsed::Structure(command) => command.format(),
        _ => return None,
    };
    (format == cli::Format::Badge).then(|| {
        CliError::new(
            error::ErrorCode::InvalidArgument,
            "`--format badge` needs the health score of `cargo fallow check`",
        )
        .with_help("Run `cargo fallow check --format badge`.")
        .emit(format);
        EXIT_INVALID_INPUT
    })
}

/// Runs the `visibility` command and returns the process exit code.
fn run_visibility(options: &cli::VisibilityOptions) -> u8 {
    visibility_report(options).unwrap_or_else(|err| {
        err.emit(options.format);
        EXIT_INVALID_INPUT
    })
}

fn visibility_report(options: &cli::VisibilityOptions) -> Result<u8, CliError> {
    if options.trace.is_some() {
        crate::trace::check_format(options.format, "visibility")?;
    }
    let progress = render::Progress::new(options.quiet);
    progress.step("Reading cargo metadata");
    let workspace = workspace::load(options.manifest_path.as_deref(), &options.cargo_flags)?;
    let options = &visibility::config::resolve(&workspace, options)?;
    // `--kind` narrows the findings, so a baseline entry of another kind is
    // not stale.
    let scope = scope::Scope::load(&workspace, &options.scope)?.narrowed(!options.kinds.is_empty());
    progress.step("Building the module graph");
    let production = graph::Production {
        bins: options.production.clone(),
    };
    // The code of a consumer workspace counts as a use.
    let extended = visibility::consumers::extend(&workspace, options)?;
    let graph_workspace = extended
        .as_ref()
        .map_or(&workspace, |extended| &extended.workspace);
    let module_graph = graph::build(graph_workspace, &production);
    if let Some(target) = &options.trace {
        progress.step("Tracing the item");
        let check_options =
            visibility::check_options(options, workspace.root.as_std_path().to_path_buf());
        let traced =
            visibility::trace::trace(&module_graph, &check_options, &options.kinds, target)?;
        return crate::trace::write(options.format, &traced.human(), &traced);
    }
    progress.step("Checking the visibility");
    let Some(limits) = options.precise else {
        return scope
            .annotate(
                visibility::fast_report(&module_graph, options, &workspace, &scope).section()?,
            )?
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
    analysis.retain_scope(&module_graph, &scope);
    let mut report = visibility::report::VisibilityReport::new(visibility::report::Inputs {
        graph: &module_graph,
        analysis: &analysis,
        precise,
        enforced: options.fail_on_issues,
    });
    report.regate(scope.rules());
    scope.annotate(report.section()?)?.write(options.format)
}

/// Runs the `files` command and returns the process exit code.
fn run_files(args: &files::FilesArgs) -> u8 {
    files::run(args)
        .and_then(|section| section.write(args.format()))
        .unwrap_or_else(|err| {
            err.emit(args.format());
            EXIT_INVALID_INPUT
        })
}

/// Runs the `deps` command and returns the process exit code.
fn run_deps(options: &deps::Options) -> u8 {
    deps_report(options).unwrap_or_else(|err| {
        err.emit(options.format);
        EXIT_INVALID_INPUT
    })
}

fn deps_report(options: &deps::Options) -> Result<u8, CliError> {
    if let Some(target) = &options.trace {
        return deps::trace::run(options, target);
    }
    deps::run(options)?.write(options.format)
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
        cli::Format::Sarif
            | cli::Format::Codeclimate
            | cli::Format::Compact
            | cli::Format::Markdown
            | cli::Format::GithubAnnotations
            | cli::Format::GithubSummary
            | cli::Format::Badge
    ) {
        return Err(CliError::new(
            error::ErrorCode::InvalidArgument,
            "`--inspect` has no SARIF, CodeClimate, compact, markdown, GitHub or badge output",
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
