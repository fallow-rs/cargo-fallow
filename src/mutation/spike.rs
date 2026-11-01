//! The hidden `schema-spike` command: one crate, one report.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use crate::error::{CliError, ErrorCode};
use crate::gates::parse_edition;
use crate::mutation::baseline::{Comparison, TestPlan, TestRun, compare, run_tests};
use crate::mutation::compile::{
    Build, CompilerError, LoopResult, Removal, RoundRecord, Via, parse_build, probed_mutants,
    render_all, run_loop,
};
use crate::mutation::files::{Collected, CrateFile, TargetRoot, collect, copy_tree};
use crate::mutation::process::{Exit, cargo, run_to_files};
use crate::mutation::schema::{Mode, Rendered};
use crate::mutation::sites::Skipped;
use crate::workspace::{self, Member, Workspace};

/// The time limit of one compile.
const BUILD_LIMIT: Duration = Duration::from_secs(3600);

/// The target kinds that the spike mutates. A proc-macro runs in the compiler,
/// so a runtime switch has no test run to select a mutant in.
const MUTATED_KINDS: [&str; 6] = ["lib", "rlib", "dylib", "cdylib", "staticlib", "bin"];

/// The inputs of the command.
#[derive(Debug, Clone)]
pub struct Options {
    /// The manifest of the workspace or the package.
    pub manifest_path: PathBuf,
    /// The package to mutate.
    pub package: String,
    /// A new folder for the copies, the build output and the logs.
    pub work_dir: PathBuf,
    /// True when the baseline test runs stay out.
    pub skip_tests: bool,
    /// The time limit of each test suite.
    pub test_limit: Duration,
}

/// The result of the crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The schema compiles.
    Compiled,
    /// The schema did not compile within the round cap.
    NeverCompiled,
    /// The unmodified crate does not compile, so the spike has no reference.
    CleanBuildFailed,
    /// The crate has no site.
    NoSites,
}

/// The report of the command.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    package: String,
    version: String,
    /// The first target root, relative to the workspace root.
    root_file: String,
    /// The package folder, relative to the workspace root.
    package_dir: String,
    status: Status,
    source_lines: usize,
    source_bytes: usize,
    schema_bytes: usize,
    files: usize,
    files_skipped: Vec<FileSkip>,
    unreached_src_files: u32,
    candidates: usize,
    kept: usize,
    kept_under_cfg: usize,
    /// The kept mutants that a build reads, from a probe build. A mutant under
    /// a `cfg` that is off has no check from the compile rounds. This number is
    /// a lower bound for a package with a library and a binary, because cargo
    /// stops at the library in the probe build.
    kept_compile_checked: usize,
    /// The compile rounds, or -1 when the schema never compiled.
    rounds: i64,
    skipped_operators: SkippedCounts,
    removed: Vec<RemovedMutant>,
    round_log: Vec<RoundRecord>,
    last_errors: Vec<ErrorSummary>,
    baseline: Baseline,
    mutants: Vec<MutantRow>,
}

