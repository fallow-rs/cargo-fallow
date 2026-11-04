//! The stages of one run.
//!
//! 1. Build the clean copy and run its baseline.
//! 2. Run the removal loop for the schema, then its baseline.
//! 3. Run the schema mutants through workers, with one switch value per process.
//! 4. Run the rebuild mutants one by one in the scratch copy.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, PoisonError, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{CliError, ErrorCode};
use crate::mutation::cache::{Cache, parts_hash, tree_hash};
use crate::mutation::coverage::{
    CoverageMap, INSTRUMENT, MapInput, Tools, build_map, find_tools, rustc_version,
};
use crate::mutation::diff::Changed;
use crate::mutation::files::CrateFile;
use crate::mutation::guard::{Guard, Scope};
use crate::mutation::id::id_text;
use crate::mutation::journal::Journal;
use crate::mutation::plan::{
    Planned, keep_rebuild_only, lines_of, rebuild_mutants, rewrites_of, schema_mutants,
};
use crate::mutation::process::{Exit, cargo, offline, run_to_files, stay_offline};
use crate::mutation::removal::{
    Build, CompilerError, LoopResult, Removal, parse_build, render_all, run_loop,
};
use crate::mutation::report::{
    Baseline, ErrorSummary, MapSource, MutantRow, RebuildInfo, RemovedMutant, SchemaInfo,
    SchemaStatus, SelectionInfo, StopReason, WorkspaceTests,
};
use crate::mutation::rewrites::{FileRewrites, Rewrite, apply, render_probe};
use crate::mutation::run::{Options, file_selected, millis};
use crate::mutation::schema::Mode;
use crate::mutation::schema_build::{SchemaWriter, read_by_build};
use crate::mutation::selection::{Picked, Selector, write_order};
use crate::mutation::status::{OVERLAP, Status, Tier, Verdict, merge_retry};
use crate::mutation::testrun::{
    BuildMeta, KnownFailures, RunSpec, Suite, SuiteStatus, TestName, TestRun, TestTree,
    build_targets, classify, compare, failures, known_failures, lost_passes, new_failure, prepare,
    run_doctests, run_nextest,
};
use crate::mutation::workdir::WorkDirs;
use crate::mutation::workers::run_workers;
use crate::render;

/// The time limit of one compile.
const BUILD_LIMIT: Duration = Duration::from_hours(1);
/// The time limit of one baseline suite.
const BASELINE_LIMIT: Duration = Duration::from_mins(30);
/// A mutant run can take this many times the baseline, because a machine under
/// load is slower, and a false timeout hides a survivor.
const TIMEOUT_FACTOR: u32 = 5;
/// The shortest limit of a mutant run.
const MIN_TIMEOUT: Duration = Duration::from_secs(20);
/// The time limit of one baseline suite of the whole workspace.
const WORKSPACE_BASELINE_LIMIT: Duration = Duration::from_hours(2);
/// A rebuild of a mutant builds less than the clean build, and this share of the
/// clean build leaves room for host load. A mutant in a `const fn` can loop in the
/// compiler for hours, because the lint cap turns off the lint that stops it.
const REBUILD_FACTOR: u32 = 3;
/// The shortest limit of one rebuild of a mutant.
const MIN_REBUILD_LIMIT: Duration = Duration::from_secs(30);
/// The longest wait for free memory before the run stops.
const MEMORY_WAIT: Duration = Duration::from_secs(120);
/// The time between two checks of the free memory in a wait.
const MEMORY_POLL: Duration = Duration::from_secs(2);

/// A retry of a timeout costs twice the limit. Host load can explain a run that
/// took this many times the baseline, and it cannot explain more.
const LOAD_FACTOR: u32 = 10;

/// True when a retry can give another result for a timeout at this limit.
fn retry_can_change_a_timeout(limit: Duration, baseline_ms: u64) -> bool {
    limit <= Duration::from_millis(baseline_ms) * LOAD_FACTOR && baseline_ms > 0
}

/// The limit of a mutant run: the override, or a share of the baseline.
fn mutant_limit(timeout: Option<Duration>, baseline_ms: u64) -> Duration {
    timeout
        .unwrap_or_else(|| (Duration::from_millis(baseline_ms) * TIMEOUT_FACTOR).max(MIN_TIMEOUT))
}

fn failure(action: &str, err: &io::Error) -> CliError {
    CliError::new(ErrorCode::MutationFailed, format!("cannot {action}: {err}"))
}

/// A guard kill with no mutant says that the limit is too low for this package,
/// or that the machine has too little free memory.
fn stop_on_kill(exit: &Exit) -> io::Result<()> {
    match exit {
        Exit::Killed(reason) => Err(io::Error::other(reason.to_string())),
        _ => Ok(()),
    }
}

/// The error of a baseline run, with a hint for a guard kill.
fn baseline_failure(action: &str, err: &io::Error) -> CliError {
    let error = failure(action, err);
    if err.to_string().contains("memory guard") {
        return error
            .with_help("Raise `--memory-limit` when the tests of this package need more memory.");
    }
    error
}

fn baseline_run(tree: TestTree<'_>, meta: &BuildMeta, label: &str) -> io::Result<TestRun> {
    baseline_with(
        tree,
        meta,
        &RunSpec {
            label: label.to_owned(),
            limit: BASELINE_LIMIT,
            ..RunSpec::new(1)
        },
    )
}

fn baseline_with(tree: TestTree<'_>, meta: &BuildMeta, spec: &RunSpec) -> io::Result<TestRun> {
    let (exit, nextest) = run_nextest(tree, meta, spec)?;
    stop_on_kill(&exit)?;
    let (exit, doc) = run_doctests(tree, spec)?;
    stop_on_kill(&exit)?;
    Ok(TestRun { nextest, doc })
}

/// The compile command. It runs in the package folder, because `-p name` is
/// ambiguous when a dependency has the same name. `select` holds the flags for the
/// cases where that folder does not select the right packages.
fn build_command(tree: TestTree<'_>, jobs: usize) -> Command {
    let mut command = cargo(tree.dir, tree.target_dir, jobs);
    command.args(["test", "--no-run", "--message-format=json"]);
    command.args(tree.cargo_flags);
    command.args(tree.select);
    if offline(tree.cargo_flags) {
        stay_offline(&mut command);
    }
    command
}

fn first_error(errors: &[CompilerError]) -> String {
    errors.first().map_or_else(
        || "the build failed".to_owned(),
        |error| error.message.clone(),
    )
}

fn summarize(errors: &[CompilerError]) -> Vec<ErrorSummary> {
    errors
        .iter()
        .map(|error| ErrorSummary {
            code: error.code.clone(),
            message: error.message.clone(),
        })
        .collect()
}

/// The result of all stages, without the mutant rows.
pub struct Stages {
    /// The baseline part of the report.
    pub baseline: Baseline,
    /// The schema part of the report.
    pub schema: SchemaInfo,
    /// The rebuild part of the report.
    pub rebuild: RebuildInfo,
    /// Why the run stopped before its last mutant.
    pub stopped_by: Option<StopReason>,
    /// True when the doctests can run.
    pub doctests: bool,
    /// The test selection part of the report.
    pub selection: SelectionInfo,
    /// The rebuild mutants that `--in-diff` left out.
    pub rewrites_outside_diff: usize,
    /// The run of the survivors against the tests of the workspace.
    pub workspace: Option<WorkspaceTests>,
}

impl Stages {
    /// The stages of a run with no mutant on a changed line. No build ran.
    fn no_changed_mutant(rewrites_outside_diff: usize) -> Self {
        Self {
            baseline: Baseline::default(),
            schema: SchemaInfo {
                probe_verified: true,
                ..SchemaInfo::default()
            },
            rebuild: RebuildInfo {
                probe_verified: true,
                ..RebuildInfo::default()
            },
            stopped_by: None,
            doctests: false,
            selection: SelectionInfo::default(),
            rewrites_outside_diff,
            workspace: None,
        }
    }
}

/// One mutant to run, with its row in the report.
#[derive(Clone)]
struct Job {
    row: usize,
    planned: Planned,
}

/// The test setup of one tier.
struct TierRun<'a> {
    tree: TestTree<'a>,
    meta: BuildMeta,
    nextest_exclude: BTreeSet<String>,
    nextest_known: KnownFailures,
    doc_known: KnownFailures,
    nextest_limit: Duration,
    nextest_baseline_ms: u64,
    doc_limit: Duration,
    doctests: bool,
    /// The tests that can run, fastest first, with their baseline time.
    tests: Vec<(String, u64)>,
    /// The nextest tool config with the order of `tests`.
    order: Option<PathBuf>,
    /// The limit of one rebuild of a mutant.
    rebuild_limit: Duration,
    /// False when a pass of each test gets no second run.
    retry_a_pass: bool,
    /// The cargo flags of each test binary, for a rebuild of only the binaries
    /// that a mutant needs. `None` builds each binary.
    targets: Option<BTreeMap<String, Vec<String>>>,
}

