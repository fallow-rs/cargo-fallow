//! The `mutation` command: one package, one report.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cli::Format;
use crate::error::{CliError, ErrorCode};
use crate::gates::parse_edition;
use crate::mutation::engine::Engine;
use crate::mutation::files::{Collected, TargetRoot, collect};
use crate::mutation::guard::{Guard, Limits};
use crate::mutation::report::{FileSkip, Gaps, GuardInfo, Report, Summary};
use crate::mutation::workdir::{WorkDirs, prepare_copies};
use crate::workspace::{self, Member, Workspace};

/// The target kinds that the engine mutates. A proc-macro runs in the compiler,
/// so a test run does not select a mutant in it.
const MUTATED_KINDS: [&str; 6] = ["lib", "rlib", "dylib", "cdylib", "staticlib", "bin"];

/// The tiers that a run selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierChoice {
    /// Only the schema tier.
    Schema,
    /// Only the rebuild tier.
    Rebuild,
    /// Both tiers.
    All,
}

impl TierChoice {
    pub const fn has_schema(self) -> bool {
        matches!(self, Self::Schema | Self::All)
    }

    pub const fn has_rebuild(self) -> bool {
        matches!(self, Self::Rebuild | Self::All)
    }
}

/// The inputs of the command.
#[derive(Debug, Clone)]
pub struct Options {
    /// The manifest of the workspace or the package.
    pub manifest_path: Option<PathBuf>,
    /// The package to mutate. A workspace with one package needs no value.
    pub package: Option<String>,
    /// A new folder for the copies and the build output. Without a value the run
    /// uses a temporary folder and removes it at the end.
    pub work_dir: Option<PathBuf>,
    /// The selected tiers.
    pub tier: TierChoice,
    /// The test processes that run at the same time.
    pub jobs: usize,
    /// The limit for one test process.
    pub memory_limit_mib: u64,
    /// The floor of the free memory.
    pub low_memory_percent: u32,
    /// False only with `--no-memory-guard`.
    pub memory_guard: bool,
    /// The limit of each mutant run. Without a value it comes from the baseline.
    pub timeout: Option<Duration>,
    /// The time after which no new mutant starts.
    pub time_budget: Option<Duration>,
    /// The largest number of mutants to run.
    pub max_mutants: Option<usize>,
    /// False when a survivor and a timeout get no second run.
    pub retry: bool,
    /// False when the doctests stay out.
    pub doctests: bool,
    /// True when the schema operators run through rebuilds, with no schema build.
    pub no_schema: bool,
    /// True when the command compiles and runs the baselines, and runs no mutant.
    pub no_run: bool,
    /// True when the temporary work folder stays after the run.
    pub keep_work_dir: bool,
    /// The output format.
    pub format: Format,
    /// True when the progress lines stay hidden.
    pub quiet: bool,
}

/// A safe default for the parallel jobs: a mutant can allocate without limit,
/// so a low number limits the damage, and the machine stays usable.
pub fn safe_jobs(cores: usize) -> usize {
    (cores / 4).clamp(1, 2)
}

fn failure(action: &str, err: &io::Error) -> CliError {
    CliError::new(ErrorCode::MutationFailed, format!("cannot {action}: {err}"))
}

/// Runs the command.
pub fn run(options: &Options) -> Result<Report, CliError> {
    let start = Instant::now();
    let workspace = workspace::load(options.manifest_path.as_deref())?;
    let member = find_member(&workspace, options.package.as_deref())?;
    let dirs = WorkDirs::create(options.work_dir.as_deref())
        .map_err(|err| failure("create the work folder", &err))?;
    let result = run_in(options, &workspace, member, &dirs, start);
    dirs.finish(options.keep_work_dir);
    result
}

