//! The stages of one run.
//!
//! 1. Build the clean copy and run its baseline.
//! 2. Run the removal loop for the schema, then its baseline.
//! 3. Run the schema mutants through workers, with one switch value per process.
//! 4. Run the rebuild mutants one by one in the scratch copy.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{CliError, ErrorCode};
use crate::mutation::cache::{Cache, parts_hash, tree_hash};
use crate::mutation::coverage::{
    CoverageMap, INSTRUMENT, MapInput, Tools, build_map, find_tools, rustc_version,
};
use crate::mutation::files::CrateFile;
use crate::mutation::guard::{Guard, Scope};
use crate::mutation::id::id_text;
use crate::mutation::journal::Journal;
use crate::mutation::plan::{Planned, lines_of, rebuild_mutants, schema_mutants};
use crate::mutation::process::{Exit, cargo, run_to_files};
use crate::mutation::removal::{
    Build, CompilerError, LoopResult, Removal, parse_build, probed_mutants, render_all, run_loop,
};
use crate::mutation::report::{
    Baseline, ErrorSummary, MapSource, MutantRow, RebuildInfo, RemovedMutant, SchemaInfo,
    SchemaStatus, SelectionInfo, StopReason,
};
use crate::mutation::rewrites::{FileRewrites, Rewrite, apply, find_rewrites, render_probe};
use crate::mutation::run::{Options, millis};
use crate::mutation::schema::{Mode, Rendered};
use crate::mutation::selection::{Picked, Selector, write_order};
use crate::mutation::status::{Status, Tier, Verdict, merge_retry};
use crate::mutation::testrun::{
    BuildMeta, RunSpec, Suite, SuiteStatus, TestRun, TestTree, classify, compare, failures,
    lost_passes, prepare, run_doctests, run_nextest,
};
use crate::mutation::workdir::WorkDirs;
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
        return error.with_help(
            "Raise `--memory-limit-mib` when the tests of this package need more memory.",
        );
    }
    error
}

fn baseline_run(tree: TestTree<'_>, meta: &BuildMeta, label: &str) -> io::Result<TestRun> {
    let spec = RunSpec {
        label: label.to_owned(),
        limit: BASELINE_LIMIT,
        ..RunSpec::new(1)
    };
    let (exit, nextest) = run_nextest(tree, meta, &spec)?;
    stop_on_kill(&exit)?;
    let (exit, doc) = run_doctests(tree, &spec)?;
    stop_on_kill(&exit)?;
    Ok(TestRun { nextest, doc })
}

/// The compile command. It runs in the package folder, because `-p name` is
/// ambiguous when a dependency has the same name.
fn build_command(dir: &Path, target_dir: &Path, jobs: usize) -> Command {
    let mut command = cargo(dir, target_dir, jobs);
    command.args(["test", "--no-run", "--message-format=json"]);
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
}

/// One mutant to run, with its row in the report.
struct Job {
    row: usize,
    planned: Planned,
}