/// The tests that pass in a baseline and that stay in the run, fastest first.
fn ordered_tests(suite: &Suite, exclude: &BTreeSet<String>) -> Vec<(String, u64)> {
    let mut tests: Vec<(String, u64)> = suite
        .passes
        .iter()
        .filter(|&(name, &passes)| passes > 0 && !exclude.contains(name))
        .map(|(name, _)| (name.clone(), suite.times.get(name).copied().unwrap_or(0)))
        .collect();
    tests.sort_by(|a, b| (a.1, &a.0).cmp(&(b.1, &b.0)));
    tests
}

/// The inputs of the stages.
pub struct Engine<'a> {
    /// The options of the command.
    pub options: &'a Options,
    /// The memory guard.
    pub guard: &'a Guard,
    /// The work folder.
    pub dirs: &'a WorkDirs,
    /// The folder of the package, relative to the workspace root.
    pub package_dir: PathBuf,
    /// The source files of the package.
    pub files: &'a [CrateFile],
    /// The start of the command.
    pub start: Instant,
    /// The target folder of the workspace, which holds the cache folder.
    pub target_dir: PathBuf,
    /// The name of the package.
    pub package: String,
    /// The changed lines of `--in-diff`.
    pub changed: Option<&'a Changed>,
    /// The flags that select the packages of each build and each doctest run.
    pub select: Vec<String>,
    /// A normal test run holds this lock for reading. A second look at a kill holds
    /// it for writing, so no other test of this run can cause the failure.
    pub alone: RwLock<()>,
}

/// The result of the schema build.
#[derive(Default)]
struct SchemaStage {
    info: SchemaInfo,
    /// The kept mutants that no build reads.
    unread: BTreeSet<u64>,
    /// The mutants that the removal loop removed, with the compiler message.
    removed: Vec<(u64, String)>,
    /// The mutants of a schema that never compiled.
    never_compiled: bool,
}