fn run_in(
    options: &Options,
    workspace: &Workspace,
    member: &Member,
    dirs: &WorkDirs,
    start: Instant,
) -> Result<Report, CliError> {
    let guard = start_guard(options, dirs)?;
    prepare_copies(workspace.root.as_std_path(), dirs, &guard)
        .map_err(|err| failure("copy the source tree", &err))?;
    let collected = collect(&dirs.scratch, &target_roots(member));
    let engine = Engine {
        options,
        guard: &guard,
        dirs,
        package_dir: package_folder(member),
        files: &collected.files,
        start,
    };
    let (rows, stages) = engine.run()?;
    let summary = Summary::of(&rows);
    Ok(Report {
        kind: "mutation",
        package: member.name.clone(),
        version: member.version.clone(),
        summary,
        stopped_by: stages.stopped_by,
        elapsed_ms: millis(start.elapsed()),
        baseline: stages.baseline,
        schema: stages.schema,
        rebuild: stages.rebuild,
        gaps: gaps_of(&collected, stages.doctests),
        guard: GuardInfo {
            enabled: options.memory_guard,
            memory_limit_mib: options.memory_limit_mib,
            low_memory_percent: options.low_memory_percent,
            kills: guard.kills(),
            jobs: options.jobs,
        },
        mutants: rows,
    })
}

/// A duration in whole milliseconds.
pub fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn start_guard(options: &Options, dirs: &WorkDirs) -> Result<Guard, CliError> {
    let help = "Pass `--no-memory-guard` only when another limit protects the machine.";
    let mut guard = if options.memory_guard {
        let limits = Limits::new(options.memory_limit_mib, options.low_memory_percent);
        Guard::start(limits)
            .map_err(|err| failure("start the memory guard", &err).with_help(help))?
    } else {
        Guard::disabled()
    };
    if cfg!(unix) {
        let exe = std::env::current_exe().map_err(|err| failure("find this program", &err))?;
        guard
            .start_watchdog(&exe, dirs.root.join("watchdog-roots"))
            .map_err(|err| failure("start the watchdog", &err))?;
    }
    Ok(guard)
}

fn find_member<'a>(
    workspace: &'a Workspace,
    package: Option<&str>,
) -> Result<&'a Member, CliError> {
    let found = match package {
        Some(name) => workspace.members.iter().find(|member| member.name == name),
        None => match workspace.members.as_slice() {
            [member] => Some(member),
            _ => {
                return Err(CliError::new(
                    ErrorCode::InvalidArgument,
                    "the workspace has more than one package, so `--package` is required",
                ));
            }
        },
    };
    found.ok_or_else(|| {
        CliError::new(
            ErrorCode::InvalidArgument,
            format!(
                "the workspace has no package `{}`",
                package.unwrap_or_default()
            ),
        )
    })
}

/// The folder of the package, relative to the workspace root.
fn package_folder(member: &Member) -> PathBuf {
    Path::new(&member.manifest_path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

fn target_roots(member: &Member) -> Vec<TargetRoot> {
    member
        .targets
        .iter()
        .filter(|target| {
            target
                .kind
                .iter()
                .any(|kind| MUTATED_KINDS.contains(&kind.as_str()))
        })
        .filter(|target| !Path::new(&target.src_path).is_absolute())
        .map(|target| TargetRoot {
            path: target.src_path.clone(),
            edition: parse_edition(&target.edition),
        })
        .collect()
}

fn gaps_of(collected: &Collected, doctests: bool) -> Gaps {
    let mut gaps = Gaps {
        files: collected.files.len(),
        files_skipped: collected
            .skipped
            .iter()
            .map(|file| FileSkip {
                path: file.path.clone(),
                reason: file.reason.clone(),
            })
            .collect(),
        unreached_src_files: collected.unreached,
        features: "default",
        doctests,
        ..Gaps::default()
    };
    for file in &collected.files {
        gaps.macro_tokens += file.found.skipped.macro_tokens;
        gaps.const_context += file.found.skipped.const_context;
        gaps.let_chain += file.found.skipped.let_chain;
        gaps.test_code += file.found.skipped.test_code;
    }
    gaps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_default_to_a_safe_value() {
        assert_eq!(safe_jobs(1), 1);
        assert_eq!(safe_jobs(7), 1);
        assert_eq!(safe_jobs(8), 2);
        assert_eq!(safe_jobs(128), 2);
    }
}
