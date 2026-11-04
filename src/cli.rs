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
    /// A CodeClimate report: a JSON array of issues, for GitLab code quality.
    Codeclimate,
}

/// The options of the `visibility` command.
#[derive(Debug, Clone)]
pub struct VisibilityOptions {
    /// The `Cargo.toml` of the workspace, when the user gave one.
    pub manifest_path: Option<PathBuf>,
    /// The output format.
    pub format: Format,
    /// True when progress lines stay hidden.
    pub quiet: bool,
    /// The production binaries, as `(package, bin)`. Empty means every binary.
    pub production: Vec<(String, String)>,
    /// The library crates whose public items are an external API.
    pub exclude: Vec<String>,
    /// True when the fields of one struct change visibility as a group.
    pub uniform_fields: bool,
    /// The finding kinds to report. Empty means each kind.
    pub kinds: Vec<crate::visibility::FindingKind>,
    /// True when a finding fails the run.
    pub fail_on_issues: bool,
    /// The scope flags: `--changed-since` and the baseline flags.
    pub scope: crate::scope::ScopeArgs,
    /// The limits of `--precise` mode, or `None` for the fast mode.
    pub precise: Option<crate::visibility::precise::Limits>,
}

/// The result of argument parsing.
#[derive(Debug)]
pub enum Parsed {
    /// Run `check` with these options.
    Run(crate::check::Options),
    /// Run the `mutants` command with these options.
    Mutants(Box<crate::mutation::run::Options>),
    /// Run the visibility check.
    Visibility(VisibilityOptions),
    /// Run the hidden worker of `visibility --precise`.
    PreciseWorker(crate::visibility::precise::WorkerArgs),
    /// Run the unused dependency check.
    Deps(crate::deps::Options),
    /// Write a starting configuration.
    Init(crate::init::InitArgs),
    /// Explain one rule.
    Explain(crate::explain::ExplainArgs),
    /// Apply the fixable actions.
    Fix(crate::fix::FixArgs),
    /// Run the unused file check.
    Files(crate::files::FilesArgs),
    /// Run one structure check: boundaries, size, complexity or dupes.
    Structure(Box<crate::structure::Command>),
    /// Run the hidden watchdog of the memory guard.
    Watchdog {
        /// The file that lists the root of each process tree.
        roots: PathBuf,
        /// The folder of the run.
        sweep: Option<PathBuf>,
        /// True when the folder of the run goes when the parent is gone.
        remove: bool,
    },
    /// Print the JSON Schema of the output.
    Schema,
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
    /// Run the static checks in one run: parse, deps, files, visibility,
    /// boundaries, size, complexity and dupes. This is the default command.
    ///
    /// The run reads the workspace once and writes one report with a section
    /// for each check. Each check keeps its own gate. `mutants` and `visibility
    /// --precise` are separate commands, because they build and run the tests or
    /// rust-analyzer and take minutes. The `deps` check resolves the
    /// dependencies with `cargo metadata`, so Cargo can use the network: use
    /// `--offline`, `--locked`, `--frozen` or `--skip deps`.
    Check(CheckArgs),
    /// Run the tests against small changes in the source of one package.
    Mutants(MutantsArgs),
    /// Report dependencies in Cargo.toml that no code uses.
    ///
    /// The command resolves the dependencies with `cargo metadata` and all
    /// features on, so Cargo can use the network. The command keeps
    /// `Cargo.lock` as it was before the run. Use `--offline`, `--locked` or
    /// `--frozen` to control the resolution.
    Deps(DepsArgs),
    /// Give a starting configuration for the workspace.
    ///
    /// The command runs the static checks and prints the tables of
    /// `[workspace.metadata.fallow]` with the limits at the current maximum. It
    /// saves the current findings as a baseline, so the first `cargo fallow
    /// check --fail-on-issues` passes. `--write` adds the tables to the root
    /// Cargo.toml and writes `fallow-baseline.json`. `--ci github` prints a
    /// workflow file.
    Init(crate::init::InitArgs),
    /// Explain a rule: its meaning, an example and the fix.
    ///
    /// `--format json` gives `rule`, `check`, `severity`, `meaning`, `example`,
    /// `fix` and `docs`. An unknown rule exits 2 and lists the rules.
    Explain(crate::explain::ExplainArgs),
    /// Apply the actions with `auto_fixable: true`: remove each unused
    /// dependency entry, move each misplaced entry to `[dev-dependencies]`, and
    /// narrow the visibility keyword of each `visibility` finding.
    ///
    /// The edit keeps the comments and the format of each Cargo.toml. A warning
    /// stays. Use `--dry-run` to see the edits first. A process without a
    /// terminal needs `--yes`. With a visibility edit, the command runs `cargo
    /// check --workspace --all-targets` before and after the edits. When the
    /// workspace does not load or build after the edits, the command writes
    /// back each file and exits 2.
    Fix(crate::fix::FixArgs),
    /// Report `.rs` files in the target folders that no target compiles.
    ///
    /// A file counts as used when a `mod` item, a `#[path]` attribute, a
    /// literal `include!` call or a Cargo target reaches it. The check skips
    /// test data folders, generated files and packages whose files the syntax
    /// cannot decide.
    Files(crate::files::FilesArgs),
    /// Report `pub` and `pub(crate)` items that can have a smaller visibility.
    Visibility(VisibilityArgs),
    /// The worker of `visibility --precise`. The `visibility` command starts it
    /// under a time limit and the memory guard.
    #[command(hide = true, name = "visibility-precise-worker")]
    PreciseWorker(crate::visibility::precise::WorkerArgs),
    #[command(flatten)]
    Structure(crate::structure::Command),
    /// Print the JSON Schema of each JSON document that the tool writes: the
    /// report of each command, the baseline file and the error document.
    Schema,
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
    /// A new folder for the copies and the build output. An existing folder or a
    /// symlink is an error. The folder stays after the run. Without this flag the
    /// run uses a temporary folder.
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
    #[arg(
        long,
        visible_alias = "fail-on-issues",
        overrides_with = "fail_on_survivors"
    )]
    fail_on_survivors: bool,
    /// Show one mutant and the tests that execute it. This runs no mutant. It builds
    /// the coverage map when the cache has no map for this source.
    #[arg(long, value_name = "ID", conflicts_with_all = [
        "in_diff", "fail_on_survivors", "tier", "timeout", "time_budget", "max_mutants",
        "no_retry", "no_doctests", "no_schema", "no_selection", "no_cache", "no_run", "files",
        "list",
    ])]
    inspect: Option<String>,
    /// With `--inspect`: read the coverage map only from the cache. The command then
    /// compiles nothing and runs no test. Exit 2 when the cache has no map.
    #[arg(long, requires = "inspect")]
    no_build: bool,
    /// List the mutants of the package, and run nothing. The command compiles
    /// nothing and runs no test. Each mutant has the status `pending`.
    #[arg(long, conflicts_with_all = [
        "fail_on_survivors", "timeout", "time_budget", "max_mutants", "no_retry",
        "no_doctests", "no_run", "keep_work_dir", "test_workspace",
    ])]
    list: bool,
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
            no_build: args.no_build,
            list: args.list,
            format: args.format,
            quiet: args.quiet,
        }
    }
}

