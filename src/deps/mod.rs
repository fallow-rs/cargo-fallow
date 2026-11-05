//! The `deps` command: dependencies in `Cargo.toml` that no code uses.
//!
//! The command resolves the workspace with `cargo metadata`, so it knows the
//! library name and the `links` key of each dependency. The module graph gives
//! the first segment of each path, import, `extern crate` and macro path in
//! each target. A dependency that no such name reaches is a finding.
//!
//! The rules stay silent where the code can use a dependency in a way that
//! syntax does not show:
//! - a dependency with a `links` key, which can link a native library or give
//!   its build script facts to the build script of the package,
//! - in a target with an `include!` call that the graph cannot read, each
//!   `name::` in the text of the build script and the runtime crate of an
//!   `X-build` build dependency, or each dependency when the package has no
//!   build script. The graph reads the file of `include!("path")`,
//! - each dependency of a package with a file that did not parse,
//! - a dev-dependency that repeats a normal dependency,
//! - each dev-dependency of a package with a compile-test harness, such as
//!   `trybuild`, because the harness builds files that no target declares.
//!
//! An optional dependency and a dependency that a feature configures give a
//! warning, not an error.

mod analyze;
pub mod drift;
mod features;
pub mod manifest;
pub mod report;
pub mod trace;
pub mod usage;

use std::path::PathBuf;

use fallow_report_protocol::{GateOutcome, GateStatus};

use crate::baseline::Entry;
use crate::cli::Format;
use crate::error::CliError;
use crate::graph::{self, Production};
use crate::render;
use crate::scope::Scope;
use crate::workspace;
use report::{DepsReport, Severity};

/// The rule severity that the gate counts.
const ERROR_SEVERITY: &str = "error";

/// The options of one `deps` run.
#[derive(Debug, Clone)]
pub struct Options {
    /// The `Cargo.toml` of the workspace, when the user gave one.
    pub manifest_path: Option<PathBuf>,
    /// The output format.
    pub format: Format,
    /// True when progress lines stay hidden.
    pub quiet: bool,
    /// True when an `error` finding fails the run.
    pub fail_on_issues: bool,
    /// The flags for `cargo metadata`: `--locked`, `--offline`, `--frozen`.
    pub cargo_flags: Vec<String>,
    /// The scope flags: `--changed-since` and the baseline flags.
    pub scope: crate::scope::ScopeArgs,
    /// The dependency of `--trace`.
    pub trace: Option<String>,
}

/// Runs the analysis and builds the section of the report.
pub fn run(options: &Options) -> Result<crate::section::Section, CliError> {
    let progress = render::Progress::new(options.quiet);
    progress.step("Resolving the workspace with cargo metadata");
    let (workspace, metadata) =
        workspace::load_resolved(options.manifest_path.as_deref(), &options.cargo_flags)?;
    progress.step("Building the module graph");
    let module_graph = graph::build(&workspace, &Production::default());
    let scope = Scope::load(&workspace, &options.scope)?;
    progress.step("Checking the dependencies");
    let drift = feature_drift(
        &workspace,
        &metadata,
        options.manifest_path.as_deref(),
        &options.cargo_flags,
    )?;
    let report = report_with(
        &workspace,
        &metadata,
        &module_graph,
        options.fail_on_issues,
        &scope,
        &drift,
    );
    scope.annotate(section(&report)?)
}

/// Runs the `feature-unification-drift` rule for the members that
/// `unify-features` names. An invalid list is an error.
pub fn feature_drift(
    workspace: &workspace::Workspace,
    metadata: &cargo_metadata::Metadata,
    manifest_path: Option<&std::path::Path>,
    cargo_flags: &[String],
) -> Result<drift::Drift, CliError> {
    let members = drift::members(workspace, &metadata.workspace_metadata)?;
    let root = workspace.root.as_std_path();
    Ok(drift::check(workspace, &members, |scope| {
        workspace::cargo_tree(manifest_path, root, cargo_flags, scope)
    }))
}

/// Checks the dependencies of a resolved workspace. The graph can have any
/// production selection: the rules do not read it. `enforced` is true when an
/// `error` finding fails the run. The report keeps only the findings in
/// `scope`.
pub fn report(
    workspace: &workspace::Workspace,
    metadata: &cargo_metadata::Metadata,
    module_graph: &graph::Graph,
    enforced: bool,
    scope: &Scope,
) -> DepsReport {
    report_with(
        workspace,
        metadata,
        module_graph,
        enforced,
        scope,
        &drift::Drift::default(),
    )
}

/// [`report`] with the findings of the `feature-unification-drift` rule.
pub fn report_with(
    workspace: &workspace::Workspace,
    metadata: &cargo_metadata::Metadata,
    module_graph: &graph::Graph,
    enforced: bool,
    scope: &Scope,
    drift: &drift::Drift,
) -> DepsReport {
    let mut analysis = analyze::analyze(workspace, metadata, module_graph);
    analysis.findings.extend(drift.findings.iter().cloned());
    analysis.skipped.extend(drift.skipped.iter().cloned());
    analysis
        .findings
        .retain(|finding| scope.keeps(&finding.manifest_path));
    analysis.findings.retain(|finding| {
        let lines = finding.line.map(|line| line..=line);
        !scope.hides(
            "deps",
            finding.rule.as_str(),
            &finding.manifest_path,
            lines.as_ref(),
        ) && scope.keeps_entry_of(|| Entry::of(&render::deps::shared_finding(finding)))
    });
    // A configured level sets the severity, so the gate counts it.
    for finding in &mut analysis.findings {
        match scope.rules().level(&[finding.rule.as_str()]) {
            Some(crate::rules::Level::Error) => finding.severity = Severity::Error,
            Some(crate::rules::Level::Warn) => finding.severity = Severity::Warning,
            _ => {}
        }
    }
    for skipped in &analysis.skipped {
        render::warn(&format!(
            "the `deps` command does not judge the package `{}`: {}",
            skipped.package, skipped.reason
        ));
    }
    let errors = analysis
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Error)
        .count();
    let status = if errors == 0 {
        GateStatus::Pass
    } else {
        GateStatus::Fail
    };
    let gate = GateOutcome::counted(status, enforced, errors as f64, ERROR_SEVERITY);
    DepsReport::new(
        workspace.members.len(),
        analysis.findings,
        analysis.skipped,
        gate,
    )
}

/// The section of a report.
pub fn section(report: &DepsReport) -> Result<crate::section::Section, CliError> {
    Ok(crate::section::Section {
        human: render::deps::human(report),
        json: crate::section::to_value(report)?,
        sarif: render::deps::sarif(report),
        findings: render::deps::findings(report),
        gates: report.gate_outcomes.clone(),
    })
}
