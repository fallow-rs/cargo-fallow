//! The structure checks: crate and module boundaries, file and function size,
//! complexity per function, and duplicated code.
//!
//! Each check is a subcommand with its own report. The four reports share the
//! envelope in [`report`], the SARIF form in [`sarif`] and the configuration
//! root `[workspace.metadata.fallow]` in [`config`]. [`source`] walks the module
//! tree once and parses each file once through the bounded parse.

pub mod boundaries;
pub mod complexity;
pub mod config;
pub mod dupes;
mod git;
pub mod report;
pub mod sarif;
pub mod size;
pub mod source;

use std::path::PathBuf;

use clap::{Args, Subcommand};
use fallow_report_protocol::exit_code::EXIT_INVALID_INPUT;

use crate::cli::Format;

/// The structure subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check the crate, module and source rules in
    /// `[workspace.metadata.fallow.boundaries]`.
    Boundaries(BoundariesArgs),
    /// Report large files and functions. With `--base` or `--staged`, also report
    /// a file over the line limit that grew.
    Size(SizeArgs),
    /// Report functions over the cyclomatic or the cognitive complexity limit.
    Complexity(ComplexityArgs),
    /// Find duplicated code in the normalized token streams of the workspace.
    Dupes(DupesArgs),
}

/// The flags that each structure subcommand has.
#[derive(Debug, Clone, Args)]
pub struct CommonArgs {
    /// Path to the Cargo.toml of the workspace.
    #[arg(long, value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,
    /// The output format.
    #[arg(long, value_enum, default_value_t = Format::Human)]
    pub format: Format,
    /// Hide the progress lines on stderr.
    #[arg(long)]
    pub quiet: bool,
}

/// The flags of `boundaries`.
#[derive(Debug, Clone, Args)]
pub struct BoundariesArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Exit 1 when a rule finds a violation.
    #[arg(long)]
    fail_on_violations: bool,
}

/// The flags of `size`.
#[derive(Debug, Clone, Args)]
pub struct SizeArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Compare each `.rs` file that HEAD changes with its version at the merge
    /// base of HEAD and this ref.
    #[arg(long, value_name = "REF", conflicts_with = "staged")]
    base: Option<String>,
    /// Compare each staged `.rs` file with its version in HEAD.
    #[arg(long)]
    staged: bool,
    /// Exit 1 when a file over the line limit grew, or a new file is over it.
    #[arg(long)]
    fail_on_growth: bool,
}

/// The flags of `complexity`.
#[derive(Debug, Clone, Args)]
pub struct ComplexityArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// The cyclomatic limit. A function over it is a finding. The default
    /// comes from the config, else 20.
    #[arg(long, value_name = "N")]
    max_cyclomatic: Option<u32>,
    /// The cognitive limit. A function over it is a finding. The default comes
    /// from the config, else 15.
    #[arg(long, value_name = "N")]
    max_cognitive: Option<u32>,
    /// Exit 1 when a function is over a limit.
    #[arg(long)]
    fail_on_complexity: bool,
}

/// The flags of `dupes`.
#[derive(Debug, Clone, Args)]
pub struct DupesArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// The normalization mode. The default comes from the config, else `mild`.
    #[arg(long, value_enum)]
    mode: Option<dupes::Mode>,
    /// The smallest clone, in tokens. The default comes from the config, else 50.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    min_tokens: Option<u32>,
    /// The smallest clone, in lines. The default comes from the config, else 5.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    min_lines: Option<u32>,
    /// Exit 1 when the duplicated lines are more than this percentage of all
    /// lines.
    #[arg(long, value_name = "PERCENT")]
    threshold: Option<f64>,
    /// Exit 1 when the run finds a clone group.
    #[arg(long)]
    fail_on_clones: bool,
}

impl Command {
    const fn common(&self) -> &CommonArgs {
        match self {
            Self::Boundaries(args) => &args.common,
            Self::Size(args) => &args.common,
            Self::Complexity(args) => &args.common,
            Self::Dupes(args) => &args.common,
        }
    }
}

/// Runs one structure subcommand and returns the process exit code.
pub fn run(command: &Command) -> u8 {
    let common = command.common();
    let result = match command {
        Command::Boundaries(args) => boundaries::run(args),
        Command::Size(args) => size::run(args),
        Command::Complexity(args) => complexity::run(args),
        Command::Dupes(args) => dupes::run(args),
    };
    result
        .and_then(|section| section.write(common.format))
        .unwrap_or_else(|err| {
            err.emit(common.format);
            EXIT_INVALID_INPUT
        })
}
