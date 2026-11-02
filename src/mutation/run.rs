//! The `mutants` command: one package, one report.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use crate::cli::Format;
use crate::error::{CliError, ErrorCode};
use crate::gates::RustGateName;
use crate::gates::parse_edition;
use crate::mutation::coverage::{CoverageMap, LineTests};
use crate::mutation::diff::Changed;
use crate::mutation::engine::{Engine, rewrites_of};
use crate::mutation::files::{Collected, CrateFile, TargetRoot, collect};
use crate::mutation::guard::{Guard, Limits};
use crate::mutation::plan::{Planned, lines_of, rebuild_mutants, schema_mutants};
use crate::mutation::report::{
    CoverageState, DiffInfo, FileSkip, Gaps, GuardInfo, INSPECT_KIND, InspectCoverage,
    InspectReport, InspectedMutant, KIND, LANGUAGE, MutantRow, Report, Summary, actions_for,
    survivor_gate,
};
use crate::mutation::workdir::{WorkDirs, prepare_copies};
use crate::report::{SCHEMA_VERSION, TOOL_VERSION};
use crate::workspace::{self, Member, Workspace};

/// The target kinds that the engine mutates. A proc-macro runs in the compiler,
/// so a test run does not select a mutant in it.
const MUTATED_KINDS: [&str; 6] = ["lib", "rlib", "dylib", "cdylib", "staticlib", "bin"];

/// The length of a mutant id.
const ID_DIGITS: usize = 16;

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
    /// False when each mutant runs each test, with no coverage build.
    pub selection: bool,
    /// False when the run does not read or write its cache folder.
    pub cache: bool,
    /// True when the command compiles and runs the baselines, and runs no mutant.
    pub no_run: bool,
    /// True when the temporary work folder stays after the run.
    pub keep_work_dir: bool,
    /// True when the tests of each workspace member run for each mutant.
    pub test_workspace: bool,
    /// The files with mutants. An empty list means each file of the package.
    pub files: Vec<String>,
    /// The ref of `--in-diff`.
    pub in_diff: Option<String>,
    /// True when a survivor fails the run.
    pub fail_on_survivors: bool,
    /// The id of `--inspect`.
    pub inspect: Option<String>,
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
    let changed = options
        .in_diff
        .as_deref()
        .map(|reference| Changed::load(workspace.root.as_std_path(), reference))
        .transpose()?;
    let dirs = WorkDirs::create(
        options.work_dir.as_deref(),
        options.keep_work_dir,
        workspace.root.as_std_path(),
    )
    .map_err(|err| failure("create the work folder", &err))?;
    run_in(options, &workspace, member, &dirs, changed.as_ref(), start)
}