#[derive(Debug, Args)]
struct VisibilityArgs {
    /// Path to the Cargo.toml of the workspace.
    #[arg(long, value_name = "PATH")]
    manifest_path: Option<PathBuf>,
    /// The output format.
    #[arg(long, value_enum, default_value_t = Format::Human)]
    format: Format,
    /// Hide the progress lines on stderr.
    #[arg(long)]
    quiet: bool,
    /// A production binary as `PACKAGE:BIN`. The flag can repeat. Without the
    /// flag, every binary of the workspace is a production binary.
    #[arg(long, value_name = "PACKAGE:BIN", value_parser = parse_production)]
    production: Vec<(String, String)>,
    /// A library crate whose public items are an external API. The flag can repeat.
    #[arg(long, value_name = "CRATE")]
    exclude_crate: Vec<String>,
    /// Change the visibility of the fields of one struct as a group, when all
    /// fields have the same written visibility.
    #[arg(long)]
    uniform_fields: bool,
    /// Report only this finding kind. The flag can repeat.
    #[arg(long = "kind", value_enum, value_name = "KIND")]
    kinds: Vec<VisibilityKindArg>,
    /// Exit 1 when the run has a finding.
    #[arg(long)]
    fail_on_issues: bool,
    #[command(flatten)]
    scope: crate::scope::ScopeArgs,
    /// Decide the findings that the syntax cannot decide with rust-analyzer: method
    /// calls on an unknown type and the output of proc-macros. This needs a build
    /// with the `precise` feature, and it runs the build scripts and proc-macros of
    /// the workspace.
    #[arg(long)]
    precise: bool,
    /// The time limit of `--precise` mode, in seconds. At the limit the undecided
    /// findings that are left stay unreported.
    #[arg(long, value_name = "SECONDS", default_value_t = crate::visibility::precise::DEFAULT_TIMEOUT_SECONDS, value_parser = clap::value_parser!(u64).range(1..), requires = "precise")]
    precise_timeout: u64,
    /// The memory limit of `--precise` mode, in MiB. Above the limit the guard
    /// stops the analysis, and the undecided findings that are left stay unreported.
    #[arg(long, value_name = "MIB", default_value_t = crate::visibility::precise::DEFAULT_MEMORY_MIB, value_parser = clap::value_parser!(u64).range(1..), requires = "precise")]
    precise_memory_limit: u64,
    /// The largest source file, in KiB, that `--precise` mode searches with name
    /// resolution. A candidate whose name is in a larger file keeps its visibility.
    #[arg(long, value_name = "KIB", default_value_t = crate::visibility::precise::DEFAULT_FILE_LIMIT_KIB, value_parser = clap::value_parser!(u64).range(1..), requires = "precise")]
    precise_file_limit: u64,
}