impl Engine<'_> {
    fn tree<'t>(&'t self, package: &'t Path, target: &'t Path) -> TestTree<'t> {
        TestTree {
            dir: package,
            target_dir: target,
            logs: &self.dirs.logs,
            build_jobs: self.options.jobs,
            guard: self.guard,
            rustflags: "",
            select: &self.select,
            cargo_flags: &self.options.cargo_flags,
        }
    }

    /// Runs each stage. The rows have the schema tier first.
    pub fn run(&self) -> Result<(Vec<MutantRow>, Stages), CliError> {
        let (rewrites, rewrites_outside_diff) = self.find_rewrites();
        if self.is_narrowed() && !self.has_mutant(&rewrites) {
            return Ok((Vec::new(), Stages::no_changed_mutant(rewrites_outside_diff)));
        }
        let scratch_package = self.dirs.scratch.join(&self.package_dir);
        let schema_package = self.dirs.schema.join(&self.package_dir);
        let clean_tree = self.tree(&scratch_package, &self.dirs.target_scratch);
        let cached = self.open_cache(&scratch_package)?;
        let plan = self.selection_plan(&scratch_package, cached.as_ref())?;
        let build_start = Instant::now();
        let (clean_meta, clean) = self.clean_stage(clean_tree)?;
        let rebuild_limit = rebuild_limit(build_start.elapsed().saturating_sub(
            Duration::from_millis(clean.nextest.elapsed_ms + clean.doc.elapsed_ms),
        ));
        let coverage = self.coverage_stage(plan, &clean, &scratch_package, cached.as_ref())?;
        let selector = coverage.map.map(Selector::new);
        let fail_alone = fail_alone_tests(selector.as_ref());

        let schema_tree = self.tree(&schema_package, &self.dirs.target_schema);
        let (mut schema, rows_schema) = self.schema_part(&schema_package)?;
        let (rebuild_info, rebuild_unread, rows_rebuild) = self
            .rebuild_stage(&rewrites, &scratch_package)
            .map_err(|err| failure("probe the rebuild mutants", &err))?;

        let mut baseline = Baseline {
            clean: Some(clean.clone()),
            failures: failures(&clean).into_iter().collect(),
            ..Baseline::default()
        };
        let schema_setup = self
            .schema_setup(&schema, schema_tree, &clean, &fail_alone, &mut baseline)
            .map_err(|err| baseline_failure("run the schema baseline", &err))?;
        baseline.timeout_ms.rebuild_nextest = millis(self.limit_for(clean.nextest.elapsed_ms));
        baseline.timeout_ms.rebuild_doc = millis(self.limit_for(clean.doc.elapsed_ms));
        let doctests = self.options.doctests && clean.doc.status == SuiteStatus::Completed;
        let clean_setup = self
            .clean_setup(clean_tree, clean_meta, &clean, &fail_alone, rebuild_limit)
            .map_err(|err| failure("write the test order", &err))?;

        let mut rows: Vec<MutantRow> = Vec::new();
        let (schema_jobs, rebuild_jobs) = self.assign(
            rows_schema,
            &schema,
            rows_rebuild,
            &rebuild_unread,
            &mut rows,
        );
        let (schema_jobs, rebuild_jobs) = self.move_jobs_of_lost_tests(
            (schema_jobs, rebuild_jobs),
            baseline.schema.as_ref().map(|run| (&clean, run)),
            selector.as_ref(),
            &mut rows,
            &mut schema.info,
        );
        let has_test = clean.nextest.pass + clean.doc.pass > 0;
        let journal = cached.as_ref().and_then(|cached| self.open_journal(cached));
        let tiers = (schema_setup.as_ref(), &clean_setup);
        let state = RunState {
            selector: selector.as_ref(),
            journal: journal.as_ref(),
        };
        let all_jobs = (schema_jobs.clone(), rebuild_jobs.clone());
        let (mut stopped_by, resumed) = self.run_jobs(
            (schema_jobs, rebuild_jobs),
            tiers,
            state,
            has_test,
            &mut rows,
        )?;
        let phase = WorkspacePhase {
            schema: schema_tree,
            clean: clean_tree,
            schema_compiled: schema_setup.is_some(),
            rebuild_limit,
        };
        let workspace = self.end_of_run(
            &phase,
            all_jobs,
            &mut stopped_by,
            journal.as_ref(),
            &mut rows,
        )?;
        sort_rows(&mut rows);
        let selection = selection_info(
            coverage.source,
            coverage.map_ms,
            selector.as_ref(),
            resumed,
            &rows,
        );
        Ok((
            rows,
            Stages {
                baseline,
                schema: schema.info,
                rebuild: rebuild_info,
                stopped_by,
                doctests,
                selection,
                rewrites_outside_diff,
                workspace,
            },
        ))
    }

    /// Builds the schema of the mutants of the schema tier.
    fn schema_part(&self, package: &Path) -> Result<(SchemaStage, Vec<Planned>), CliError> {
        if !self.options.tier.has_schema() {
            return Ok((SchemaStage::default(), Vec::new()));
        }
        let planned = schema_mutants(self.files);
        if self.options.no_schema {
            return Ok((SchemaStage::default(), planned));
        }
        self.progress("compiling the schema");
        let stage = self
            .schema_stage(package, planned.len())
            .map_err(|err| failure("build the schema", &err))?;
        Ok((stage, planned))
    }

    /// Runs the jobs that the journal has no result for. Returns why the run
    /// stopped and the number of results from the journal.
    fn run_jobs(
        &self,
        (schema_jobs, rebuild_jobs): (Vec<Job>, Vec<Job>),
        tiers: (Option<&TierRun<'_>>, &TierRun<'_>),
        state: RunState<'_>,
        has_test: bool,
        rows: &mut [MutantRow],
    ) -> Result<(Option<StopReason>, usize), CliError> {
        if self.options.no_run {
            return Ok((None, 0));
        }
        if !has_test {
            // No test that passes: no run can see a difference.
            for job in schema_jobs.iter().chain(&rebuild_jobs) {
                let verdict =
                    Verdict::with_reason(Status::NoCoverage, "the package has no test that passes");
                apply_verdict(&mut rows[job.row], verdict);
            }
            return Ok((None, 0));
        }
        let mut resumed = 0;
        let (schema_jobs, rebuild_jobs) =
            restore_jobs(state.journal, schema_jobs, rebuild_jobs, rows, &mut resumed);
        let stopped_by = self.execute(schema_jobs, rebuild_jobs, tiers, state, rows)?;
        Ok((stopped_by, resumed))
    }

    /// Runs the stage of the workspace when the flag asks for it, and removes the
    /// journal when each mutant has a result.
    fn end_of_run(
        &self,
        phase: &WorkspacePhase<'_>,
        jobs: (Vec<Job>, Vec<Job>),
        stopped_by: &mut Option<StopReason>,
        journal: Option<&Journal>,
        rows: &mut [MutantRow],
    ) -> Result<Option<WorkspaceTests>, CliError> {
        let mut workspace = None;
        if self.options.test_workspace && !self.options.no_run && stopped_by.is_none() {
            let (info, stop) = self
                .workspace_stage(phase, jobs, rows)
                .map_err(|err| baseline_failure("run the tests of the workspace", &err))?;
            workspace = Some(info);
            *stopped_by = stop;
        }
        if let Some(journal) = journal
            && rows.iter().all(|row| row.status != Status::Pending)
        {
            journal.finish();
        }
        Ok(workspace)
    }

    /// Runs each mutant that the tests of the package do not detect against the tests
    /// of each workspace member.
    ///
    /// The tests of the package are fast and the coverage map holds them, so they run
    /// first. A workspace can hold many tests, and such a run needs one build of each
    /// test binary, so only the remaining mutants get this run. A schema build runs
    /// these mutants with no further compile.
    fn workspace_stage(
        &self,
        phase: &WorkspacePhase<'_>,
        (schema_jobs, rebuild_jobs): (Vec<Job>, Vec<Job>),
        rows: &mut [MutantRow],
    ) -> io::Result<(WorkspaceTests, Option<StopReason>)> {
        let start = Instant::now();
        let open = |job: &Job| {
            let row = &rows[job.row];
            matches!(row.status, Status::Survived)
                || (row.status == Status::NoCoverage && row.reason.as_deref() == Some(NO_TEST))
        };
        let schema_jobs: Vec<Job> = schema_jobs.into_iter().filter(&open).collect();
        let rebuild_jobs: Vec<Job> = rebuild_jobs.into_iter().filter(&open).collect();
        let mutants_run = schema_jobs.len() + rebuild_jobs.len();
        let mut info = WorkspaceTests {
            mutants_run,
            ..WorkspaceTests::default()
        };
        if mutants_run == 0 {
            return Ok((info, None));
        }
        self.progress("building the tests of the workspace");
        let select = ["--workspace".to_owned()];
        let clean_tree = TestTree {
            select: &select,
            ..phase.clean
        };
        let build_start = Instant::now();
        let clean_meta = prepare_or_error(clean_tree, "workspace-clean")?;
        let rebuild_limit = rebuild_limit(build_start.elapsed()).max(phase.rebuild_limit);
        let spec = RunSpec {
            label: "workspace-clean".to_owned(),
            limit: WORKSPACE_BASELINE_LIMIT,
            ..RunSpec::new(self.options.jobs)
        };
        self.progress("running the baseline of the workspace");
        let clean = baseline_with(clean_tree, &clean_meta, &spec)?;
        info.failures = failures(&clean).into_iter().collect();
        let no_tests = BTreeSet::new();
        let mut clean_setup =
            self.clean_setup(clean_tree, clean_meta, &clean, &no_tests, rebuild_limit)?;
        clean_setup.order = None;
        clean_setup.retry_a_pass = false;
        let schema_tree = TestTree {
            select: &select,
            ..phase.schema
        };
        let schema_part = if phase.schema_compiled && !schema_jobs.is_empty() {
            let meta = prepare_or_error(schema_tree, "workspace-schema")?;
            let spec = RunSpec {
                label: "workspace-schema".to_owned(),
                ..spec
            };
            let run = baseline_with(schema_tree, &meta, &spec)?;
            let mut setup = self.schema_tier(schema_tree, meta, &clean, &run, &no_tests)?;
            setup.order = None;
            setup.retry_a_pass = false;
            Some((setup, run))
        } else {
            None
        };
        let mut unused = SchemaInfo::default();
        let (schema_jobs, rebuild_jobs) = self.move_jobs_of_lost_tests(
            (schema_jobs, rebuild_jobs),
            schema_part.as_ref().map(|(_, run)| (&clean, run)),
            None,
            rows,
            &mut unused,
        );
        let before: Vec<u64> = rows.iter().map(|row| row.tests_run).collect();
        let state = RunState {
            selector: None,
            journal: None,
        };
        let mut stopped = match &schema_part {
            Some((setup, _)) => self.run_schema_jobs(&schema_jobs, setup, state, rows)?,
            None => None,
        };
        if stopped.is_none() {
            stopped = self.run_rebuild_jobs(&rebuild_jobs, &clean_setup, state, rows)?;
        }
        for job in schema_jobs.iter().chain(&rebuild_jobs) {
            rows[job.row].tests_run += before[job.row];
            if matches!(rows[job.row].status, Status::Killed | Status::Timeout) {
                info.detected += 1;
            }
        }
        info.elapsed_ms = millis(start.elapsed());
        Ok((info, stopped))
    }

    fn clean_stage(&self, tree: TestTree<'_>) -> Result<(BuildMeta, TestRun), CliError> {
        self.progress("building the clean copy");
        let meta = prepare(tree, "clean")
            .map_err(|err| failure("run a build", &err))?
            .map_err(|text| {
                CliError::new(
                    ErrorCode::MutationFailed,
                    format!("the package does not build:\n{}", tail(&text)),
                )
            })?;
        self.progress("running the clean baseline");
        let clean = baseline_run(tree, &meta, "clean")
            .map_err(|err| baseline_failure("run the baseline", &err))?;
        self.check_baseline(&clean)?;
        Ok((meta, clean))
    }

    fn clean_setup<'t>(
        &self,
        tree: TestTree<'t>,
        meta: BuildMeta,
        clean: &TestRun,
        fail_alone: &BTreeSet<String>,
        rebuild_limit: Duration,
    ) -> io::Result<TierRun<'t>> {
        let mut nextest_exclude: BTreeSet<String> = clean.nextest.failed.iter().cloned().collect();
        nextest_exclude.extend(fail_alone.iter().cloned());
        let tests = ordered_tests(&clean.nextest, &nextest_exclude);
        let order = self.write_order("clean", &tests)?;
        // A build of the workspace selects many packages, and a target flag of one
        // package does not select the same binary there.
        let one_package = !tree.select.iter().any(|flag| flag == "--workspace");
        let targets = if one_package {
            meta.target_flags().ok()
        } else {
            None
        };
        Ok(TierRun {
            tree,
            meta,
            targets,
            tests,
            order,
            nextest_known: known_failures(nextest_exclude.iter()),
            nextest_exclude,
            doc_known: known_failures(clean.doc.failed.iter()),
            rebuild_limit,
            retry_a_pass: true,
            nextest_limit: self.limit_for(clean.nextest.elapsed_ms),
            nextest_baseline_ms: clean.nextest.elapsed_ms,
            doc_limit: self.limit_for(clean.doc.elapsed_ms),
            doctests: self.options.doctests
                && clean.doc.status == SuiteStatus::Completed
                && clean.doc.pass > 0,
        })
    }

    /// Writes the order of the tests of a tier. A run with no selection keeps the
    /// plain order of nextest.
    fn write_order(&self, label: &str, tests: &[(String, u64)]) -> io::Result<Option<PathBuf>> {
        if !self.options.selection {
            return Ok(None);
        }
        let path = self.dirs.logs.join(format!("{label}-order.toml"));
        write_order(&path, tests)?;
        Ok(Some(path))
    }

    /// The cache folder and the hash of the source, or `None` with `--no-cache`.
    pub(super) fn open_cache(&self, package: &Path) -> Result<Option<CachedTree>, CliError> {
        if !self.options.cache || self.options.no_run {
            return Ok(None);
        }
        let hash = tree_hash(&self.dirs.scratch).map_err(|err| failure("hash the source", &err))?;
        let version = rustc_version(package);
        let flags = std::env::var("RUSTFLAGS").unwrap_or_default();
        let encoded_flags = std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
        Ok(Some(CachedTree {
            cache: Cache::new(&self.target_dir, &self.package),
            key: parts_hash(&[
                env!("CARGO_PKG_VERSION"),
                &hash.to_string(),
                &version,
                &flags,
                &encoded_flags,
                &self.dirs.parent_configs,
                &self.select.join(" "),
                &self.package,
            ]),
        }))
    }

    /// Decides how the run gets its map. This check runs before the first build,
    /// so missing tools stop the run at once.
    fn selection_plan(
        &self,
        package: &Path,
        cached: Option<&CachedTree>,
    ) -> Result<SelectionPlan, CliError> {
        if !self.options.selection || self.options.no_run {
            return Ok(SelectionPlan::Off);
        }
        if let Some(found) = cached.and_then(|cached| cached.cache.read_map(cached.key)) {
            return Ok(SelectionPlan::Cached(found));
        }
        let tools = find_tools(package).map_err(|message| {
            CliError::new(
                ErrorCode::MutationFailed,
                format!("test selection needs the LLVM tools: {message}"),
            )
            .with_help(
                "Run `rustup component add llvm-tools`, or pass `--no-selection` to run each \
                 test for each mutant.",
            )
        })?;
        Ok(SelectionPlan::Build(tools))
    }

    /// Reads or builds the map.
    fn coverage_stage(
        &self,
        plan: SelectionPlan,
        clean: &TestRun,
        package: &Path,
        cached: Option<&CachedTree>,
    ) -> Result<Coverage, CliError> {
        let tools = match plan {
            SelectionPlan::Off => return Ok(Coverage::default()),
            SelectionPlan::Cached(map) => {
                return Ok(Coverage {
                    map: Some(map),
                    source: MapSource::Cached,
                    map_ms: 0,
                });
            }
            SelectionPlan::Build(tools) => tools,
        };
        let start = Instant::now();
        self.progress("building the coverage map");
        let map = self
            .build_coverage(&tools, clean, package, cached)
            .map_err(|err| {
                failure("build the coverage map", &err)
                    .with_help("Pass `--no-selection` to run each test for each mutant.")
            })?;
        // The build is only an input of the map. A failed removal only leaves
        // files in a work folder.
        let _ = fs::remove_dir_all(&self.dirs.target_coverage);
        Ok(Coverage {
            map: Some(map),
            source: MapSource::Built,
            map_ms: millis(start.elapsed()),
        })
    }

    fn build_coverage(
        &self,
        tools: &Tools,
        clean: &TestRun,
        package: &Path,
        cached: Option<&CachedTree>,
    ) -> io::Result<CoverageMap> {
        let tree = TestTree {
            rustflags: INSTRUMENT,
            ..self.tree(package, &self.dirs.target_coverage)
        };
        let meta = prepare(tree, "coverage")?.map_err(|text| {
            io::Error::other(format!(
                "the coverage build does not compile:\n{}",
                tail(&text)
            ))
        })?;
        let tests = ordered_tests(&clean.nextest, &BTreeSet::new());
        // A run on a part of the package needs line data only for that part. Such a
        // map stays out of the cache.
        let narrowed = self.is_narrowed();
        let files: BTreeSet<String> = self
            .files
            .iter()
            .map(|file| file.path.clone())
            .filter(|path| !narrowed || self.in_narrow_scope(path))
            .collect();
        let built = build_map(&MapInput {
            tree,
            meta: &meta,
            tools,
            tests: &tests,
            files: &files,
            root: &self.dirs.scratch,
            jobs: self.options.jobs,
        })?;
        if built.map.tests.is_empty() && !tests.is_empty() {
            render::warn("no test wrote usable coverage data, so each mutant runs each test");
        }
        if built.complete
            && !narrowed
            && let Some(cached) = cached
        {
            cached.cache.write_map(cached.key, &built.map);
        }
        Ok(built.map)
    }

    /// Opens the journal of this source and these options. A journal that does not
    /// open only costs time after a stop, so the run continues without it.
    fn open_journal(&self, cached: &CachedTree) -> Option<Journal> {
        let options = self.options;
        let setup = format!(
            "{}|{}|{:?}|{}|{}|{}|{}|{:?}|{}|{}",
            cached.key,
            options.test_workspace,
            options.tier,
            options.no_schema,
            options.doctests,
            options.retry,
            options.selection,
            options.timeout,
            options.memory_limit_mib,
            options.low_memory_percent,
        );
        let path = cached.cache.journal_path(parts_hash(&[&setup]));
        match Journal::open(&path) {
            Ok(journal) => Some(journal),
            Err(err) => {
                render::warn(&format!("cannot open the journal: {err}"));
                None
            }
        }
    }

    /// Runs the schema jobs and then the rebuild jobs.
    fn execute(
        &self,
        schema_jobs: Vec<Job>,
        rebuild_jobs: Vec<Job>,
        (schema_setup, clean_setup): (Option<&TierRun<'_>>, &TierRun<'_>),
        state: RunState<'_>,
        rows: &mut [MutantRow],
    ) -> Result<Option<StopReason>, CliError> {
        let (schema_jobs, rebuild_jobs) = self.apply_cap(schema_jobs, rebuild_jobs);
        if let Some(setup) = schema_setup {
            let stopped = self
                .run_schema_jobs(&schema_jobs, setup, state, rows)
                .map_err(|err| failure("run a schema mutant", &err))?;
            if stopped.is_some() {
                return Ok(stopped);
            }
        }
        self.run_rebuild_jobs(&rebuild_jobs, clean_setup, state, rows)
            .map_err(|err| failure("run a rebuild mutant", &err))
    }

    fn progress(&self, message: &str) {
        render::Progress::new(self.options.quiet).step(message);
    }

    /// The rebuild mutants of each file, and the number that `--in-diff` left out.
    pub(super) fn find_rewrites(&self) -> (Vec<FileRewrites>, usize) {
        if !self.options.tier.has_rebuild() {
            let empty = self.files.iter().map(|_| FileRewrites::default()).collect();
            return (empty, 0);
        }
        let mut outside = 0;
        let mut all = rewrites_of(self.files);
        if self.options.schema_takes_rebuild_operators() {
            keep_rebuild_only(&mut all);
        }
        for (file, found) in self.files.iter().zip(&mut all) {
            if !file_selected(&self.options.files, &file.path) {
                found.rewrites.clear();
            }
        }
        if let Some(changed) = self.changed {
            for (file, found) in self.files.iter().zip(&mut all) {
                let before = found.rewrites.len();
                found.rewrites.retain(|rewrite| {
                    changed.touches(&file.path, lines_of(&file.original, &rewrite.range))
                });
                outside += before - found.rewrites.len();
            }
        }
        (all, outside)
    }

    /// True when a tier of this run has a mutant.
    fn has_mutant(&self, rewrites: &[FileRewrites]) -> bool {
        rewrites.iter().any(|file| !file.rewrites.is_empty())
            || (self.options.tier.has_schema()
                && self.files.iter().any(|file| !file.found.sites.is_empty()))
    }

    /// True when the flags of a narrow run keep a mutant in this file.
    fn in_narrow_scope(&self, path: &str) -> bool {
        file_selected(&self.options.files, path)
            && self.changed.is_none_or(|changed| changed.has_file(path))
    }

    /// True when a flag leaves out a part of the package.
    fn is_narrowed(&self) -> bool {
        self.changed.is_some() || !self.options.files.is_empty()
    }

    /// The coverage map for `--inspect`: from the cache, or from a new build. This
    /// runs the tests with no mutant.
    pub fn map_only(&self) -> Result<Coverage, CliError> {
        let package = self.dirs.scratch.join(&self.package_dir);
        let cached = self.open_cache(&package)?;
        let plan = self.selection_plan(&package, cached.as_ref())?;
        if let SelectionPlan::Cached(map) = plan {
            return Ok(Coverage {
                map: Some(map),
                source: MapSource::Cached,
                map_ms: 0,
            });
        }
        let tree = self.tree(&package, &self.dirs.target_scratch);
        let (_, clean) = self.clean_stage(tree)?;
        self.coverage_stage(plan, &clean, &package, cached.as_ref())
    }

    /// A limit for a mutant run, from a baseline of the same tree.
    fn limit_for(&self, baseline_ms: u64) -> Duration {
        mutant_limit(self.options.timeout, baseline_ms)
    }

    /// Stops the run when the baseline says nothing about a mutant. A run with
    /// `--no-run` only reports the baseline.
    fn check_baseline(&self, clean: &TestRun) -> Result<(), CliError> {
        if self.options.no_run {
            return Ok(());
        }
        if clean.nextest.status != SuiteStatus::Completed {
            return Err(CliError::new(
                ErrorCode::MutationFailed,
                "the baseline test run did not complete",
            ));
        }
        let failed = clean.nextest.fail + clean.doc.fail;
        if failed > 0 && clean.nextest.pass + clean.doc.pass == 0 {
            return Err(CliError::new(
                ErrorCode::MutationFailed,
                "each test fails with no mutant, so the run has no test result",
            )
            .with_help("Fix the tests, then run the command again."));
        }
        Ok(())
    }

    fn schema_stage(&self, package: &Path, candidates: usize) -> io::Result<SchemaStage> {
        let source_bytes = self.files.iter().map(|file| file.original.len()).sum();
        let mut info = SchemaInfo {
            status: SchemaStatus::NoSites,
            source_bytes,
            schema_bytes: source_bytes,
            candidates,
            kept: candidates,
            probe_verified: true,
            ..SchemaInfo::default()
        };
        if candidates == 0 {
            return Ok(SchemaStage {
                info,
                ..SchemaStage::default()
            });
        }
        let mut writer = SchemaWriter::new(self.files, &self.dirs.schema);
        let result = self.removal_loop(&mut writer, package, &mut info)?;
        info.round_log.clone_from(&result.log);
        info.last_errors = summarize(&result.last_errors);
        info.kept = result.kept.len();
        info.kept_under_cfg = self.count_under_cfg(&result.kept);
        info.removed = result
            .removed
            .iter()
            .map(|removal| self.removed_row(removal))
            .collect();
        let removed = result
            .removed
            .iter()
            .map(|removal| (removal.id, removal.message.clone()))
            .collect();
        let Some(rounds) = result.rounds else {
            info.status = SchemaStatus::NeverCompiled;
            info.rounds = -1;
            return Ok(SchemaStage {
                info,
                removed,
                never_compiled: true,
                ..SchemaStage::default()
            });
        };
        info.status = SchemaStatus::Compiled;
        info.rounds = i64::from(rounds);
        let probed = read_by_build(&result.kept, |left| {
            writer.write(&render_all(self.files, left, Mode::Probe))?;
            self.compile(package, &self.dirs.target_schema, "probe")
        })?;
        writer.write(&render_all(self.files, &result.kept, Mode::Schema))?;
        info.kept_compile_checked = probed.read.len();
        info.probe_verified = probed.verified;
        let unread: BTreeSet<u64> = if probed.verified {
            result.kept.difference(&probed.read).copied().collect()
        } else {
            BTreeSet::new()
        };
        Ok(SchemaStage {
            info,
            unread,
            removed,
            never_compiled: false,
        })
    }

    fn count_under_cfg(&self, kept: &BTreeSet<u64>) -> usize {
        self.files
            .iter()
            .flat_map(|file| {
                file.found
                    .sites
                    .iter()
                    .filter(|site| file.under_cfg || site.under_cfg)
            })
            .flat_map(|site| &site.mutants)
            .filter(|mutant| kept.contains(&mutant.id))
            .count()
    }

    fn removed_row(&self, removal: &Removal) -> RemovedMutant {
        let found = self.files.iter().find_map(|file| {
            file.found.sites.iter().find_map(|site| {
                site.mutants
                    .iter()
                    .find(|mutant| mutant.id == removal.id)
                    .map(|mutant| (file, site, mutant))
            })
        });
        RemovedMutant {
            id: id_text(removal.id),
            file: found
                .map(|(file, _, _)| file.path.clone())
                .unwrap_or_default(),
            line: found.map_or(0, |(file, site, _)| {
                file.original[..site.report_range().start]
                    .matches('\n')
                    .count()
                    + 1
            }),
            replacement: found
                .map(|(_, _, mutant)| {
                    mutant
                        .text
                        .clone()
                        .unwrap_or_else(|| mutant.replacement.to_owned())
                })
                .unwrap_or_default(),
            round: removal.round,
            how: removal.how,
            code: removal.code.clone(),
            message: removal.message.clone(),
        }
    }

    fn removal_loop(
        &self,
        writer: &mut SchemaWriter<'_>,
        package: &Path,
        info: &mut SchemaInfo,
    ) -> io::Result<LoopResult> {
        let mut round = 0_u32;
        let mut failed = None;
        let result = run_loop(self.files, |rendered| {
            round += 1;
            let compiled = writer.write(rendered).and_then(|bytes| {
                info.schema_bytes = bytes;
                self.compile(
                    package,
                    &self.dirs.target_schema,
                    &format!("round-{round:02}"),
                )
            });
            compiled.unwrap_or_else(|err| {
                failed = Some(err);
                Build::default()
            })
        });
        failed.map_or(Ok(result), Err)
    }

    fn compile(&self, package: &Path, target: &Path, label: &str) -> io::Result<Build> {
        self.compile_within(self.tree(package, target), label, BUILD_LIMIT, &[])
    }

    /// Builds `tree`. `targets` selects the binaries, and an empty list builds each
    /// binary.
    fn compile_within(
        &self,
        tree: TestTree<'_>,
        label: &str,
        limit: Duration,
        targets: &[String],
    ) -> io::Result<Build> {
        let out = self.dirs.logs.join(format!("{label}.json"));
        let err = self.dirs.logs.join(format!("{label}.err"));
        let mut command = build_command(tree, self.options.jobs);
        command.args(targets);
        let exit = run_to_files(command, &out, &err, limit, self.guard, Scope::Build)?;
        if let Exit::Killed(reason) = &exit {
            return Err(io::Error::other(reason.to_string()));
        }
        let mut build = parse_build(&fs::read_to_string(out)?);
        build.success = build.success && exit == Exit::Code(Some(0));
        if exit == Exit::TimedOut {
            build.errors.insert(
                0,
                CompilerError {
                    message: format!("the compile passed its time limit of {} s", limit.as_secs()),
                    ..CompilerError::default()
                },
            );
        }
        Ok(build)
    }

    /// Prepares the schema tier: the build, the baseline and the time limits.
    /// The limit comes from the schema build, because a switch adds time.
    fn schema_setup<'t>(
        &self,
        schema: &SchemaStage,
        tree: TestTree<'t>,
        clean: &TestRun,
        fail_alone: &BTreeSet<String>,
        baseline: &mut Baseline,
    ) -> io::Result<Option<TierRun<'t>>> {
        if schema.info.status != SchemaStatus::Compiled {
            return Ok(None);
        }
        self.progress("running the schema baseline");
        let meta = prepare(tree, "schema")?.map_err(|text| {
            io::Error::other(format!("the schema does not build:\n{}", tail(&text)))
        })?;
        let run = baseline_run(tree, &meta, "schema")?;
        if run.nextest.status != SuiteStatus::Completed && !self.options.no_run {
            return Err(io::Error::other(
                "the baseline test run of the schema build did not complete",
            ));
        }
        let comparison = compare(clean, &run);
        baseline.regressions = comparison.regressions;
        baseline.tests_pass_with_schema = comparison.tests_pass_with_schema;
        baseline.timeout_ms.schema_nextest = millis(self.limit_for(run.nextest.elapsed_ms));
        baseline.timeout_ms.schema_doc = millis(self.limit_for(run.doc.elapsed_ms));
        let setup = self.schema_tier(tree, meta, clean, &run, fail_alone)?;
        baseline.schema = Some(run);
        Ok(Some(setup))
    }

    /// The test setup of a schema build, from its baseline.
    fn schema_tier<'t>(
        &self,
        tree: TestTree<'t>,
        meta: BuildMeta,
        clean: &TestRun,
        run: &TestRun,
        fail_alone: &BTreeSet<String>,
    ) -> io::Result<TierRun<'t>> {
        let mut nextest_exclude: BTreeSet<String> = clean.nextest.failed.iter().cloned().collect();
        nextest_exclude.extend(run.nextest.failed.iter().cloned());
        nextest_exclude.extend(lost_passes(&clean.nextest, &run.nextest));
        nextest_exclude.extend(fail_alone.iter().cloned());
        let mut doc_known = known_failures(clean.doc.failed.iter());
        for (name, count) in known_failures(run.doc.failed.iter()) {
            let known = doc_known.entry(name).or_default();
            *known = count.max(*known);
        }
        let tests = ordered_tests(&run.nextest, &nextest_exclude);
        let order = self.write_order("schema", &tests)?;
        Ok(TierRun {
            tree,
            meta,
            tests,
            order,
            nextest_known: known_failures(nextest_exclude.iter()),
            nextest_exclude,
            doc_known,
            rebuild_limit: BUILD_LIMIT,
            retry_a_pass: true,
            targets: None,
            nextest_limit: self.limit_for(run.nextest.elapsed_ms),
            nextest_baseline_ms: run.nextest.elapsed_ms,
            doc_limit: self.limit_for(run.doc.elapsed_ms),
            doctests: self.options.doctests
                && run.doc.status == SuiteStatus::Completed
                && run.doc.pass > 0,
        })
    }

    fn rebuild_stage(
        &self,
        rewrites: &[FileRewrites],
        package: &Path,
    ) -> io::Result<(RebuildInfo, BTreeSet<u64>, Vec<Planned>)> {
        let planned = rebuild_mutants(self.files, rewrites);
        let mut info = RebuildInfo {
            candidates: planned.len(),
            unknown_return_types: rewrites.iter().map(|file| file.unknown_return_types).sum(),
            probe_verified: true,
        };
        if planned.is_empty() {
            return Ok((info, BTreeSet::new(), planned));
        }
        self.progress("probing the rebuild mutants");
        let all: BTreeSet<u64> = planned.iter().map(|mutant| mutant.id).collect();
        let probed = read_by_build(&all, |left| {
            self.write_rebuild_probe(rewrites, left)?;
            self.compile(package, &self.dirs.target_scratch, "rebuild-probe")
        });
        self.restore_scratch()?;
        let probed = probed?;
        info.probe_verified = probed.verified;
        let unread = if probed.verified {
            all.difference(&probed.read).copied().collect()
        } else {
            BTreeSet::new()
        };
        Ok((info, unread, planned))
    }

    fn write_rebuild_probe(
        &self,
        rewrites: &[FileRewrites],
        left: &BTreeSet<u64>,
    ) -> io::Result<()> {
        for (file, found) in self.files.iter().zip(rewrites) {
            let enabled: Vec<&Rewrite> = found
                .rewrites
                .iter()
                .filter(|rewrite| left.contains(&rewrite.id))
                .collect();
            fs::write(
                self.dirs.scratch.join(&file.path),
                render_probe(&file.original, &enabled),
            )?;
        }
        Ok(())
    }

    fn restore_scratch(&self) -> io::Result<()> {
        for file in self.files {
            fs::write(self.dirs.scratch.join(&file.path), &file.original)?;
        }
        Ok(())
    }

    /// Gives each mutant its row and its first status, and returns the jobs.
    fn assign(
        &self,
        schema_planned: Vec<Planned>,
        schema: &SchemaStage,
        rebuild_planned: Vec<Planned>,
        rebuild_unread: &BTreeSet<u64>,
        rows: &mut Vec<MutantRow>,
    ) -> (Vec<Job>, Vec<Job>) {
        let unread_text = "no build reads this code with the default features";
        let (mut schema_jobs, mut rebuild_jobs) = (Vec::new(), Vec::new());
        for mut planned in schema_planned {
            let row = rows.len();
            let message = schema
                .removed
                .iter()
                .find(|(id, _)| *id == planned.id)
                .map(|(_, message)| message.clone());
            let broke_schema = message.is_some() || schema.never_compiled;
            if self.options.no_schema {
                planned.row.tier = Tier::Rebuild;
                rows.push(planned.row.clone());
                rebuild_jobs.push(Job { row, planned });
            } else if broke_schema {
                planned.row.removed_from_schema = true;
                if self.options.tier.has_rebuild() {
                    planned.row.tier = Tier::Rebuild;
                    rows.push(planned.row.clone());
                    rebuild_jobs.push(Job { row, planned });
                } else {
                    let reason = message.unwrap_or_else(|| "the schema never compiled".to_owned());
                    apply_verdict(
                        &mut planned.row,
                        Verdict::with_reason(Status::CompileError, reason),
                    );
                    rows.push(planned.row);
                }
            } else if schema.unread.contains(&planned.id) {
                apply_verdict(
                    &mut planned.row,
                    Verdict::with_reason(Status::NoCoverage, unread_text),
                );
                rows.push(planned.row);
            } else {
                rows.push(planned.row.clone());
                schema_jobs.push(Job { row, planned });
            }
        }
        for mut planned in rebuild_planned {
            let row = rows.len();
            if rebuild_unread.contains(&planned.id) {
                apply_verdict(
                    &mut planned.row,
                    Verdict::with_reason(Status::NoCoverage, unread_text),
                );
                rows.push(planned.row);
            } else {
                rows.push(planned.row.clone());
                rebuild_jobs.push(Job { row, planned });
            }
        }
        (schema_jobs, rebuild_jobs)
    }

    /// Moves a schema job to the rebuild tier when a test that the schema build broke
    /// can see the mutant. That test does not run in the schema tier, so the switch
    /// would hide the mutant from it. With no line data each job can be such a job, and
    /// a doctest has no line data.
    fn move_jobs_of_lost_tests(
        &self,
        (schema_jobs, mut rebuild_jobs): (Vec<Job>, Vec<Job>),
        runs: Option<(&TestRun, &TestRun)>,
        selector: Option<&Selector>,
        rows: &mut [MutantRow],
        info: &mut SchemaInfo,
    ) -> (Vec<Job>, Vec<Job>) {
        let Some((clean, schema)) = runs else {
            return (schema_jobs, rebuild_jobs);
        };
        let lost: Vec<(String, u64)> = lost_passes(&clean.nextest, &schema.nextest)
            .into_iter()
            .map(|name| (name, 0))
            .collect();
        let lost_doc = !lost_passes(&clean.doc, &schema.doc).is_empty();
        if lost.is_empty() && !lost_doc {
            return (schema_jobs, rebuild_jobs);
        }
        let mut kept = Vec::new();
        for mut job in schema_jobs {
            let sees_it = lost_doc
                || selector.is_none_or(|selector| {
                    let file = &self.files[job.planned.file];
                    let lines = lines_of(&file.original, &job.planned.range);
                    !selector.pick(&lost, &file.path, lines).empty
                });
            if sees_it {
                job.planned.row.tier = Tier::Rebuild;
                rows[job.row].tier = Tier::Rebuild;
                rebuild_jobs.push(job);
                info.moved_to_rebuild += 1;
            } else {
                kept.push(job);
            }
        }
        (kept, rebuild_jobs)
    }

    /// Keeps the first `--max-mutants` jobs, schema jobs first. The others stay
    /// `pending`.
    fn apply_cap(&self, mut schema: Vec<Job>, mut rebuild: Vec<Job>) -> (Vec<Job>, Vec<Job>) {
        if let Some(cap) = self.options.max_mutants {
            schema.truncate(cap);
            rebuild.truncate(cap - schema.len());
        }
        (schema, rebuild)
    }

    /// Returns a reason when no new mutant can start. The check waits for memory.
    fn stop_reason(&self) -> Option<StopReason> {
        if let Some(budget) = self.options.time_budget
            && self.start.elapsed() >= budget
        {
            return Some(StopReason::TimeBudget);
        }
        let wait = Instant::now();
        while !self.guard.memory_is_free() {
            if wait.elapsed() >= MEMORY_WAIT {
                return Some(StopReason::LowMemory);
            }
            thread::sleep(MEMORY_POLL);
        }
        None
    }

    /// One worker for each job: at most `jobs` test processes run at a time.
    fn run_schema_jobs(
        &self,
        jobs: &[Job],
        setup: &TierRun<'_>,
        state: RunState<'_>,
        rows: &mut [MutantRow],
    ) -> io::Result<Option<StopReason>> {
        let start_rows: &[MutantRow] = rows;
        let stop: Mutex<Option<StopReason>> = Mutex::new(None);
        let results = run_workers(jobs.len(), self.options.jobs, |index| {
            let job = &jobs[index];
            if let Some(reason) = self.stop_reason() {
                *stop.lock().unwrap_or_else(PoisonError::into_inner) = Some(reason);
                return Ok(None);
            }
            let picked = self.pick(state, setup, job);
            let label = id_text(job.planned.id);
            self.evaluate(setup, state, Some(job.planned.id), &label, &picked)
                .map(|done| Some(self.finished_row(&start_rows[job.row], done, &picked, state)))
        })?;
        for (index, row) in results {
            rows[jobs[index].row] = row;
        }
        Ok(stop.into_inner().unwrap_or_else(PoisonError::into_inner))
    }

    /// Writes each mutant into the one scratch copy, builds it, runs the tests and
    /// restores the file. The restore also happens after an error.
    fn run_rebuild_jobs(
        &self,
        jobs: &[Job],
        setup: &TierRun<'_>,
        state: RunState<'_>,
        rows: &mut [MutantRow],
    ) -> io::Result<Option<StopReason>> {
        for job in jobs {
            if let Some(reason) = self.stop_reason() {
                return Ok(Some(reason));
            }
            let picked = self.pick(state, setup, job);
            let done = if picked.empty && !setup.doctests {
                Done::no_test()
            } else {
                let file = &self.files[job.planned.file];
                let path = self.dirs.scratch.join(&file.path);
                let text = apply(&file.original, &job.planned.range, &job.planned.text);
                let done = fs::write(&path, text)
                    .and_then(|()| self.rebuild_and_test(job, setup, state, &picked));
                fs::write(&path, &file.original)?;
                done?
            };
            rows[job.row] = self.finished_row(&rows[job.row], done, &picked, state);
        }
        Ok(None)
    }

    /// The tests of one mutant. With no map, the mutant runs each test.
    fn pick(&self, state: RunState<'_>, setup: &TierRun<'_>, job: &Job) -> Picked {
        let Some(selector) = state.selector else {
            return Picked {
                only: None,
                suites: None,
                empty: false,
                baseline_ms: setup.nextest_baseline_ms,
                no_line_data: false,
            };
        };
        if job.planned.constant {
            return Picked {
                only: None,
                suites: None,
                empty: false,
                baseline_ms: setup.nextest_baseline_ms,
                no_line_data: true,
            };
        }
        let file = &self.files[job.planned.file];
        let lines = lines_of(&file.original, &job.planned.range);
        let mut picked = selector.pick(&setup.tests, &file.path, lines);
        if picked.only.is_none() {
            // The run has each test, so its limit comes from the whole suite.
            picked.baseline_ms = setup.nextest_baseline_ms;
        }
        picked
    }

    fn rebuild_and_test(
        &self,
        job: &Job,
        setup: &TierRun<'_>,
        state: RunState<'_>,
        picked: &Picked,
    ) -> io::Result<Done> {
        let start = Instant::now();
        // The tests of a mutant run only from the binaries that this build makes. A
        // doctest run builds the library itself.
        let targets = build_targets(setup.targets.as_ref(), picked.suites.as_ref());
        let build = self.compile_within(setup.tree, "rebuild", setup.rebuild_limit, &targets);
        let build = match build {
            Ok(build) => build,
            // The guard killed the compiler, and a compile says nothing about the
            // mutant.
            Err(err) => {
                return Ok(Done {
                    elapsed: start.elapsed(),
                    ..Done::of(Verdict::with_reason(Status::RuntimeError, err.to_string()))
                });
            }
        };
        if !build.success {
            return Ok(Done {
                elapsed: start.elapsed(),
                ..Done::of(Verdict::with_reason(
                    Status::CompileError,
                    first_error(&build.errors),
                ))
            });
        }
        let mut done = self.evaluate(setup, state, None, &id_text(job.planned.id), picked)?;
        done.elapsed = start.elapsed();
        Ok(done)
    }

    /// One mutant: the first run, and one retry when the result asks for it.
    fn evaluate(
        &self,
        setup: &TierRun<'_>,
        state: RunState<'_>,
        mutant: Option<u64>,
        label: &str,
        picked: &Picked,
    ) -> io::Result<Done> {
        if picked.empty && !setup.doctests {
            return Ok(Done::no_test());
        }
        let start = Instant::now();
        let limit = self.nextest_limit(setup, picked);
        let run = MutantRun {
            setup,
            mutant,
            label,
            picked,
        };
        let first = self.shared_pass(run, 1)?;
        let mut done = Done {
            elapsed: start.elapsed(),
            tests_run: first.tests_run,
            doctests_run: first.doctests_run,
            ..Done::of(first.verdict.clone())
        };
        if self.options.retry && done.verdict.status == Status::Killed {
            self.confirm_kill(run, state, &first, &mut done)?;
            done.elapsed = start.elapsed();
        }
        let retry_useful = if done.verdict.status == Status::Timeout {
            retry_can_change_a_timeout(limit, picked.baseline_ms)
        } else {
            // A second run of only the doctests says nothing about the lines.
            !picked.empty && setup.retry_a_pass
        };
        if self.options.retry && !done.retried && done.verdict.wants_retry() && retry_useful {
            // Host load often lasts into the retry, so a timeout waits twice as long.
            let factor = if done.verdict.status == Status::Timeout {
                2
            } else {
                1
            };
            let retry = self.shared_pass(run, factor)?;
            let mut second = Done {
                tests_run: retry.tests_run,
                doctests_run: retry.doctests_run,
                ..Done::of(retry.verdict.clone())
            };
            // A failure in the retry can also come from another test process, so it
            // gets the same second look as a kill in the first run.
            let retry_failed = second.verdict.status == Status::Killed;
            if retry_failed {
                self.confirm_kill(run, state, &retry, &mut second)?;
            }
            done.verdict = merge_retry(done.verdict, second.verdict, retry_failed);
            done.retried = true;
            done.tests_run += second.tests_run;
            done.doctests_run += second.doctests_run;
            done.elapsed = start.elapsed();
        }
        if picked.empty && done.verdict.status == Status::Survived {
            done.verdict = Verdict::with_reason(Status::NoCoverage, NO_TEST_BUT_DOCTESTS);
        }
        Ok(done)
    }

    /// Takes a second look at a kill, with no other test process of this run.
    ///
    /// A kill can come from another test at the same time, from the fastest-first
    /// order, or from a flaky test. The cheap case first: the failing test alone. The
    /// map says that this test passes alone with no mutant, so its failure comes from
    /// the mutant. Else the selected tests run again, in the plain order of the
    /// project and one at a time.
    fn confirm_kill(
        &self,
        run: MutantRun<'_>,
        state: RunState<'_>,
        first: &Pass,
        done: &mut Done,
    ) -> io::Result<()> {
        let setup = run.setup;
        let _alone = self.alone.write().unwrap_or_else(PoisonError::into_inner);
        done.retried = true;
        let filter = first
            .killer
            .as_deref()
            .filter(|killer| state.selector.is_some_and(|map| map.passes_alone(killer)))
            .and_then(TestName::parse)
            .map(|name| name.filter());
        if let Some(only) = filter {
            let spec = self.second_look_spec(run, Some(only));
            let (exit, suite) = run_nextest(setup.tree, &setup.meta, &spec)?;
            done.tests_run += u64::from(suite.tests_run());
            let verdict = classify("nextest", &exit, &suite, &setup.nextest_known);
            if verdict.as_ref().is_some_and(|v| v.status == Status::Killed) {
                return Ok(());
            }
        }
        let again = self.one_pass(run, 1, true)?;
        done.tests_run += again.tests_run;
        done.doctests_run += again.doctests_run;
        done.verdict = match again.verdict.status {
            Status::Survived if self.options.jobs > 1 => {
                Verdict::with_reason(Status::Survived, OVERLAP)
            }
            Status::Survived => Verdict {
                status: Status::Flaky,
                reason: Some("the first run failed a test, and the second run passed".to_owned()),
                killed_by: first.verdict.killed_by.clone(),
            },
            _ => again.verdict,
        };
        Ok(())
    }

    /// The run of a second look: one process at a time, in the plain order.
    fn second_look_spec(&self, run: MutantRun<'_>, only: Option<String>) -> RunSpec {
        let setup = run.setup;
        RunSpec {
            label: format!("mutant-{}", run.label),
            mutant: run.mutant,
            limit: self.nextest_limit(setup, run.picked),
            fail_fast: true,
            jobs: 1,
            exclude: setup.nextest_exclude.clone(),
            only,
            profile_file: None,
            order: None,
            remove_logs: true,
        }
    }

    /// The limit of a nextest run: from the selected tests with a map.
    fn nextest_limit(&self, setup: &TierRun<'_>, picked: &Picked) -> Duration {
        if picked.only.is_some() {
            self.limit_for(picked.baseline_ms)
        } else {
            setup.nextest_limit
        }
    }

    /// One run next to other test runs of this run.
    fn shared_pass(&self, run: MutantRun<'_>, factor: u32) -> io::Result<Pass> {
        let _shared = self.alone.read().unwrap_or_else(PoisonError::into_inner);
        self.one_pass(run, factor, false)
    }

    fn one_pass(&self, run: MutantRun<'_>, factor: u32, one_at_a_time: bool) -> io::Result<Pass> {
        let MutantRun { setup, picked, .. } = run;
        let mut spec = if one_at_a_time {
            self.second_look_spec(run, picked.only.clone())
        } else {
            RunSpec {
                jobs: if run.mutant.is_some() {
                    1
                } else {
                    self.options.jobs
                },
                order: setup.order.clone(),
                ..self.second_look_spec(run, picked.only.clone())
            }
        };
        spec.limit = self.nextest_limit(setup, picked) * factor;
        let mut pass = Pass {
            verdict: Verdict::new(Status::Survived),
            tests_run: 0,
            doctests_run: 0,
            killer: None,
        };
        if !picked.empty {
            let (exit, suite) = run_nextest(setup.tree, &setup.meta, &spec)?;
            pass.tests_run = u64::from(suite.tests_run());
            if let Some(verdict) = classify("nextest", &exit, &suite, &setup.nextest_known) {
                pass.killer = new_failure(&suite, &setup.nextest_known).map(str::to_owned);
                pass.verdict = verdict;
                return Ok(pass);
            }
        }
        if !setup.doctests {
            return Ok(pass);
        }
        spec.limit = setup.doc_limit * factor;
        let (exit, suite) = run_doctests(setup.tree, &spec)?;
        pass.doctests_run = u64::from(suite.pass + suite.fail);
        if let Some(verdict) = classify("cargo test --doc", &exit, &suite, &setup.doc_known) {
            pass.verdict = verdict;
        }
        Ok(pass)
    }

    /// The row of a finished mutant. The journal gets the row at once.
    fn finished_row(
        &self,
        row: &MutantRow,
        done: Done,
        picked: &Picked,
        state: RunState<'_>,
    ) -> MutantRow {
        let mut row = row.clone();
        row.retried = done.retried;
        row.duration_ms = millis(done.elapsed);
        row.tests_run = done.tests_run;
        row.doctests_run = done.doctests_run;
        row.no_line_data = picked.no_line_data;
        apply_verdict(&mut row, done.verdict);
        if let Some(journal) = state.journal {
            journal.record(&row);
        }
        if !self.options.quiet {
            let line = format!(
                "{} {}:{} {}",
                row.status.name(),
                row.file,
                row.line,
                row.operator
            );
            self.progress(&line);
        }
        row
    }
}