fn run_in(
    options: &Options,
    workspace: &Workspace,
    member: &Member,
    dirs: &WorkDirs,
    changed: Option<&Changed>,
    start: Instant,
) -> Result<Report, CliError> {
    let guard = start_guard(options, dirs)?;
    let target_dir = workspace.target_dir.as_std_path();
    prepare_copies(workspace.root.as_std_path(), target_dir, dirs, &guard)
        .map_err(|err| failure("copy the source tree", &err))?;
    let mut collected = collect(&dirs.scratch, &target_roots(member));
    retain_selected_files(options, &mut collected.files)?;
    let sites_outside = changed.map_or(0, |changed| {
        let removed = changed.retain_sites(&mut collected.files);
        if options.tier.has_schema() {
            removed
        } else {
            0
        }
    });
    let engine = Engine {
        options,
        guard: &guard,
        dirs,
        package_dir: package_folder(member),
        files: &collected.files,
        start,
        target_dir: target_dir.to_path_buf(),
        package: member.name.clone(),
        changed,
        select: package_selection(options, workspace, member),
        alone: RwLock::new(()),
    };
    let (mut rows, stages) = engine.run()?;
    let inspect = inspect_command(options, &member.name);
    for row in &mut rows {
        row.actions = actions_for(row, &inspect);
    }
    let summary = Summary::of(&rows);
    let gate = survivor_gate(&rows, options.no_run, options.fail_on_survivors);
    Ok(Report {
        schema_version: SCHEMA_VERSION,
        version: TOOL_VERSION,
        kind: KIND,
        language: LANGUAGE,
        package: member.name.clone(),
        package_version: member.version.clone(),
        total_issues: summary.survived,
        summary,
        gate_outcomes: BTreeMap::from([(RustGateName::SurvivingMutants, gate)]),
        in_diff: changed.map(|changed| DiffInfo {
            reference: changed.reference.clone(),
            base: changed.base.clone(),
            changed_files: changed.file_count(),
            mutants_outside: sites_outside + stages.rewrites_outside_diff,
        }),
        stopped_by: stages.stopped_by,
        elapsed_ms: millis(start.elapsed()),
        baseline: stages.baseline,
        schema: stages.schema,
        rebuild: stages.rebuild,
        gaps: Gaps {
            untested_dependents: if options.test_workspace {
                Vec::new()
            } else {
                dependents_of(workspace, member)
            },
            ..gaps_of(&collected, stages.doctests)
        },
        selection: stages.selection,
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

/// The command that inspects one mutant of this package, without the id.
fn inspect_command(options: &Options, package: &str) -> String {
    let manifest = options
        .manifest_path
        .as_ref()
        .map_or_else(String::new, |path| {
            format!(" --manifest-path {}", shell_word(&path.to_string_lossy()))
        });
    format!("cargo fallow mutants --package {package}{manifest} --inspect")
}

/// One word for a POSIX shell.
fn shell_word(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./=:".contains(&byte));
    if plain {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// Shows one mutant and the tests that execute it. No mutated code runs: the
/// walk only reads the source, and a new coverage map runs the plain tests in a copy
/// under the guard.
pub fn inspect(options: &Options, id: &str) -> Result<InspectReport, CliError> {
    let start = Instant::now();
    if id.len() != ID_DIGITS
        || !id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(CliError::new(
            ErrorCode::InvalidArgument,
            format!("`{id}` is not a mutant id: an id has {ID_DIGITS} hexadecimal digits"),
        )
        .with_help("The `id` field of a `cargo fallow mutants` report has the value."));
    }
    let workspace = workspace::load(options.manifest_path.as_deref())?;
    let (member, files, planned) = locate(&workspace, options.package.as_deref(), id)?;
    let dirs = WorkDirs::create(
        options.work_dir.as_deref(),
        options.keep_work_dir,
        workspace.root.as_std_path(),
    )
    .map_err(|err| failure("create the work folder", &err))?;
    let result = (|| {
        let guard = start_guard(options, &dirs)?;
        let target_dir = workspace.target_dir.as_std_path();
        prepare_copies(workspace.root.as_std_path(), target_dir, &dirs, &guard)
            .map_err(|err| failure("copy the source tree", &err))?;
        Engine {
            options,
            guard: &guard,
            dirs: &dirs,
            package_dir: package_folder(member),
            files: &files,
            start,
            target_dir: target_dir.to_path_buf(),
            package: member.name.clone(),
            changed: None,
            select: package_selection(options, &workspace, member),
            alone: RwLock::new(()),
        }
        .map_only()
    })();
    drop(dirs);
    let coverage = result?;
    let file = &files[planned.file];
    let map = coverage.map.unwrap_or_default();
    let (state, tests) = coverage_of(&map, file, &planned);
    let mut tests_without_data = map.unmapped;
    tests_without_data.sort_unstable();
    Ok(InspectReport {
        schema_version: SCHEMA_VERSION,
        version: TOOL_VERSION,
        kind: INSPECT_KIND,
        language: LANGUAGE,
        package: member.name.clone(),
        mutant: inspected(file, &planned),
        coverage: InspectCoverage {
            map: coverage.source,
            state,
            tests,
            tests_without_data,
        },
    })
}

/// Finds the member and the mutant of an id in the source tree.
fn locate<'a>(
    workspace: &'a Workspace,
    package: Option<&str>,
    id: &str,
) -> Result<(&'a Member, Vec<CrateFile>, Planned), CliError> {
    let members: Vec<&Member> = match package {
        Some(_) => vec![find_member(workspace, package)?],
        None => workspace.members.iter().collect(),
    };
    for member in members {
        let collected = collect(workspace.root.as_std_path(), &target_roots(member));
        let rewrites = rewrites_of(&collected.files);
        let found = schema_mutants(&collected.files)
            .into_iter()
            .chain(rebuild_mutants(&collected.files, &rewrites))
            .find(|planned| planned.row.id == id);
        if let Some(planned) = found {
            return Ok((member, collected.files, planned));
        }
    }
    Err(CliError::new(
        ErrorCode::InvalidArgument,
        format!("the source has no mutant `{id}`"),
    )
    .with_help(
        "An id changes when the text of its code changes. Run `cargo fallow mutants` again.",
    ))
}

fn coverage_of(
    map: &CoverageMap,
    file: &CrateFile,
    planned: &Planned,
) -> (CoverageState, Vec<String>) {
    if planned.constant {
        return (CoverageState::ConstantContext, Vec::new());
    }
    match map.tests_on(&file.path, lines_of(&file.original, &planned.range)) {
        LineTests::NoData => (CoverageState::NoLineData, Vec::new()),
        LineTests::Tests(found) => {
            let mut tests: Vec<String> = found
                .iter()
                .filter_map(|index| map.tests.get(*index as usize).cloned())
                .collect();
            tests.sort_unstable();
            let state = if tests.is_empty() {
                CoverageState::NotCovered
            } else {
                CoverageState::Covered
            };
            (state, tests)
        }
    }
}

fn inspected(file: &CrateFile, planned: &Planned) -> InspectedMutant {
    let row: &MutantRow = &planned.row;
    let text = &file.original;
    let start = text[..planned.range.start]
        .rfind('\n')
        .map_or(0, |at| at + 1);
    let end = text[planned.range.end..]
        .find('\n')
        .map_or(text.len(), |at| planned.range.end + at);
    let before = &text[start..end];
    let after = format!(
        "{}{}{}",
        &text[start..planned.range.start],
        planned.text,
        &text[planned.range.end..end]
    );
    InspectedMutant {
        id: row.id.clone(),
        tier: row.tier,
        operator: row.operator,
        file: row.file.clone(),
        line: row.line,
        column: row.column,
        replacement: row.replacement.clone(),
        before: before.to_owned(),
        after,
        under_cfg: row.under_cfg,
    }
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
    guard.set_run_folder(&dirs.root);
    if cfg!(unix) {
        let exe = std::env::current_exe().map_err(|err| failure("find this program", &err))?;
        guard
            .start_watchdog(
                &exe,
                dirs.root.join("watchdog-roots"),
                &dirs.root,
                dirs.removes_folder(),
            )
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

/// True when `--file` selects this path. An empty list selects each path.
pub fn file_selected(selected: &[String], path: &str) -> bool {
    selected.is_empty()
        || selected.iter().any(|value| {
            let value = value.trim_start_matches("./").trim_end_matches('/');
            path == value
                || path
                    .strip_prefix(value)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
}

/// Removes the sites of each file that `--file` leaves out. The file stays in the
/// list: a target root holds the helper of the schema. A value that matches no file
/// is an error, because a run with the wrong scope looks like a clean result.
fn retain_selected_files(options: &Options, files: &mut [CrateFile]) -> Result<(), CliError> {
    for value in &options.files {
        let one = std::slice::from_ref(value);
        if !files.iter().any(|file| file_selected(one, &file.path)) {
            return Err(CliError::new(
                ErrorCode::InvalidArgument,
                format!("`--file {value}` matches no source file of the package"),
            )
            .with_help("Pass a path from the workspace root, for example `src/lib.rs`."));
        }
    }
    for file in files {
        if !file_selected(&options.files, &file.path) {
            file.found.sites.clear();
        }
    }
    Ok(())
}

/// The flags that select the packages of each build and each doctest run.
///
/// cargo runs in the package folder, and that selects the package, with one
/// exception: in the workspace root cargo selects the default members, and those can
/// leave out the root package. `--package` there needs the version: the same name
/// can be a dependency.
fn package_selection(options: &Options, workspace: &Workspace, member: &Member) -> Vec<String> {
    if options.test_workspace {
        return vec!["--workspace".to_owned()];
    }
    if workspace.members.len() > 1 && package_folder(member).as_os_str().is_empty() {
        return vec![
            "--package".to_owned(),
            format!("{}@{}", member.name, member.version),
        ];
    }
    Vec::new()
}

/// The members that depend on the package, directly or through other members. Their
/// tests can see a mutant, and they do not run without `--test-workspace`.
fn dependents_of(workspace: &Workspace, member: &Member) -> Vec<String> {
    let mut found: BTreeSet<&str> = BTreeSet::new();
    let mut frontier = vec![member.name.as_str()];
    while let Some(name) = frontier.pop() {
        for other in &workspace.members {
            if other.name != member.name
                && other.dependencies.iter().any(|dep| dep == name)
                && found.insert(&other.name)
            {
                frontier.push(&other.name);
            }
        }
    }
    found.into_iter().map(str::to_owned).collect()
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
    fn a_shell_word_quotes_only_when_it_must() {
        assert_eq!(shell_word("a/b-c_d.toml"), "a/b-c_d.toml");
        assert_eq!(shell_word("a b"), "'a b'");
        assert_eq!(shell_word("it's"), r"'it'\''s'");
        assert_eq!(shell_word(""), "''");
    }

    #[test]
    fn jobs_default_to_a_safe_value() {
        assert_eq!(safe_jobs(1), 1);
        assert_eq!(safe_jobs(7), 1);
        assert_eq!(safe_jobs(8), 2);
        assert_eq!(safe_jobs(128), 2);
    }
}
