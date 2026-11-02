//! The report of the `mutants` command.

use std::collections::BTreeMap;

use fallow_report_protocol::{GateFile, GateOutcome, GateStatus};
use serde::Serialize;

use crate::gates::RustGateName;
use crate::mutation::removal::{RoundRecord, Via};
use crate::mutation::status::{Status, Tier, score};
use crate::mutation::testrun::TestRun;

/// The `kind` of a run report.
pub const KIND: &str = "mutants";
/// The `kind` of an inspect report.
pub const INSPECT_KIND: &str = "mutants-inspect";
/// The `language` of each report.
pub const LANGUAGE: &str = "rust";

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
    /// The read-only next steps. The tool never writes a test or changes code, so
    /// no action can be applied automatically.
    pub actions: Vec<Action>,
}

/// A read-only next step for one mutant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Action {
    /// The kind of the step.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// Always false.
    pub auto_fixable: bool,
    /// What the step shows.
    pub description: &'static str,
    /// The command of the step.
    pub command: String,
}

/// The kind of the only action.
const INSPECT_ACTION: &str = "inspect-mutant";

/// The actions of a mutant with this status. `inspect` is the command that shows
/// one mutant, without the id.
pub fn actions_for(row: &MutantRow, inspect: &str) -> Vec<Action> {
    let description = match row.status {
        Status::Survived => {
            "Each test passed with this change. Inspect the change and the tests that execute it."
        }
        Status::NoCoverage => {
            "No test can see this change. Inspect the change and the coverage of its lines."
        }
        Status::Flaky => {
            "Two runs of this change disagree. Inspect the change and the tests that execute it."
        }
        Status::Killed
        | Status::Timeout
        | Status::CompileError
        | Status::RuntimeError
        | Status::Pending => return Vec::new(),
    };
    vec![Action {
        kind: INSPECT_ACTION,
        auto_fixable: false,
        description,
        command: format!("{inspect} {}", row.id),
    }]
}

/// The outcome of the `surviving-mutants` gate. A run with no survivor and no
/// result for some mutant gets `warn`, not `pass`, because the gate did not judge
/// those mutants.
pub fn survivor_gate(rows: &[MutantRow], no_run: bool, enforced: bool) -> GateOutcome {
    let mut per_file: BTreeMap<&str, u32> = BTreeMap::new();
    let mut undecided = false;
    for row in rows {
        match row.status {
            Status::Survived => *per_file.entry(&row.file).or_default() += 1,
            Status::Pending | Status::RuntimeError => undecided = true,
            _ => {}
        }
    }
    let survivors: u32 = per_file.values().sum();
    let status = if no_run {
        GateStatus::Skipped
    } else if survivors > 0 {
        GateStatus::Fail
    } else if undecided {
        GateStatus::Warn
    } else {
        GateStatus::Pass
    };
    let mut outcome = GateOutcome::measured(status, enforced, f64::from(survivors), 0.0);
    outcome.files = per_file
        .into_iter()
        .map(|(path, error_count)| GateFile {
            path: path.to_owned(),
            error_count,
            panicked: false,
            reason: None,
        })
        .collect();
    outcome
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
    /// The mutants that ran as a rebuild, because the schema build changes the result
    /// of a test that can see them.
    pub moved_to_rebuild: usize,
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
    /// The workspace members that depend on the package and whose tests did not run.
    /// `--test-workspace` runs them.
    pub untested_dependents: Vec<String>,
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
    /// The tests that fail when they run alone with no mutant. They decide nothing.
    pub tests_fail_alone: Vec<String>,
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

/// The diff of an `--in-diff` run.
#[derive(Debug, Clone, Serialize)]
pub struct DiffInfo {
    /// The ref of the flag.
    #[serde(rename = "ref")]
    pub reference: String,
    /// The commit that the working tree is compared with: the fork point of the
    /// ref and `HEAD`.
    pub base: String,
    /// The files with a changed line.
    pub changed_files: usize,
    /// The mutants that the run left out, because no line of the mutant changed.
    pub mutants_outside: usize,
}

/// The report of one run.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// The version of the report shape.
    pub schema_version: u32,
    /// The version of cargo-fallow.
    pub version: &'static str,
    /// Always `mutants`.
    pub kind: &'static str,
    /// Always `rust`.
    pub language: &'static str,
    /// The package.
    pub package: String,
    /// The package version.
    pub package_version: String,
    /// The number of surviving mutants.
    pub total_issues: usize,
    /// The count of each status.
    pub summary: Summary,
    /// The gate outcomes, keyed by gate name.
    pub gate_outcomes: BTreeMap<RustGateName, GateOutcome>,
    /// The diff of an `--in-diff` run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_diff: Option<DiffInfo>,
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

impl Report {
    /// True when an enforced gate failed.
    pub fn fails_run(&self) -> bool {
        self.gate_outcomes.values().any(GateOutcome::fails_run)
    }

    /// True when the gate is enforced, found no survivor, and the machine stopped
    /// the run. The gate then has no verdict that a person chose to accept, so the
    /// run is an execution error.
    pub fn gate_has_no_verdict(&self) -> bool {
        self.stopped_by == Some(StopReason::LowMemory)
            && self
                .gate_outcomes
                .values()
                .any(|gate| gate.enforced && gate.status == GateStatus::Warn)
    }
}

/// How the coverage map sees the lines of one mutant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageState {
    /// The listed tests execute a line of the mutant.
    Covered,
    /// The map has data on the lines, and no test executes them.
    NotCovered,
    /// The map has no data on the lines, so a run has each test.
    NoLineData,
    /// The mutant is in a constant context. The compiler can evaluate such code, so
    /// the map says nothing about it, and a run has each test.
    ConstantContext,
}

