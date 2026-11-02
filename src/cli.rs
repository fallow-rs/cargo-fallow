//! The command-line interface.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::time::Duration;

use clap::error::ErrorKind;
use clap::{Args, Parser, Subcommand, ValueEnum};
use fallow_report_protocol::exit_code::{EXIT_INVALID_INPUT, EXIT_OK};

use crate::error::{CliError, ErrorCode};
use crate::mutation::guard::{DEFAULT_LIMIT_MIB, DEFAULT_LOW_PERCENT};
use crate::mutation::run::{TierChoice, safe_jobs};

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
    /// Run the `mutants` command with these options.
    Mutants(Box<crate::mutation::run::Options>),
    /// Run the hidden watchdog of the memory guard.
    Watchdog {
        /// The file that lists the root of each process tree.
        roots: PathBuf,
        /// The folder of the run.
        sweep: Option<PathBuf>,
        /// True when the folder of the run goes when the parent is gone.
        remove: bool,
    },
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
    /// Run the tests against small changes in the source of one package.
    Mutants(MutantsArgs),
    /// Kill the test processes when the parent process dies. The memory guard
    /// starts this command.
    #[command(hide = true, name = "mutation-watchdog")]
    MutationWatchdog {
        /// The file that lists the root of each process tree.
        roots: PathBuf,
        /// The folder of the run. A test binary in this folder dies with the parent.
        #[arg(long, value_name = "DIR")]
        sweep: Option<PathBuf>,
        /// Remove the folder of the run when the parent is gone.
        #[arg(long)]
        remove: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum TierArg {
    /// All mutants in one build, with a runtime switch.
    Schema,
    /// One rebuild for each mutant.
    Rebuild,
    /// Both tiers.
    All,
}

#[derive(Debug, Args)]
struct MutantsArgs {
    /// Path to the Cargo.toml of the workspace.
    #[arg(long, value_name = "PATH")]
    manifest_path: Option<PathBuf>,
    /// The package to mutate. A workspace with one package needs no value.
    #[arg(long, short = 'p')]
    package: Option<String>,
    /// A new folder for the copies and the build output. The folder stays after
    /// the run. Without this flag the run uses a temporary folder.
    #[arg(long, value_name = "PATH")]
    work_dir: Option<PathBuf>,
    /// The output format.
    #[arg(long, value_enum, default_value_t = Format::Human)]
    format: Format,
    /// Hide the progress lines on stderr.
    #[arg(long)]
    quiet: bool,
    /// Run only the mutants on lines that changed since the fork point of this ref.
    /// The changes of the working tree and the untracked files count.
    #[arg(long, value_name = "REF")]
    in_diff: Option<String>,
    /// Exit 1 when a mutant survives. A mutant with `no_coverage` does not count.
    #[arg(long)]
    fail_on_survivors: bool,
    /// Show one mutant and the tests that execute it. This runs no mutant. It builds
    /// the coverage map when the cache has no map for this source.
    #[arg(long, value_name = "ID", conflicts_with_all = [
        "in_diff", "fail_on_survivors", "tier", "timeout", "time_budget", "max_mutants",
        "no_retry", "no_doctests", "no_schema", "no_selection", "no_cache", "no_run", "files",
    ])]
    inspect: Option<String>,
    /// The mutants to run.
    #[arg(long, value_enum, default_value_t = TierArg::All)]
    tier: TierArg,
    /// The test processes that run at the same time. The default is a quarter of
    /// the cores, with a maximum of 2, because a mutant can allocate without limit.
    #[arg(long, short = 'j', value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    jobs: Option<u32>,
    /// Kill a test process above this footprint, in MiB.
    #[arg(long = "memory-limit", value_name = "MIB", default_value_t = DEFAULT_LIMIT_MIB, value_parser = clap::value_parser!(u64).range(1..))]
    memory_limit_mib: u64,
    /// Kill the largest process tree when the free memory is under this percentage.
    #[arg(long, value_name = "PERCENT", default_value_t = DEFAULT_LOW_PERCENT, value_parser = clap::value_parser!(u32).range(0..=100))]
    low_memory_percent: u32,
    /// Run without the memory guard. Use this only when another limit protects
    /// the machine: a mutant can fill the memory and stop the machine.
    #[arg(long)]
    no_memory_guard: bool,
    /// The time limit of each mutant run. The default is 5 times the baseline,
    /// with a minimum of 20 seconds.
    #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(1..))]
    timeout: Option<u64>,
    /// Start no new mutant after this time. The other mutants stay `pending`.
    #[arg(long, value_name = "SECONDS")]
    time_budget: Option<u64>,
    /// Run at most this many mutants. The other mutants stay `pending`.
    #[arg(long, value_name = "N")]
    max_mutants: Option<usize>,
    /// Give no mutant a second run. Without this flag a survivor, a timeout and a
    /// kill get a second run. A second run stops a flaky test from giving a verdict.
    #[arg(long)]
    no_retry: bool,
    /// Do not run the doctests for a survivor.
    #[arg(long)]
    no_doctests: bool,
    /// Run the schema operators through rebuilds, with no schema build. This is
    /// slow, and it checks that the switch behaves as a plain edit of the source.
    #[arg(long)]
    no_schema: bool,
    /// Run each test for each mutant, with no coverage build. This is slow, and it
    /// checks that the selection does not change a verdict.
    #[arg(long)]
    no_selection: bool,
    /// Do not read or write the coverage map and the journal of the run.
    #[arg(long)]
    no_cache: bool,
    /// Compile and run the baselines, and run no mutant.
    #[arg(long)]
    no_run: bool,
    /// Keep the temporary work folder after the run.
    #[arg(long)]
    keep_work_dir: bool,
    /// Run each mutant that the tests of the package do not detect against the tests
    /// of each workspace member. Without this flag only the tests of the package run,
    /// and the report names the other members that depend on it.
    #[arg(long)]
    test_workspace: bool,
    /// Put mutants only in this file or folder, by the path from the workspace root.
    /// The flag can repeat. A value that matches no file of the package is an error.
    #[arg(long = "file", short = 'f', value_name = "PATH")]
    files: Vec<String>,
}