/// The reason of a mutant with a verdict from no run.
const NO_TEST: &str = "no test executes this line";
/// The reason of a mutant where only the doctests ran. The doctests have no line
/// data, so a pass does not show that they execute the mutant.
const NO_TEST_BUT_DOCTESTS: &str =
    "no nextest test executes this line, and the doctests pass with the mutant";

/// The trees of the run against the tests of the workspace.
struct WorkspacePhase<'a> {
    schema: TestTree<'a>,
    clean: TestTree<'a>,
    schema_compiled: bool,
    rebuild_limit: Duration,
}

/// Builds the test binaries of a tree. A build error stops the stage.
fn prepare_or_error(tree: TestTree<'_>, label: &str) -> io::Result<BuildMeta> {
    prepare(tree, label)?.map_err(|text| {
        io::Error::other(format!(
            "the tests of the workspace do not build:\n{}",
            tail(&text)
        ))
    })
}

/// The source of the map, the cache and the journal of one run.
#[derive(Clone, Copy)]
struct RunState<'a> {
    selector: Option<&'a Selector>,
    journal: Option<&'a Journal>,
}

/// The cache folder with the key of this source.
pub(super) struct CachedTree {
    pub(super) cache: Cache,
    pub(super) key: u64,
}

/// How a run gets its map.
enum SelectionPlan {
    Off,
    Cached(CoverageMap),
    Build(Tools),
}