/// A finding kind of the `visibility` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum VisibilityKindArg {
    /// A `pub` item that no code reaches.
    DeadPublic,
    /// A `pub` item that only its own crate uses.
    UnnecessaryPublic,
    /// A `pub(crate)`, `pub(super)` or `pub(in path)` item that only its own module
    /// uses.
    UnnecessaryRestrictedVisibility,
}

pub fn parse_production(value: &str) -> Result<(String, String), String> {
    match value.split_once(':') {
        Some((package, bin)) if !package.is_empty() && !bin.is_empty() => {
            Ok((package.to_owned(), bin.to_owned()))
        }
        _ => Err(format!("`{value}` is not in the form PACKAGE:BIN")),
    }
}

impl From<VisibilityArgs> for VisibilityOptions {
    fn from(args: VisibilityArgs) -> Self {
        Self {
            manifest_path: args.manifest_path,
            format: args.format,
            quiet: args.quiet,
            production: args.production,
            exclude: args.exclude_crate,
            uniform_fields: args.uniform_fields,
            kinds: args
                .kinds
                .into_iter()
                .map(|kind| match kind {
                    VisibilityKindArg::DeadPublic => crate::visibility::FindingKind::DeadPublic,
                    VisibilityKindArg::UnnecessaryPublic => {
                        crate::visibility::FindingKind::UnnecessaryPublic
                    }
                    VisibilityKindArg::UnnecessaryRestrictedVisibility => {
                        crate::visibility::FindingKind::UnnecessaryRestricted
                    }
                })
                .collect(),
            fail_on_issues: args.fail_on_issues,
            scope: args.scope,
            precise: args.precise.then_some(crate::visibility::precise::Limits {
                timeout: Duration::from_secs(args.precise_timeout),
                memory_mib: args.precise_memory_limit,
                file_limit_kib: args.precise_file_limit,
            }),
        }
    }
}

#[derive(Debug, Args)]
struct DepsArgs {
    /// Path to the Cargo.toml of the workspace.
    #[arg(long, value_name = "PATH")]
    manifest_path: Option<PathBuf>,
    /// The output format.
    #[arg(long, value_enum, default_value_t = Format::Human)]
    format: Format,
    /// Hide the progress lines on stderr.
    #[arg(long)]
    quiet: bool,
    /// Exit 1 when a finding has the `error` severity. A warning does not count.
    #[arg(long)]
    fail_on_issues: bool,
    #[command(flatten)]
    scope: crate::scope::ScopeArgs,
    /// Give `--locked` to `cargo metadata`: fail when `Cargo.lock` must change.
    #[arg(long)]
    locked: bool,
    /// Give `--offline` to `cargo metadata`: do not use the network.
    #[arg(long)]
    offline: bool,
    /// Give `--frozen` to `cargo metadata`: the same as `--locked` and
    /// `--offline`.
    #[arg(long)]
    frozen: bool,
}