/// The test setup of one tier.
struct TierRun<'a> {
    tree: TestTree<'a>,
    meta: BuildMeta,
    nextest_exclude: BTreeSet<String>,
    doc_known: BTreeSet<String>,
    nextest_limit: Duration,
    nextest_baseline_ms: u64,
    doc_limit: Duration,
    doctests: bool,
    /// The tests that can run, fastest first, with their baseline time.
    tests: Vec<(String, u64)>,
    /// The nextest tool config with the order of `tests`.
    order: Option<PathBuf>,
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
        }
    }

    /// Runs each stage. The rows have the schema tier first.
    pub fn run(&self) -> Result<(Vec<MutantRow>, Stages), CliError> {
        let scratch_package = self.dirs.scratch.join(&self.package_dir);
        let schema_package = self.dirs.schema.join(&self.package_dir);
        let clean_tree = self.tree(&scratch_package, &self.dirs.target_scratch);
        let cached = self.open_cache(&scratch_package)?;
        let plan = self.selection_plan(&scratch_package, cached.as_ref())?;
        let (clean_meta, clean) = self.clean_stage(clean_tree)?;
        let coverage = self.coverage_stage(plan, &clean, &scratch_package, cached.as_ref())?;
        let selector = coverage.map.map(Selector::new);

        let schema_tree = self.tree(&schema_package, &self.dirs.target_schema);
        let mut schema = SchemaStage::default();
        let mut rows_schema: Vec<Planned> = Vec::new();
        if self.options.tier.has_schema() {
            rows_schema = schema_mutants(self.files);
            if !self.options.no_schema {
                self.progress("compiling the schema");
                schema = self
                    .schema_stage(&schema_package, rows_schema.len())
                    .map_err(|err| failure("build the schema", &err))?;
            }
        }
        let rewrites = self.find_rewrites();
        let (rebuild_info, rebuild_unread, rows_rebuild) = self
            .rebuild_stage(&rewrites, &scratch_package)
            .map_err(|err| failure("probe the rebuild mutants", &err))?;

        let mut baseline = Baseline {
            clean: Some(clean.clone()),
            failures: failures(&clean).into_iter().collect(),
            ..Baseline::default()
        };
        let schema_setup = self
            .schema_setup(&schema, schema_tree, &clean, &mut baseline)
            .map_err(|err| baseline_failure("run the schema baseline", &err))?;
        baseline.timeout_ms.rebuild_nextest = millis(self.limit_for(clean.nextest.elapsed_ms));
        baseline.timeout_ms.rebuild_doc = millis(self.limit_for(clean.doc.elapsed_ms));
        let doctests = self.options.doctests && clean.doc.status == SuiteStatus::Completed;
        let clean_setup = self
            .clean_setup(clean_tree, clean_meta, &clean)
            .map_err(|err| failure("write the test order", &err))?;

        let mut rows: Vec<MutantRow> = Vec::new();
        let (schema_jobs, rebuild_jobs) = self.assign(
            rows_schema,
            &schema,
            rows_rebuild,
            &rebuild_unread,
            &mut rows,
        );
        let has_test = clean.nextest.pass + clean.doc.pass > 0;
        let journal = cached.as_ref().and_then(|cached| self.open_journal(cached));
        let tiers = (schema_setup.as_ref(), &clean_setup);
        let state = RunState {
            selector: selector.as_ref(),
            journal: journal.as_ref(),
        };
        let (stopped_by, resumed) = self.run_jobs(
            (schema_jobs, rebuild_jobs),
            tiers,
            state,
            has_test,
            &mut rows,
        )?;
        rows.sort_by(|a, b| {
            (a.tier == Tier::Rebuild, &a.file, a.line, a.column).cmp(&(
                b.tier == Tier::Rebuild,
                &b.file,
                b.line,
                b.column,
            ))
        });
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
            },
        ))
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
        if let Some(journal) = state.journal
            && rows.iter().all(|row| row.status != Status::Pending)
        {
            journal.finish();
        }
        Ok((stopped_by, resumed))
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
    ) -> io::Result<TierRun<'t>> {
        let nextest_exclude: BTreeSet<String> = clean.nextest.failed.iter().cloned().collect();
        let tests = ordered_tests(&clean.nextest, &nextest_exclude);
        let order = self.write_order("clean", &tests)?;
        Ok(TierRun {
            tree,
            meta,
            tests,
            order,
            nextest_exclude,
            doc_known: clean.doc.failed.iter().cloned().collect(),
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
    fn open_cache(&self, package: &Path) -> Result<Option<CachedTree>, CliError> {
        if !self.options.cache || self.options.no_run {
            return Ok(None);
        }
        let hash = tree_hash(&self.dirs.scratch).map_err(|err| failure("hash the source", &err))?;
        let version = rustc_version(package);
        let flags = std::env::var("RUSTFLAGS").unwrap_or_default();
        Ok(Some(CachedTree {
            cache: Cache::new(&self.target_dir, &self.package),
            key: parts_hash(&[
                env!("CARGO_PKG_VERSION"),
                &hash.to_string(),
                &version,
                &flags,
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
        let files = self.files.iter().map(|file| file.path.clone()).collect();
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
            "{}|{:?}|{}|{}|{}|{}|{:?}|{}|{}",
            cached.key,
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

    fn find_rewrites(&self) -> Vec<FileRewrites> {
        if !self.options.tier.has_rebuild() {
            return self.files.iter().map(|_| FileRewrites::default()).collect();
        }
        self.files
            .iter()
            .map(|file| {
                // The walk of the sites parsed this text, so an error here only
                // leaves the file without rebuild mutants.
                find_rewrites(&file.original, file.edition, &file.path).unwrap_or_default()
            })
            .collect()
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
                file.original[..site.range.start].matches('\n').count() + 1
            }),
            replacement: found
                .map(|(_, _, mutant)| mutant.replacement.to_owned())
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
        let out = self.dirs.logs.join(format!("{label}.json"));
        let err = self.dirs.logs.join(format!("{label}.err"));
        let command = build_command(package, target, self.options.jobs);
        let exit = run_to_files(command, &out, &err, BUILD_LIMIT, self.guard, Scope::Build)?;
        if let Exit::Killed(reason) = &exit {
            return Err(io::Error::other(reason.to_string()));
        }
        let mut build = parse_build(&fs::read_to_string(out)?);
        build.success = build.success && exit == Exit::Code(Some(0));
        Ok(build)
    }

    /// Prepares the schema tier: the build, the baseline and the time limits.
    /// The limit comes from the schema build, because a switch adds time.
    fn schema_setup<'t>(
        &self,
        schema: &SchemaStage,
        tree: TestTree<'t>,
        clean: &TestRun,
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
        let mut nextest_exclude: BTreeSet<String> = clean.nextest.failed.iter().cloned().collect();
        nextest_exclude.extend(run.nextest.failed.iter().cloned());
        nextest_exclude.extend(lost_passes(&clean.nextest, &run.nextest));
        let mut doc_known: BTreeSet<String> = clean.doc.failed.iter().cloned().collect();
        doc_known.extend(run.doc.failed.iter().cloned());
        doc_known.extend(lost_passes(&clean.doc, &run.doc));
        let tests = ordered_tests(&run.nextest, &nextest_exclude);
        let order = self.write_order("schema", &tests)?;
        let setup = TierRun {
            tree,
            meta,
            tests,
            order,
            nextest_exclude,
            doc_known,
            nextest_limit: self.limit_for(run.nextest.elapsed_ms),
            nextest_baseline_ms: run.nextest.elapsed_ms,
            doc_limit: self.limit_for(run.doc.elapsed_ms),
            doctests: self.options.doctests
                && run.doc.status == SuiteStatus::Completed
                && run.doc.pass > 0,
        };
        baseline.schema = Some(run);
        Ok(Some(setup))
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
        let next = AtomicUsize::new(0);
        let results: Mutex<Vec<(usize, io::Result<MutantRow>)>> = Mutex::new(Vec::new());
        let start_rows: &[MutantRow] = rows;
        let stop: Mutex<Option<StopReason>> = Mutex::new(None);
        thread::scope(|scope| {
            for _ in 0..self.options.jobs {
                scope.spawn(|| {
                    loop {
                        let index = next.fetch_add(1, Ordering::SeqCst);
                        let Some(job) = jobs.get(index) else {
                            return;
                        };
                        if let Some(reason) = self.stop_reason() {
                            *stop.lock().unwrap_or_else(PoisonError::into_inner) = Some(reason);
                            next.store(jobs.len(), Ordering::SeqCst);
                            return;
                        }
                        let picked = self.pick(state, setup, job);
                        let label = id_text(job.planned.id);
                        let row = self
                            .evaluate(setup, Some(job.planned.id), &label, &picked)
                            .map(|done| {
                                self.finished_row(&start_rows[job.row], done, &picked, state)
                            });
                        results
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .push((index, row));
                    }
                });
            }
        });
        let results = results.into_inner().unwrap_or_else(PoisonError::into_inner);
        for (index, row) in results {
            rows[jobs[index].row] = row?;
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
                    .and_then(|()| self.rebuild_and_test(job, setup, &picked));
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
                empty: false,
                baseline_ms: setup.nextest_baseline_ms,
                no_line_data: false,
            };
        };
        if job.planned.constant {
            return Picked {
                only: None,
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
        picked: &Picked,
    ) -> io::Result<Done> {
        let start = Instant::now();
        let build = self.compile(setup.tree.dir, setup.tree.target_dir, "rebuild");
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
        let mut done = self.evaluate(setup, None, &id_text(job.planned.id), picked)?;
        done.elapsed = start.elapsed();
        Ok(done)
    }

    /// One mutant: the first run, and one retry when the result asks for it.
    fn evaluate(
        &self,
        setup: &TierRun<'_>,
        mutant: Option<u64>,
        label: &str,
        picked: &Picked,
    ) -> io::Result<Done> {
        if picked.empty && !setup.doctests {
            return Ok(Done::no_test());
        }
        let start = Instant::now();
        let limit = self.nextest_limit(setup, picked);
        let first = self.one_pass(setup, mutant, label, 1, picked)?;
        let mut done = Done {
            elapsed: start.elapsed(),
            tests_run: first.tests_run,
            doctests_run: first.doctests_run,
            ..Done::of(first.verdict)
        };
        let retry_useful = if done.verdict.status == Status::Timeout {
            retry_can_change_a_timeout(limit, picked.baseline_ms)
        } else {
            // A second run of only the doctests says nothing about the lines.
            !picked.empty
        };
        if self.options.retry && done.verdict.wants_retry() && retry_useful {
            // Host load often lasts into the retry, so a timeout waits twice as long.
            let factor = if done.verdict.status == Status::Timeout {
                2
            } else {
                1
            };
            let retry = self.one_pass(setup, mutant, label, factor, picked)?;
            done.verdict = merge_retry(done.verdict, retry.verdict);
            done.retried = true;
            done.tests_run += retry.tests_run;
            done.doctests_run += retry.doctests_run;
            done.elapsed = start.elapsed();
        }
        if picked.empty && done.verdict.status == Status::Survived {
            done.verdict = Verdict::with_reason(Status::NoCoverage, NO_TEST_BUT_DOCTESTS);
        }
        Ok(done)
    }

    /// The limit of a nextest run: from the selected tests with a map.
    fn nextest_limit(&self, setup: &TierRun<'_>, picked: &Picked) -> Duration {
        if picked.only.is_some() {
            self.limit_for(picked.baseline_ms)
        } else {
            setup.nextest_limit
        }
    }

    fn one_pass(
        &self,
        setup: &TierRun<'_>,
        mutant: Option<u64>,
        label: &str,
        factor: u32,
        picked: &Picked,
    ) -> io::Result<Pass> {
        let mut spec = RunSpec {
            label: format!("mutant-{label}"),
            mutant,
            limit: self.nextest_limit(setup, picked) * factor,
            fail_fast: true,
            jobs: if mutant.is_some() {
                1
            } else {
                self.options.jobs
            },
            exclude: setup.nextest_exclude.clone(),
            only: picked.only.clone(),
            profile_file: None,
            order: setup.order.clone(),
            remove_logs: true,
        };
        let mut pass = Pass {
            verdict: Verdict::new(Status::Survived),
            tests_run: 0,
            doctests_run: 0,
        };
        if !picked.empty {
            let (exit, suite) = run_nextest(setup.tree, &setup.meta, &spec)?;
            pass.tests_run = u64::from(suite.pass + suite.fail);
            if let Some(verdict) = classify("nextest", &exit, &suite, &setup.nextest_exclude) {
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

/// The source of the map, the cache and the journal of one run.
#[derive(Clone, Copy)]
struct RunState<'a> {
    selector: Option<&'a Selector>,
    journal: Option<&'a Journal>,
}

/// The cache folder with the key of this source.
struct CachedTree {
    cache: Cache,
    key: u64,
}

/// How a run gets its map.
enum SelectionPlan {
    Off,
    Cached(CoverageMap),
    Build(Tools),
}

/// The map of a run.
#[derive(Default)]
struct Coverage {
    map: Option<CoverageMap>,
    source: MapSource,
    map_ms: u64,
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

fn selection_info(
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
        mutants_without_line_data: rows.iter().filter(|row| row.no_line_data).count(),
        resumed,
        tests_run: rows.iter().map(|row| row.tests_run).sum(),
        doctests_run: rows.iter().map(|row| row.doctests_run).sum(),
    }
}

/// The result of one run of one mutant.
struct Pass {
    verdict: Verdict,
    tests_run: u64,
    doctests_run: u64,
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

/// The result of the probe builds.
struct Probed {
    /// The mutants that a build reads.
    read: BTreeSet<u64>,
    /// False when a build had another error, so the probe proves nothing.
    verified: bool,
}

/// Finds the mutants that a build reads.
///
/// cargo stops at the library when the library has an error, so a mutant in a
/// binary target needs a second pass with no probe left in the library. The loop
/// ends when a pass finds no new mutant.
fn read_by_build<B>(all: &BTreeSet<u64>, mut compile: B) -> io::Result<Probed>
where
    B: FnMut(&BTreeSet<u64>) -> io::Result<Build>,
{
    let mut read = BTreeSet::new();
    let mut left = all.clone();
    while !left.is_empty() {
        let build = compile(&left)?;
        let found: BTreeSet<u64> = probed_mutants(&build)
            .intersection(&left)
            .copied()
            .collect();
        if found.is_empty() {
            return Ok(Probed {
                read,
                verified: build.success,
            });
        }
        left.retain(|id| !found.contains(id));
        read.extend(found);
    }
    Ok(Probed {
        read,
        verified: true,
    })
}

/// Writes the generated files that changed since the last round.
struct SchemaWriter<'a> {
    files: &'a [CrateFile],
    dir: &'a Path,
    written: Vec<String>,
}

impl<'a> SchemaWriter<'a> {
    fn new(files: &'a [CrateFile], dir: &'a Path) -> Self {
        let written = files.iter().map(|file| file.original.clone()).collect();
        Self {
            files,
            dir,
            written,
        }
    }

    /// Returns the total size of the generated text.
    fn write(&mut self, rendered: &[Rendered]) -> io::Result<usize> {
        let mut total = 0;
        for (index, output) in rendered.iter().enumerate() {
            total += output.text.len();
            if self.written[index] != output.text {
                fs::write(self.dir.join(&self.files[index].path), &output.text)?;
                self.written[index].clone_from(&output.text);
            }
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mutation::schema::PROBE_PREFIX;

    fn probe_error(id: u64) -> CompilerError {
        CompilerError {
            message: format!("cannot find value `{PROBE_PREFIX}{id:x}` in this scope"),
            ..CompilerError::default()
        }
    }

    fn failing(errors: Vec<CompilerError>) -> Build {
        Build {
            success: false,
            errors,
        }
    }

    #[test]
    fn a_second_pass_finds_the_mutants_after_the_library() {
        let all: BTreeSet<u64> = [1, 2, 3].into();
        let mut calls = Vec::new();
        let Ok(probed) = read_by_build(&all, |left| {
            calls.push(left.clone());
            Ok(if left.contains(&1) {
                failing(vec![probe_error(1)])
            } else if left.contains(&2) {
                failing(vec![probe_error(2)])
            } else {
                Build {
                    success: true,
                    errors: Vec::new(),
                }
            })
        }) else {
            panic!("the probe has no I/O");
        };
        assert_eq!(probed.read, [1, 2].into());
        assert!(probed.verified);
        assert_eq!(calls.len(), 3);
    }

    #[test]
    fn another_error_proves_nothing() {
        let all: BTreeSet<u64> = [1].into();
        let Ok(probed) = read_by_build(&all, |_| Ok(failing(vec![CompilerError::default()])))
        else {
            panic!("the probe has no I/O");
        };
        assert!(probed.read.is_empty());
        assert!(!probed.verified);
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