/// The map of a run.
#[derive(Default)]
pub struct Coverage {
    /// The map, or `None` with no selection.
    pub map: Option<CoverageMap>,
    /// Where the map comes from.
    pub source: MapSource,
    /// The time to build the map, in milliseconds.
    pub map_ms: u64,
}

/// Gives each job with a result in the journal its row, and returns the others.
fn restore_jobs(
    journal: Option<&Journal>,
    schema: Vec<Job>,
    rebuild: Vec<Job>,
    rows: &mut [MutantRow],
    resumed: &mut usize,
) -> (Vec<Job>, Vec<Job>) {
    let Some(journal) = journal else {
        return (schema, rebuild);
    };
    let mut keep = |jobs: Vec<Job>| -> Vec<Job> {
        jobs.into_iter()
            .filter(|job| {
                let done = journal.restore(&mut rows[job.row]);
                *resumed += usize::from(done);
                !done
            })
            .collect()
    };
    (keep(schema), keep(rebuild))
}

pub(super) fn selection_info(
    source: MapSource,
    map_ms: u64,
    selector: Option<&Selector>,
    resumed: usize,
    rows: &[MutantRow],
) -> SelectionInfo {
    SelectionInfo {
        enabled: selector.is_some(),
        map: source,
        map_ms,
        tests_mapped: selector.map_or(0, |selector| selector.map().tests.len()),
        tests_unmapped: selector.map_or_else(Vec::new, |selector| selector.map().unmapped.clone()),
        tests_fail_alone: selector
            .map_or_else(Vec::new, |selector| selector.map().fail_alone.clone()),
        mutants_without_line_data: rows.iter().filter(|row| row.no_line_data).count(),
        resumed,
        tests_run: rows.iter().map(|row| row.tests_run).sum(),
        doctests_run: rows.iter().map(|row| row.doctests_run).sum(),
    }
}

