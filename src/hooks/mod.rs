//! The `hooks` command: the agent gate and the git pre-commit hook.
//!
//! - `classify` finds a `git commit` or a `git push` in a shell command.
//! - `runtime` is `hooks gate-runtime`: the decision of the gate.
//! - `base` gives the findings of the base commit, so the gate blocks only on
//!   new findings.
//! - `report` writes the JSON report of a block without following a link.
//! - `settings` merges the handler into the configuration of each harness.
//! - `files` writes the generated scripts with a checksum.
//! - `install` installs, removes and shows the gate and the hook.

pub mod base;
pub mod classify;
pub mod files;
pub mod install;
pub mod report;
pub mod runtime;
pub mod settings;

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

use crate::cli::Format;

/// The surface of a hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Target {
    /// The Claude Code and Codex gate before a `git commit` or a `git push`.
    Agent,
    /// The git pre-commit hook.
    Git,
}

/// The subcommands of `hooks`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Install the agent gate or the git pre-commit hook.
    ///
    /// The agent gate blocks a `git commit` or a `git push` of Claude Code or
    /// Codex when `cargo fallow check` finds a new finding in the changed
    /// files. The git hook does the same for each commit. A finding that the
    /// base commit already has does not block.
    Install(InstallArgs),
    /// Remove the agent gate or the git pre-commit hook. A file that someone
    /// edited stays, unless you give `--force`.
    Uninstall(InstallArgs),
    /// Show the state of the agent gate and the git pre-commit hook.
    Status(StatusArgs),
    /// The runtime of the gate script. The generated scripts run it.
    #[command(hide = true, name = "gate-runtime")]
    GateRuntime {
        /// The operation.
        #[arg(value_enum)]
        operation: runtime::Operation,
    },
}

/// The flags of `hooks install` and `hooks uninstall`.
#[derive(Debug, Clone, Args)]
pub struct InstallArgs {
    /// The hook surface.
    #[arg(long, value_enum)]
    pub target: Target,
    /// Only this harness of the agent target. The default is each harness
    /// whose folder exists, else Claude Code.
    #[arg(long, value_enum, value_name = "HARNESS")]
    pub agent: Option<settings::Harness>,
    /// Path to the Cargo.toml of the workspace.
    #[arg(long, value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,
    /// Show the changes, and write nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Replace or remove a script that someone edited, or a hook that
    /// cargo-fallow did not write.
    #[arg(long)]
    pub force: bool,
    /// The output format: `human` or `json`.
    #[arg(long, default_value = "human", value_parser = crate::cli::human_or_json())]
    pub format: Format,
}

/// The flags of `hooks status`.
#[derive(Debug, Clone, Args)]
pub struct StatusArgs {
    /// Path to the Cargo.toml of the workspace.
    #[arg(long, value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,
    /// The output format: `human` or `json`.
    #[arg(long, default_value = "human", value_parser = crate::cli::human_or_json())]
    pub format: Format,
}

impl Command {
    /// The output format.
    pub const fn format(&self) -> Format {
        match self {
            Self::Install(args) | Self::Uninstall(args) => args.format,
            Self::Status(args) => args.format,
            Self::GateRuntime { .. } => Format::Human,
        }
    }
}

/// Runs one `hooks` subcommand and returns the exit code.
pub fn run(command: &Command) -> u8 {
    let result = match command {
        Command::GateRuntime { operation } => return runtime::run(*operation),
        Command::Install(args) => install::install(args),
        Command::Uninstall(args) => install::uninstall(args),
        Command::Status(args) => install::status(args),
    };
    result.unwrap_or_else(|err| {
        err.emit(command.format());
        fallow_report_protocol::exit_code::EXIT_INVALID_INPUT
    })
}