impl From<MutantsArgs> for crate::mutation::run::Options {
    fn from(args: MutantsArgs) -> Self {
        let cores = std::thread::available_parallelism().map_or(1, usize::from);
        Self {
            manifest_path: args.manifest_path,
            package: args.package,
            work_dir: args.work_dir,
            tier: match args.tier {
                TierArg::Schema => TierChoice::Schema,
                TierArg::Rebuild => TierChoice::Rebuild,
                TierArg::All => TierChoice::All,
            },
            jobs: args
                .jobs
                .map_or_else(|| safe_jobs(cores), |jobs| jobs as usize),
            memory_limit_mib: args.memory_limit_mib,
            low_memory_percent: args.low_memory_percent,
            memory_guard: !args.no_memory_guard,
            timeout: args.timeout.map(Duration::from_secs),
            time_budget: args.time_budget.map(Duration::from_secs),
            max_mutants: args.max_mutants,
            retry: !args.no_retry,
            doctests: !args.no_doctests,
            no_schema: args.no_schema,
            selection: !args.no_selection,
            cache: !args.no_cache,
            no_run: args.no_run,
            keep_work_dir: args.keep_work_dir,
            test_workspace: args.test_workspace,
            files: args.files,
            in_diff: args.in_diff,
            fail_on_survivors: args.fail_on_survivors,
            inspect: args.inspect,
            format: args.format,
            quiet: args.quiet,
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
            Some(Command::Mutants(args)) => Parsed::Mutants(Box::new(args.into())),
            Some(Command::MutationWatchdog {
                roots,
                sweep,
                remove,
            }) => Parsed::Watchdog {
                roots,
                sweep,
                remove,
            },
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