/// One mutant in one tier.
#[derive(Clone, Copy)]
struct MutantRun<'a> {
    setup: &'a TierRun<'a>,
    mutant: Option<u64>,
    label: &'a str,
    picked: &'a Picked,
}

/// The result of one run of one mutant.
struct Pass {
    verdict: Verdict,
    tests_run: u64,
    doctests_run: u64,
    /// The full name of the nextest test that failed.
    killer: Option<String>,
}

/// The result of the runs of one mutant.
struct Done {
    verdict: Verdict,
    retried: bool,
    elapsed: Duration,
    tests_run: u64,
    doctests_run: u64,
}

impl Done {
    fn of(verdict: Verdict) -> Self {
        Self {
            verdict,
            retried: false,
            elapsed: Duration::ZERO,
            tests_run: 0,
            doctests_run: 0,
        }
    }

    /// The result of a mutant that no test executes, with no run.
    fn no_test() -> Self {
        Self::of(Verdict::with_reason(Status::NoCoverage, NO_TEST))
    }
}

fn apply_verdict(row: &mut MutantRow, verdict: Verdict) {
    row.status = verdict.status;
    row.reason = verdict.reason;
    row.killed_by = verdict.killed_by;
}

/// The last lines of a long error text.
fn tail(text: &str) -> String {
    const LINES: usize = 40;
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(LINES)..].join("\n")
}

