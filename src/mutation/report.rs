//! The report of the `mutants` command.

use std::fmt::Write;

use serde::Serialize;

use crate::mutation::removal::{RoundRecord, Via};
use crate::mutation::status::{Status, Tier, score};
use crate::mutation::testrun::TestRun;

/// One mutant in the report.
#[derive(Debug, Clone, Serialize)]
pub struct MutantRow {
    /// The stable id: 16 hexadecimal digits.
    pub id: String,
    /// The tier that ran the mutant.
    pub tier: Tier,
    /// The operator name.
    pub operator: &'static str,
    /// The file, relative to the workspace root.
    pub file: String,
    /// The line in the original file, from 1.
    pub line: usize,
    /// The column in the original file, from 1, in bytes.
    pub column: usize,
    /// The start of the original text.
    pub original: String,
    /// The replacement text.
    pub replacement: String,
    /// The status.
    pub status: Status,
    /// The reason of a status without a test result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The first test that failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub killed_by: Option<String>,
    /// True when the mutant got a second run.
    pub retried: bool,
    /// True when the mutant broke the schema and then ran in a rebuild.
    pub removed_from_schema: bool,
    /// True when a `cfg` attribute is on an enclosing item.
    pub under_cfg: bool,
    /// The wall time of the runs, in milliseconds.
    pub duration_ms: u64,
    /// The nextest tests that gave a result with this mutant, over all runs.
    pub tests_run: u64,
    /// The doctests that gave a result with this mutant, over all runs.
    pub doctests_run: u64,
    /// True when the coverage map has no data on the lines of this mutant, so the
    /// mutant ran each test.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub no_line_data: bool,
}

/// The count of each status.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Summary {
    /// All mutants.
    pub total: usize,
    /// See [`Status::Killed`].
    pub killed: usize,
    /// See [`Status::Survived`].
    pub survived: usize,
    /// See [`Status::Timeout`].
    pub timeout: usize,
    /// See [`Status::Flaky`].
    pub flaky: usize,
    /// See [`Status::NoCoverage`].
    pub no_coverage: usize,
    /// See [`Status::CompileError`].
    pub compile_error: usize,
    /// See [`Status::RuntimeError`].
    pub runtime_error: usize,
    /// See [`Status::Pending`].
    pub pending: usize,
    /// Detected mutants in percent, or `None` with no test result.
    pub score: Option<f64>,
}

impl Summary {
    /// Counts the rows.
    pub fn of(rows: &[MutantRow]) -> Self {
        let mut summary = Self {
            total: rows.len(),
            ..Self::default()
        };
        for row in rows {
            let count = match row.status {
                Status::Killed => &mut summary.killed,
                Status::Survived => &mut summary.survived,
                Status::Timeout => &mut summary.timeout,
                Status::Flaky => &mut summary.flaky,
                Status::NoCoverage => &mut summary.no_coverage,
                Status::CompileError => &mut summary.compile_error,
                Status::RuntimeError => &mut summary.runtime_error,
                Status::Pending => &mut summary.pending,
            };
            *count += 1;
        }
        summary.score = score(summary.killed, summary.timeout, summary.survived);
        summary
    }
}

/// The time limit of each suite, in milliseconds.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Timeouts {
    /// A nextest run of a schema mutant.
    pub schema_nextest: u64,
    /// A doctest run of a schema mutant.
    pub schema_doc: u64,
    /// A nextest run of a rebuild mutant.
    pub rebuild_nextest: u64,
    /// A doctest run of a rebuild mutant.
    pub rebuild_doc: u64,
}

/// The baseline part of the report.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Baseline {
    /// The run on the unmodified copy.
    pub clean: Option<TestRun>,
    /// The run on the schema build with no active mutant.
    pub schema: Option<TestRun>,
    /// The tests that fail on the unmodified copy. They decide no mutant.
    pub failures: Vec<String>,
    /// The tests that pass on the unmodified copy and fail on the schema build.
    /// They decide no schema mutant.
    pub regressions: Vec<String>,
    /// True when both runs completed and `regressions` is empty.
    pub tests_pass_with_schema: bool,
    /// The time limits from these runs.
    pub timeout_ms: Timeouts,
}