#[derive(Debug, Clone, Serialize)]
struct FileSkip {
    path: String,
    reason: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct SkippedCounts {
    const_context: u32,
    test_code: u32,
    let_chain: u32,
    macro_tokens: u32,
}

#[derive(Debug, Clone, Serialize)]
struct RemovedMutant {
    id: String,
    file: String,
    line: usize,
    replacement: String,
    round: u32,
    how: Via,
    code: Option<String>,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
struct ErrorSummary {
    code: Option<String>,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
struct MutantRow {
    id: String,
    file: String,
    line: usize,
    replacement: String,
    kept: bool,
    under_cfg: bool,
}

/// The baseline part of the report.
#[derive(Debug, Clone, Serialize)]
struct Baseline {
    /// `compared`, `skipped` or `not_run`.
    status: &'static str,
    clean: Option<TestRun>,
    schema: Option<TestRun>,
    regressions: Vec<String>,
    tests_pass_with_schema: bool,
}

impl Baseline {
    const fn absent(status: &'static str) -> Self {
        Self {
            status,
            clean: None,
            schema: None,
            regressions: Vec::new(),
            tests_pass_with_schema: false,
        }
    }
}

/// Runs the command.
pub fn run(options: &Options) -> Result<Report, CliError> {
    let workspace = workspace::load(Some(&options.manifest_path))?;
    let member = find_member(&workspace, &options.package)?;
    let dirs = WorkDirs::create(&options.work_dir)
        .map_err(|err| io_error("create the work folder", &err))?;
    prepare_copies(workspace.root.as_std_path(), &dirs)
        .map_err(|err| io_error("copy the source tree", &err))?;
    let collected = collect(&dirs.clean, &target_roots(member));
    run_collected(options, member, &dirs, &collected).map_err(|err| io_error("run a build", &err))
}

fn find_member<'a>(workspace: &'a Workspace, package: &str) -> Result<&'a Member, CliError> {
    workspace
        .members
        .iter()
        .find(|member| member.name == package)
        .ok_or_else(|| {
            CliError::new(
                ErrorCode::InvalidArgument,
                format!("the workspace has no package `{package}`"),
            )
        })
}

fn io_error(action: &str, err: &io::Error) -> CliError {
    CliError::new(ErrorCode::SpikeFailed, format!("cannot {action}: {err}"))
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

struct WorkDirs {
    clean: PathBuf,
    schema: PathBuf,
    target_clean: PathBuf,
    target_schema: PathBuf,
    logs: PathBuf,
}

impl WorkDirs {
    /// Creates the work folder. An existing folder with content is an error, so
    /// the command never writes over other files.
    fn create(work: &Path) -> io::Result<Self> {
        if fs::read_dir(work).is_ok_and(|mut entries| entries.next().is_some()) {
            return Err(io::Error::other(format!(
                "the work folder `{}` is not empty",
                work.display()
            )));
        }
        fs::create_dir_all(work)?;
        let work = work.canonicalize()?;
        let dirs = Self {
            clean: work.join("clean"),
            schema: work.join("schema"),
            target_clean: work.join("target-clean"),
            target_schema: work.join("target-schema"),
            logs: work.join("logs"),
        };
        fs::create_dir_all(&dirs.logs)?;
        Ok(dirs)
    }
}

/// Writes the clean copy with a lock file, then the schema copy from it, so
/// both builds use the same dependency versions.
fn prepare_copies(root: &Path, dirs: &WorkDirs) -> io::Result<()> {
    copy_tree(root, &dirs.clean)?;
    if !dirs.clean.join("Cargo.lock").exists() {
        let mut command = cargo(&dirs.clean, &dirs.target_clean);
        command.arg("generate-lockfile");
        let out = dirs.logs.join("lockfile.out");
        let err = dirs.logs.join("lockfile.err");
        run_to_files(command, &out, &err, BUILD_LIMIT)?;
    }
    copy_tree(&dirs.clean, &dirs.schema)
}

/// The compile command. It runs in the package folder, because `-p name` is
/// ambiguous when a dependency has the same name.
fn build_command(dir: &Path, target_dir: &Path) -> std::process::Command {
    let mut command = cargo(dir, target_dir);
    command.args(["test", "--no-run", "--message-format=json"]);
    command
}

fn compile_once(dir: &Path, target_dir: &Path, logs: &Path, label: &str) -> io::Result<Build> {
    let out = logs.join(format!("{label}.json"));
    let err = logs.join(format!("{label}.err"));
    let exit = run_to_files(build_command(dir, target_dir), &out, &err, BUILD_LIMIT)?;
    let mut build = parse_build(&fs::read_to_string(out)?);
    build.success = build.success && exit == Exit::Code(Some(0));
    Ok(build)
}

fn run_collected(
    options: &Options,
    member: &Member,
    dirs: &WorkDirs,
    collected: &Collected,
) -> io::Result<Report> {
    let mut report = Report::new(member, collected);
    if report.candidates == 0 {
        report.status = Status::NoSites;
        report.rounds = 0;
        return Ok(report);
    }
    let package_dir = package_folder(member);
    let clean_package = dirs.clean.join(&package_dir);
    let schema_package = dirs.schema.join(&package_dir);
    let clean = compile_once(
        &clean_package,
        &dirs.target_clean,
        &dirs.logs,
        "clean-build",
    )?;
    if !clean.success {
        report.status = Status::CleanBuildFailed;
        report.last_errors = summarize(&clean.errors);
        return Ok(report);
    }
    let mut writer = SchemaWriter::new(&collected.files, &dirs.schema);
    let mut round = 0_u32;
    let mut failure = None;
    let result = run_loop(&collected.files, |rendered| {
        round += 1;
        let compiled = writer.write(rendered).and_then(|bytes| {
            report.schema_bytes = bytes;
            compile_once(
                &schema_package,
                &dirs.target_schema,
                &dirs.logs,
                &format!("round-{round:02}"),
            )
        });
        compiled.unwrap_or_else(|err| {
            failure = Some(err);
            Build::default()
        })
    });
    if let Some(err) = failure {
        return Err(err);
    }
    report.apply_loop(&result);
    if result.rounds.is_none() {
        return Ok(report);
    }
    report.kept_compile_checked = probe(
        &collected.files,
        &mut writer,
        &result,
        &schema_package,
        dirs,
    )?;
    report.baseline = if options.skip_tests {
        Baseline::absent("skipped")
    } else {
        run_baseline(options, &clean_package, &schema_package, dirs)?
    };
    Ok(report)
}

/// Counts the kept mutants that a build reads, and restores the final schema.
fn probe(
    files: &[CrateFile],
    writer: &mut SchemaWriter<'_>,
    result: &LoopResult,
    schema_package: &Path,
    dirs: &WorkDirs,
) -> io::Result<usize> {
    writer.write(&render_all(files, &result.kept, Mode::Probe))?;
    let build = compile_once(schema_package, &dirs.target_schema, &dirs.logs, "probe")?;
    writer.write(&render_all(files, &result.kept, Mode::Schema))?;
    Ok(probed_mutants(&build).intersection(&result.kept).count())
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

fn run_baseline(
    options: &Options,
    clean_package: &Path,
    schema_package: &Path,
    dirs: &WorkDirs,
) -> io::Result<Baseline> {
    let plan = |dir, target_dir, label| TestPlan {
        dir,
        target_dir,
        logs: &dirs.logs,
        label,
        limit: options.test_limit,
    };
    let clean = run_tests(plan(clean_package, &dirs.target_clean, "clean"))?;
    let schema = run_tests(plan(schema_package, &dirs.target_schema, "schema"))?;
    let Comparison {
        regressions,
        tests_pass_with_schema,
    } = compare(&clean, &schema);
    Ok(Baseline {
        status: "compared",
        clean: Some(clean),
        schema: Some(schema),
        regressions,
        tests_pass_with_schema,
    })
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

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count() + 1
}

fn id_text(id: u64) -> String {
    format!("{id:x}")
}

impl Report {
    fn new(member: &Member, collected: &Collected) -> Self {
        let mut skipped = Skipped::default();
        let mut mutants = Vec::new();
        for file in &collected.files {
            let found = &file.found;
            skipped.const_context += found.skipped.const_context;
            skipped.test_code += found.skipped.test_code;
            skipped.let_chain += found.skipped.let_chain;
            skipped.macro_tokens += found.skipped.macro_tokens;
            for site in &found.sites {
                let line = line_of(&file.original, site.range.start);
                mutants.extend(site.mutants.iter().map(|mutant| MutantRow {
                    id: id_text(mutant.id),
                    file: file.path.clone(),
                    line,
                    replacement: mutant.replacement.to_owned(),
                    kept: true,
                    under_cfg: file.under_cfg || site.under_cfg,
                }));
            }
        }
        let source_bytes = collected.files.iter().map(|file| file.original.len()).sum();
        Self {
            package: member.name.clone(),
            version: member.version.clone(),
            root_file: collected
                .files
                .iter()
                .find(|file| file.root.is_some())
                .map(|file| file.path.clone())
                .unwrap_or_default(),
            package_dir: package_folder(member).to_string_lossy().into_owned(),
            status: Status::NeverCompiled,
            source_lines: collected
                .files
                .iter()
                .map(|file| file.original.lines().count())
                .sum(),
            source_bytes,
            schema_bytes: source_bytes,
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
            candidates: mutants.len(),
            kept: mutants.len(),
            kept_under_cfg: mutants.iter().filter(|mutant| mutant.under_cfg).count(),
            kept_compile_checked: 0,
            rounds: -1,
            skipped_operators: SkippedCounts {
                const_context: skipped.const_context,
                test_code: skipped.test_code,
                let_chain: skipped.let_chain,
                macro_tokens: skipped.macro_tokens,
            },
            removed: Vec::new(),
            round_log: Vec::new(),
            last_errors: Vec::new(),
            baseline: Baseline::absent("not_run"),
            mutants,
        }
    }

    fn apply_loop(&mut self, result: &LoopResult) {
        self.round_log.clone_from(&result.log);
        self.last_errors = summarize(&result.last_errors);
        self.removed = result
            .removed
            .iter()
            .map(|removal| self.removed_row(removal))
            .collect();
        for row in &mut self.mutants {
            row.kept = !result
                .removed
                .iter()
                .any(|removal| id_text(removal.id) == row.id);
        }
        self.kept = self.mutants.iter().filter(|row| row.kept).count();
        self.kept_under_cfg = self
            .mutants
            .iter()
            .filter(|row| row.kept && row.under_cfg)
            .count();
        match result.rounds {
            Some(rounds) => {
                self.status = Status::Compiled;
                self.rounds = i64::from(rounds);
            }
            None => {
                self.status = Status::NeverCompiled;
                self.rounds = -1;
            }
        }
    }

    fn removed_row(&self, removal: &Removal) -> RemovedMutant {
        let id = id_text(removal.id);
        let row = self.mutants.iter().find(|row| row.id == id);
        RemovedMutant {
            file: row.map(|row| row.file.clone()).unwrap_or_default(),
            line: row.map_or(0, |row| row.line),
            replacement: row.map(|row| row.replacement.clone()).unwrap_or_default(),
            id,
            round: removal.round,
            how: removal.how,
            code: removal.code.clone(),
            message: removal.message.clone(),
        }
    }
}