/// The schema tier first, then by position.
pub(super) fn sort_rows(rows: &mut [MutantRow]) {
    rows.sort_by(|a, b| {
        (a.tier == Tier::Rebuild, &a.file, a.line, a.column).cmp(&(
            b.tier == Tier::Rebuild,
            &b.file,
            b.line,
            b.column,
        ))
    });
}

/// The tests that the map found to fail alone.
fn fail_alone_tests(selector: Option<&Selector>) -> BTreeSet<String> {
    selector.map_or_else(BTreeSet::new, |selector| {
        selector.map().fail_alone.iter().cloned().collect()
    })
}

/// The limit of one rebuild of a mutant, from the time of the clean build.
fn rebuild_limit(clean_build: Duration) -> Duration {
    (clean_build * REBUILD_FACTOR).clamp(MIN_REBUILD_LIMIT, BUILD_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rebuild_limit_comes_from_the_clean_build() {
        assert_eq!(rebuild_limit(Duration::from_secs(1)), MIN_REBUILD_LIMIT);
        assert_eq!(
            rebuild_limit(Duration::from_mins(5)),
            Duration::from_mins(15)
        );
        assert_eq!(rebuild_limit(Duration::from_hours(9)), BUILD_LIMIT);
    }

    #[test]
    fn a_limit_comes_from_the_baseline_with_a_floor_and_an_override() {
        assert_eq!(mutant_limit(None, 100), MIN_TIMEOUT);
        assert_eq!(mutant_limit(None, 10_000), Duration::from_secs(50));
        let fixed = Some(Duration::from_secs(3));
        assert_eq!(mutant_limit(fixed, 10_000), Duration::from_secs(3));
    }

    #[test]
    fn a_timeout_far_above_the_baseline_gets_no_retry() {
        let limit = Duration::from_secs(20);
        assert!(!retry_can_change_a_timeout(limit, 100));
        assert!(!retry_can_change_a_timeout(limit, 1999));
        assert!(retry_can_change_a_timeout(limit, 2000));
        assert!(retry_can_change_a_timeout(limit, 15_000));
        assert!(!retry_can_change_a_timeout(Duration::ZERO, 0));
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        let text: Vec<String> = (0..50).map(|n| n.to_string()).collect();
        let tail = tail(&text.join("\n"));
        assert_eq!(tail.lines().count(), 40);
        assert!(tail.ends_with("49"));
    }
}