/// The mutant of an inspect report.
#[derive(Debug, Clone, Serialize)]
pub struct InspectedMutant {
    /// The stable id.
    pub id: String,
    /// The tier of the mutant. A schema mutant can move to the rebuild tier in a run
    /// when it breaks the schema build.
    pub tier: Tier,
    /// The operator name.
    pub operator: &'static str,
    /// The file, relative to the workspace root.
    pub file: String,
    /// The line, from 1.
    pub line: usize,
    /// The column, from 1, in bytes.
    pub column: usize,
    /// The replacement text.
    pub replacement: String,
    /// The whole lines of the mutant in the source.
    pub before: String,
    /// The same lines with the mutant applied.
    pub after: String,
    /// True when a `cfg` attribute is on an enclosing item.
    pub under_cfg: bool,
}

/// The coverage part of an inspect report.
#[derive(Debug, Clone, Serialize)]
pub struct InspectCoverage {
    /// Where the map comes from.
    pub map: MapSource,
    /// How the map sees the lines of the mutant.
    pub state: CoverageState,
    /// The tests that execute a line of the mutant, sorted by name.
    pub tests: Vec<String>,
    /// The tests with no usable data, sorted by name. A run has these tests for each
    /// mutant.
    pub tests_without_data: Vec<String>,
}

/// The report of `--inspect`.
#[derive(Debug, Clone, Serialize)]
pub struct InspectReport {
    /// The version of the report shape.
    pub schema_version: u32,
    /// The version of cargo-fallow.
    pub version: &'static str,
    /// Always `mutants-inspect`.
    pub kind: &'static str,
    /// Always `rust`.
    pub language: &'static str,
    /// The package.
    pub package: String,
    /// The mutant.
    pub mutant: InspectedMutant,
    /// The tests of the mutant.
    pub coverage: InspectCoverage,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(file: &str, status: Status) -> MutantRow {
        MutantRow {
            id: "0000000000000001".to_owned(),
            tier: Tier::Schema,
            operator: "o",
            file: file.to_owned(),
            line: 1,
            column: 1,
            original: "a".to_owned(),
            replacement: "b".to_owned(),
            status,
            reason: None,
            killed_by: None,
            retried: false,
            removed_from_schema: false,
            under_cfg: false,
            duration_ms: 0,
            tests_run: 0,
            doctests_run: 0,
            no_line_data: false,
            actions: Vec::new(),
        }
    }

    fn status_of(statuses: &[Status]) -> GateStatus {
        let rows: Vec<MutantRow> = statuses.iter().map(|s| row("a.rs", *s)).collect();
        survivor_gate(&rows, false, true).status
    }

    #[test]
    fn the_gate_counts_only_survivors_and_warns_on_a_mutant_with_no_result() {
        assert_eq!(status_of(&[]), GateStatus::Pass);
        assert_eq!(status_of(&[Status::Killed]), GateStatus::Pass);
        assert_eq!(status_of(&[Status::NoCoverage]), GateStatus::Pass);
        assert_eq!(status_of(&[Status::Flaky]), GateStatus::Pass);
        assert_eq!(status_of(&[Status::Pending]), GateStatus::Warn);
        assert_eq!(status_of(&[Status::RuntimeError]), GateStatus::Warn);
        let both = [Status::Pending, Status::Survived];
        assert_eq!(status_of(&both), GateStatus::Fail);
    }

    #[test]
    fn the_gate_names_each_file_once_with_its_count() {
        let rows = [
            row("b.rs", Status::Survived),
            row("a.rs", Status::Survived),
            row("b.rs", Status::Survived),
            row("c.rs", Status::Killed),
        ];
        let gate = survivor_gate(&rows, false, false);
        assert!(!gate.fails_run());
        assert_eq!(gate.observed, Some(3.0));
        let files: Vec<(&str, u32)> = gate
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.error_count))
            .collect();
        assert_eq!(files, [("a.rs", 1), ("b.rs", 2)]);
    }

    #[test]
    fn an_enforced_gate_has_no_verdict_after_a_low_memory_stop() {
        let verdict = |status: GateStatus, enforced: bool, stop: Option<StopReason>| {
            let mut gates = BTreeMap::new();
            gates.insert(
                RustGateName::SurvivingMutants,
                GateOutcome::measured(status, enforced, 0.0, 0.0),
            );
            let report = Report {
                schema_version: 1,
                version: "0",
                kind: KIND,
                language: LANGUAGE,
                package: String::new(),
                package_version: String::new(),
                total_issues: 0,
                summary: Summary::default(),
                gate_outcomes: gates,
                in_diff: None,
                stopped_by: stop,
                elapsed_ms: 0,
                baseline: Baseline::default(),
                schema: SchemaInfo::default(),
                rebuild: RebuildInfo::default(),
                gaps: Gaps::default(),
                selection: SelectionInfo::default(),
                guard: GuardInfo {
                    enabled: true,
                    memory_limit_mib: 1,
                    low_memory_percent: 1,
                    kills: 0,
                    jobs: 1,
                },
                mutants: Vec::new(),
            };
            report.gate_has_no_verdict()
        };
        let low = Some(StopReason::LowMemory);
        assert!(verdict(GateStatus::Warn, true, low));
        assert!(!verdict(GateStatus::Warn, false, low));
        // A survivor is a verdict, also in a stopped run.
        assert!(!verdict(GateStatus::Fail, true, low));
        // A time budget is a limit that a person chose.
        assert!(!verdict(
            GateStatus::Warn,
            true,
            Some(StopReason::TimeBudget)
        ));
    }
}