/// The result of the schema build.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaStatus {
    /// The schema compiles.
    Compiled,
    /// The schema did not compile within the round cap.
    NeverCompiled,
    /// The crate has no site.
    NoSites,
    /// The command did not select this tier.
    #[default]
    NotRun,
}

/// One mutant that the removal loop removed.
#[derive(Debug, Clone, Serialize)]
pub struct RemovedMutant {
    /// The id.
    pub id: String,
    /// The file.
    pub file: String,
    /// The line.
    pub line: usize,
    /// The replacement.
    pub replacement: String,
    /// The compile round.
    pub round: u32,
    /// How the loop found the mutant.
    pub how: Via,
    /// The compiler error code.
    pub code: Option<String>,
    /// The compiler message.
    pub message: String,
}

/// One compiler error, without its spans.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorSummary {
    /// The error code.
    pub code: Option<String>,
    /// The first line of the message.
    pub message: String,
}

/// The schema part of the report.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SchemaInfo {
    /// The result of the build.
    pub status: SchemaStatus,
    /// The compile rounds, or -1 when the schema never compiled.
    pub rounds: i64,
    /// The size of the source files.
    pub source_bytes: usize,
    /// The size of the schema files.
    pub schema_bytes: usize,
    /// The mutants that the walk found.
    pub candidates: usize,
    /// The mutants in the final schema.
    pub kept: usize,
    /// The kept mutants under a `cfg` attribute.
    pub kept_under_cfg: usize,
    /// The kept mutants that a build reads, from the probe builds.
    pub kept_compile_checked: usize,
    /// False when a probe build had another error, so the probe proves nothing.
    /// Then each mutant runs, and a survivor can be code that no build reads.
    pub probe_verified: bool,
    /// The removed mutants.
    pub removed: Vec<RemovedMutant>,
    /// One record for each compile round.
    pub round_log: Vec<RoundRecord>,
    /// The errors of the last failed round.
    pub last_errors: Vec<ErrorSummary>,
}

/// The rebuild part of the report.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct RebuildInfo {
    /// The mutants that the walk found.
    pub candidates: usize,
    /// The functions with a return type that has no known replacement.
    pub unknown_return_types: u32,
    /// False when a probe build had another error, so the probe proves nothing.
    pub probe_verified: bool,
}

/// A file that the walk reached but cannot use.
#[derive(Debug, Clone, Serialize)]
pub struct FileSkip {
    /// The path.
    pub path: String,
    /// The reason.
    pub reason: String,
}

/// What the run does not cover. A score says nothing about this code.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Gaps {
    /// The source files with a mutant.
    pub files: usize,
    /// The files that the walk reached but cannot use.
    pub files_skipped: Vec<FileSkip>,
    /// The `.rs` files that no `mod` declaration reached: for example a file of a
    /// macro-declared module, a test-only file or an unused file.
    pub unreached_src_files: u32,
    /// Operator tokens in macro arguments and macro bodies.
    pub macro_tokens: u32,
    /// Schema operators in constant contexts and patterns.
    pub const_context: u32,
    /// Schema operators in `let` chains.
    pub let_chain: u32,
    /// Operators in test code.
    pub test_code: u32,
    /// The features of this run. Other feature sets are not compiled.
    pub features: &'static str,
    /// True when the doctests ran for the survivors.
    pub doctests: bool,
}

/// Where the coverage map of a run comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MapSource {
    /// The run has no map, so each mutant runs each test.
    #[default]
    None,
    /// The run built the map.
    Built,
    /// The run read the map from the cache.
    Cached,
}

/// The test selection part of the report.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SelectionInfo {
    /// True when a coverage map selects the tests of each mutant.
    pub enabled: bool,
    /// Where the map comes from.
    pub map: MapSource,
    /// The time to build the map, in milliseconds. A cached map has 0.
    pub map_ms: u64,
    /// The tests with data in the map.
    pub tests_mapped: usize,
    /// The tests with no usable data. Each mutant runs these tests.
    pub tests_unmapped: Vec<String>,
    /// The mutants with no coverage data on their lines, so they ran each test.
    pub mutants_without_line_data: usize,
    /// The mutants that the journal gave. These mutants did not run again.
    pub resumed: usize,
    /// The sum of `tests_run` over the mutants.
    pub tests_run: u64,
    /// The sum of `doctests_run` over the mutants.
    pub doctests_run: u64,
}

