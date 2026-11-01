//! The command-line interface.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::time::Duration;

use clap::error::ErrorKind;
use clap::{Args, Parser, Subcommand, ValueEnum};
use fallow_report_protocol::exit_code::{EXIT_INVALID_INPUT, EXIT_OK};

use crate::error::{CliError, ErrorCode};

/// The name that Cargo passes as the first argument to a subcommand binary.
const CARGO_SUBCOMMAND: &str = "fallow";

/// The output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// A short summary for a person.
    Human,
    /// The report as JSON.
    Json,
    /// A SARIF 2.1.0 log.
    Sarif,
}

/// The options of one `check` run.
#[derive(Debug, Clone)]
pub struct Options {
    /// The `Cargo.toml` of the workspace, when the user gave one.
    pub manifest_path: Option<PathBuf>,
    /// The output format.
    pub format: Format,
    /// True when progress lines stay hidden.
    pub quiet: bool,
    /// True when a target root file that does not parse fails the run.
    pub fail_on_parse_error: bool,
}

/// The result of argument parsing.
#[derive(Debug)]
pub enum Parsed {
    /// Run `check` with these options.
    Run(Options),
    /// Run the hidden `schema-spike` command with these options.
    Spike(crate::mutation::spike::Options),
    /// Stop with this exit code. The parser already wrote its output.
    Exit(u8),
}

#[derive(Debug, Parser)]
#[command(
    name = "cargo-fallow",
    bin_name = "cargo fallow",
    version,
    about = "Codebase analysis for Rust workspaces",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    check: CheckArgs,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Analyze the workspace. This is the default command.
    Check(CheckArgs),
    /// Measure mutant schemata on one crate. The M1 spike uses this command.
    #[command(hide = true)]
    SchemaSpike(SpikeArgs),
}

#[derive(Debug, Args)]
struct SpikeArgs {
    /// Path to the Cargo.toml of the workspace.
    #[arg(long, value_name = "PATH")]
    manifest_path: PathBuf,
    /// The package to mutate.
    #[arg(long)]
    package: String,
    /// A new folder for the copies, the build output and the logs.
    #[arg(long, value_name = "PATH")]
    work_dir: PathBuf,
    /// Do not run the baseline tests.
    #[arg(long)]
    skip_tests: bool,
    /// The time limit of each test suite, in seconds.
    #[arg(long, value_name = "SECONDS", default_value_t = 900)]
    test_timeout: u64,
}

impl From<SpikeArgs> for crate::mutation::spike::Options {
    fn from(args: SpikeArgs) -> Self {
        Self {
            manifest_path: args.manifest_path,
            package: args.package,
            work_dir: args.work_dir,
            skip_tests: args.skip_tests,
            test_limit: Duration::from_secs(args.test_timeout),
        }
    }
}

#[derive(Debug, Args)]
struct CheckArgs {
    /// Path to the Cargo.toml of the workspace.
    #[arg(long, value_name = "PATH")]
    manifest_path: Option<PathBuf>,
    /// The output format.
    #[arg(long, value_enum, default_value_t = Format::Human)]
    format: Format,
    /// Hide the progress lines on stderr.
    #[arg(long)]
    quiet: bool,
    /// Exit 1 when a target root file does not parse.
    #[arg(long)]
    fail_on_parse_error: bool,
}

impl From<CheckArgs> for Options {
    fn from(args: CheckArgs) -> Self {
        Self {
            manifest_path: args.manifest_path,
            format: args.format,
            quiet: args.quiet,
            fail_on_parse_error: args.fail_on_parse_error,
        }
    }
}

/// Removes the `fallow` argument that Cargo inserts after the binary name.
pub fn strip_cargo_subcommand(mut args: Vec<OsString>) -> Vec<OsString> {
    if args.get(1).is_some_and(|arg| arg == CARGO_SUBCOMMAND) {
        args.remove(1);
    }
    args
}

/// Parses the arguments. On a usage error, writes the error and returns the
/// exit code.
pub fn parse(args: &[OsString]) -> Parsed {
    match Cli::try_parse_from(args) {
        Ok(cli) => match cli.command {
            Some(Command::SchemaSpike(spike)) => Parsed::Spike(spike.into()),
            Some(Command::Check(check)) => Parsed::Run(check.into()),
            None => Parsed::Run(cli.check.into()),
        },
        Err(err) => Parsed::Exit(report_usage_error(&err, args)),
    }
}

fn report_usage_error(err: &clap::Error, args: &[OsString]) -> u8 {
    if matches!(
        err.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        // A failed write of the help text changes nothing: the exit code stays 0.
        let _ = err.print();
        return EXIT_OK;
    }
    if wants_json(args) {
        CliError::new(ErrorCode::InvalidArgument, first_line(err))
            .with_help("Run `cargo fallow --help` for the list of flags.")
            .emit(Format::Json);
    } else {
        // The exit code is 2 also when the write to stderr fails.
        let _ = err.print();
    }
    EXIT_INVALID_INPUT
}

/// The first line of the clap message, without the `error: ` prefix.
fn first_line(err: &clap::Error) -> String {
    let rendered = err.render().to_string();
    let line = rendered.lines().next().unwrap_or_default();
    line.strip_prefix("error: ").unwrap_or(line).to_owned()
}

/// True when the raw arguments ask for JSON output.
fn wants_json(args: &[OsString]) -> bool {
    let json = OsStr::new("json");
    args.iter().enumerate().any(|(index, arg)| {
        arg == "--format=json"
            || (arg == "--format" && args.get(index + 1).map(OsString::as_os_str) == Some(json))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn cargo_argument_is_removed_once() {
        let stripped = strip_cargo_subcommand(os(&["cargo-fallow", "fallow", "fallow"]));
        assert_eq!(stripped, os(&["cargo-fallow", "fallow"]));
        let direct = strip_cargo_subcommand(os(&["cargo-fallow", "check"]));
        assert_eq!(direct, os(&["cargo-fallow", "check"]));
    }

    #[test]
    fn json_request_is_found_in_both_spellings() {
        assert!(wants_json(&os(&["x", "--format", "json", "--bogus"])));
        assert!(wants_json(&os(&["x", "--bogus", "--format=json"])));
        assert!(!wants_json(&os(&["x", "--format", "sarif", "json"])));
        assert!(!wants_json(&os(&["x", "--format"])));
    }

    #[test]
    fn check_is_the_default_command() {
        let Parsed::Run(options) = parse(&os(&["cargo-fallow", "--format", "sarif", "--quiet"]))
        else {
            panic!("the arguments are valid");
        };
        assert_eq!(options.format, Format::Sarif);
        assert!(options.quiet);
        assert!(!options.fail_on_parse_error);
    }
}