impl From<DepsArgs> for crate::deps::Options {
    fn from(args: DepsArgs) -> Self {
        let cargo_flags = cargo_flags(args.locked, args.offline, args.frozen);
        Self {
            manifest_path: args.manifest_path,
            format: args.format,
            quiet: args.quiet,
            fail_on_issues: args.fail_on_issues,
            cargo_flags,
            scope: args.scope,
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
    /// Run only these checks. The flag takes a list with commas and can repeat.
    #[arg(
        long,
        value_enum,
        value_name = "CHECK",
        value_delimiter = ',',
        conflicts_with = "skip"
    )]
    only: Vec<crate::check::CheckName>,
    /// Do not run these checks. The flag takes a list with commas and can repeat.
    #[arg(long, value_enum, value_name = "CHECK", value_delimiter = ',')]
    skip: Vec<crate::check::CheckName>,
    /// Exit 1 when the gate of a check that ran fails: parse-error,
    /// error-severity-findings (deps), unused-files, visibility-findings,
    /// boundary-violations, health-findings (complexity) and
    /// duplication-findings (dupes).
    #[arg(long)]
    fail_on_issues: bool,
    /// Exit 1 when a target root file does not parse.
    #[arg(long)]
    fail_on_parse_error: bool,
    #[command(flatten)]
    scope: crate::scope::ScopeArgs,
    /// Give `--locked` to the `cargo metadata` call of the deps check.
    #[arg(long)]
    locked: bool,
    /// Give `--offline` to the `cargo metadata` call of the deps check.
    #[arg(long)]
    offline: bool,
    /// Give `--frozen` to the `cargo metadata` call of the deps check.
    #[arg(long)]
    frozen: bool,
}

/// The `cargo metadata` flags that are on.
fn cargo_flags(locked: bool, offline: bool, frozen: bool) -> Vec<String> {
    [
        (locked, "--locked"),
        (offline, "--offline"),
        (frozen, "--frozen"),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .map(|(_, flag)| flag.to_owned())
    .collect()
}

impl From<CheckArgs> for crate::check::Options {
    fn from(args: CheckArgs) -> Self {
        Self {
            manifest_path: args.manifest_path,
            format: args.format,
            quiet: args.quiet,
            only: args.only,
            skip: args.skip,
            fail_on_issues: args.fail_on_issues,
            fail_on_parse_error: args.fail_on_parse_error,
            cargo_flags: cargo_flags(args.locked, args.offline, args.frozen),
            scope: args.scope,
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
            Some(Command::Visibility(args)) => Parsed::Visibility(args.into()),
            Some(Command::PreciseWorker(args)) => Parsed::PreciseWorker(args),
            Some(Command::Deps(args)) => Parsed::Deps(args.into()),
            Some(Command::Init(args)) => Parsed::Init(args),
            Some(Command::Explain(args)) => Parsed::Explain(args),
            Some(Command::Fix(args)) => Parsed::Fix(args),
            Some(Command::Files(args)) => Parsed::Files(args),
            Some(Command::Structure(command)) => Parsed::Structure(Box::new(command)),
            Some(Command::Schema) => Parsed::Schema,
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

    #[test]
    fn fail_on_issues_is_the_survivor_gate_of_mutants() {
        let Parsed::Mutants(options) = parse(&os(&["cargo-fallow", "mutants", "--fail-on-issues"]))
        else {
            panic!("the arguments are valid");
        };
        assert!(options.fail_on_survivors);
        let Parsed::Mutants(options) = parse(&os(&[
            "cargo-fallow",
            "mutants",
            "--fail-on-survivors",
            "--fail-on-issues",
        ])) else {
            panic!("the two gate flags are valid together");
        };
        assert!(options.fail_on_survivors);
        for conflict in [&["--list"][..], &["--inspect", "x"]] {
            let mut args = vec!["cargo-fallow", "mutants", "--fail-on-issues"];
            args.extend_from_slice(conflict);
            assert!(
                matches!(parse(&os(&args)), Parsed::Exit(EXIT_INVALID_INPUT)),
                "{conflict:?}"
            );
        }
    }
}