/// The guard part of the report.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct GuardInfo {
    /// False only with `--no-memory-guard`.
    pub enabled: bool,
    /// The limit for one test process.
    pub memory_limit_mib: u64,
    /// The floor of the free memory.
    pub low_memory_percent: u32,
    /// The process trees that the guard killed.
    pub kills: u32,
    /// The parallel jobs.
    pub jobs: usize,
}

/// Why a run stopped before its last mutant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The time budget ended.
    TimeBudget,
    /// The free memory stayed under the floor.
    LowMemory,
}

/// The report of one run.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// Always `mutation`.
    pub kind: &'static str,
    /// The package.
    pub package: String,
    /// The package version.
    pub version: String,
    /// The count of each status.
    pub summary: Summary,
    /// Why the run stopped before its last mutant.
    pub stopped_by: Option<StopReason>,
    /// The wall time of the run.
    pub elapsed_ms: u64,
    /// The baseline runs.
    pub baseline: Baseline,
    /// The schema tier.
    pub schema: SchemaInfo,
    /// The rebuild tier.
    pub rebuild: RebuildInfo,
    /// What the run does not cover.
    pub gaps: Gaps,
    /// The test selection.
    pub selection: SelectionInfo,
    /// The memory guard.
    pub guard: GuardInfo,
    /// Each mutant, by tier, file and offset.
    pub mutants: Vec<MutantRow>,
}

/// Writes the report for a person.
pub fn human(report: &Report) -> String {
    let mut out = String::new();
    let s = &report.summary;
    // A write to a `String` cannot fail.
    let _ = writeln!(
        out,
        "{} {}: {} mutants: {} killed, {} survived, {} timeout, {} flaky, {} no coverage, \
         {} compile error, {} runtime error, {} pending",
        report.package,
        report.version,
        s.total,
        s.killed,
        s.survived,
        s.timeout,
        s.flaky,
        s.no_coverage,
        s.compile_error,
        s.runtime_error,
        s.pending
    );
    let _ = match s.score {
        Some(score) => writeln!(out, "score {score:.1}%"),
        None => writeln!(out, "no score: no mutant has a test result"),
    };
    if let Some(reason) = report.stopped_by {
        let text = match reason {
            StopReason::TimeBudget => "the time budget ended",
            StopReason::LowMemory => "free memory stayed under the floor",
        };
        let _ = writeln!(out, "the run stopped early: {text}");
    }
    let _ = writeln!(out, "memory guard kills: {}", report.guard.kills);
    let selection = &report.selection;
    if selection.enabled {
        let _ = writeln!(
            out,
            "test selection: {} tests in the map, {} with no data, {} test runs, {} doctest runs",
            selection.tests_mapped,
            selection.tests_unmapped.len(),
            selection.tests_run,
            selection.doctests_run
        );
    }
    if selection.resumed > 0 {
        let _ = writeln!(
            out,
            "resumed: {} mutants came from the journal",
            selection.resumed
        );
    }
    for (status, title) in [
        (Status::Survived, "survived"),
        (Status::Flaky, "flaky"),
        (Status::RuntimeError, "runtime error"),
    ] {
        write_group(&mut out, report, status, title);
    }
    out
}

fn write_group(out: &mut String, report: &Report, status: Status, title: &str) {
    let rows: Vec<&MutantRow> = report
        .mutants
        .iter()
        .filter(|m| m.status == status)
        .collect();
    if rows.is_empty() {
        return;
    }
    let _ = writeln!(out, "\n{title}:");
    for row in rows {
        let _ = write!(
            out,
            "  {}:{} {} `{}` -> `{}` [{}]",
            row.file, row.line, row.operator, row.original, row.replacement, row.id
        );
        if let Some(reason) = &row.reason {
            let _ = write!(out, ": {reason}");
        }
        out.push('\n');
    }
}
