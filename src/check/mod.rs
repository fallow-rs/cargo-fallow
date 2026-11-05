//! The `check` command: the static checks of the workspace in one run.
//!
//! One run reads `cargo metadata` once. The module graph and the source walk
//! run on two threads: the graph feeds `deps` and `visibility`, and the walk
//! feeds `size`, `complexity` and `dupes`. `files` and `boundaries` read the
//! source on their own threads. Each check gives the same section as its own
//! command, with its own gates. [`report`] joins the sections.
//!
//! `mutants` and `visibility --precise` are not part of the run: they build
//! and run the tests or rust-analyzer, and take minutes.

pub mod report;

use std::path::PathBuf;
use std::thread;

use clap::ValueEnum;
use fallow_report_protocol::exit_code::EXIT_INVALID_INPUT;
use serde::Serialize;

use crate::cli::{Format, VisibilityOptions};
use crate::error::{CliError, ErrorCode};
use crate::finding::{Action, Category, Draft, Finding, Impact, Location, Severity};
use crate::gates::{self, RustGateName};
use crate::graph::{self, Graph, extract::Vis};
use crate::health::{
    self, CRITICAL_CYCLOMATIC, Health, LARGE_FUNCTION_LINES, Metrics, ScopedHealth,
};
use crate::render::{self, Progress};
use crate::scope::Scope;
use crate::section::Section;
use crate::structure::complexity::Function;
use crate::structure::config::{self, matches_prefix};
use crate::structure::source::{self, Loaded, Needs};
use crate::structure::{boundaries, complexity, crap, dupes, size};
use crate::workspace::{self, Workspace};
use crate::{deps, files, visibility};

/// One check of the run.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum CheckName {
    /// The parse of each target root file: the `parse-error` gate.
    Parse,
    /// Unused dependencies, the same as `cargo fallow deps`.
    Deps,
    /// Unused source files, the same as `cargo fallow files`.
    Files,
    /// Visibility in the fast mode, the same as `cargo fallow visibility`.
    Visibility,
    /// Boundary rules, the same as `cargo fallow boundaries`.
    Boundaries,
    /// File and function size, the same as `cargo fallow size`.
    Size,
    /// Complexity, the same as `cargo fallow complexity`.
    Complexity,
    /// Duplicated code, the same as `cargo fallow dupes`.
    Dupes,
}

impl CheckName {
    /// Each check, in run and report order.
    pub const ALL: [Self; 8] = [
        Self::Parse,
        Self::Deps,
        Self::Files,
        Self::Visibility,
        Self::Boundaries,
        Self::Size,
        Self::Complexity,
        Self::Dupes,
    ];

    /// The name in the report.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Parse => "parse",
            Self::Deps => "deps",
            Self::Files => "files",
            Self::Visibility => "visibility",
            Self::Boundaries => "boundaries",
            Self::Size => "size",
            Self::Complexity => "complexity",
            Self::Dupes => "dupes",
        }
    }
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
    /// The checks to run. Empty means each check.
    pub only: Vec<CheckName>,
    /// The checks to leave out.
    pub skip: Vec<CheckName>,
    /// True when the gate of each check that ran is enforced.
    pub fail_on_issues: bool,
    /// True when the `parse-error` gate is enforced.
    pub fail_on_parse_error: bool,
    /// The ref of `--base` for the file-growth gate of `size`.
    pub base: Option<String>,
    /// True with `--staged`: the file-growth gate compares the index with HEAD.
    pub staged: bool,
    /// The flags for the `cargo metadata` call of `deps`.
    pub cargo_flags: Vec<String>,
    /// The scope flags: `--changed-since` and the baseline flags.
    pub scope: crate::scope::ScopeArgs,
    /// The flags of the file scores, the hotspots and the trend.
    pub health: crate::hotspots::HealthArgs,
    /// The LCOV flags of the CRAP score.
    pub coverage: crate::structure::crap::CoverageArgs,
}

impl Options {
    /// True with `--base` or `--staged`. A growth finding depends on the old
    /// version of the run, so such a run does not judge stale baseline
    /// entries, the same as `size`.
    const fn growth_run(&self) -> bool {
        self.base.is_some() || self.staged
    }

    /// The checks that run, in report order.
    pub fn selected(&self) -> Vec<CheckName> {
        CheckName::ALL
            .into_iter()
            .filter(|name| self.only.is_empty() || self.only.contains(name))
            .filter(|name| !self.skip.contains(name))
            .collect()
    }
}

/// Runs `check` and returns the process exit code.
pub fn run(options: &Options) -> u8 {
    execute(options)
        .and_then(|report| report.write(options.format))
        .unwrap_or_else(|err| {
            err.emit(options.format);
            EXIT_INVALID_INPUT
        })
}

/// The settings of the structure checks that read the source walk.
struct SourceSettings {
    /// True when a health flag needs the file scores.
    scores: bool,
    size: Option<config::Size>,
    /// The changes for the file-growth gate of `size`, with `--base` or
    /// `--staged`.
    changes: Option<crate::structure::git::Changes>,
    complexity: Option<config::Complexity>,
    dupes: Option<config::Duplication>,
    /// The LCOV data of `--coverage`.
    coverage: Option<crap::Lcov>,
}

impl SourceSettings {
    /// The tables of the checks in `runs`.
    fn read(workspace: &Workspace, runs: &impl Fn(CheckName) -> bool) -> Result<Self, CliError> {
        let complexity: Option<config::Complexity> = runs(CheckName::Complexity)
            .then(|| config::section(workspace, "complexity"))
            .transpose()?;
        if let Some(complexity) = &complexity {
            complexity.validate()?;
        }
        Ok(Self {
            scores: false,
            changes: None,
            coverage: None,
            size: runs(CheckName::Size)
                .then(|| config::section(workspace, "size"))
                .transpose()?,
            complexity,
            dupes: runs(CheckName::Dupes)
                .then(|| config::section(workspace, "dupes"))
                .transpose()?,
        })
    }
}

/// The changes for the file-growth gate of `size`, with `--base` or
/// `--staged`. A base that git cannot read stops the run, as in
/// `size --base`.
fn growth_changes(
    options: &Options,
    workspace: &Workspace,
    size_runs: bool,
) -> Result<Option<crate::structure::git::Changes>, CliError> {
    let root = workspace.root.as_std_path();
    if !size_runs && (options.base.is_some() || options.staged) {
        return Err(CliError::new(
            ErrorCode::InvalidArgument,
            "`--base` and `--staged` need the size check, and this run leaves it out",
        )
        .with_help("Add `size` to `--only`, or remove it from `--skip`."));
    }
    Ok(match (&options.base, options.staged) {
        _ if !size_runs => None,
        (Some(base), _) => Some(crate::structure::git::since_base(root, base)?),
        (None, true) => Some(crate::structure::git::staged(root)?),
        (None, false) => None,
    })
}

/// Reads the LCOV file of `--coverage`. The flag needs the complexity check.
fn read_coverage(
    options: &Options,
    workspace: &Workspace,
    complexity: bool,
) -> Result<Option<crap::Lcov>, CliError> {
    if options.coverage.path.is_some() && !complexity {
        return Err(CliError::new(
            ErrorCode::InvalidArgument,
            "`--coverage` needs the complexity check",
        )
        .with_help("Add `complexity` to `--only`, or remove it from `--skip`."));
    }
    crap::read(&options.coverage, workspace)
}

/// What the graph thread gives back.
#[derive(Default)]
struct GraphPart {
    /// The `pub` items and the `dead_public` findings of each file.
    dead_counts: Option<crate::hotspots::DeadCounts>,
    deps: Option<Section>,
    /// The `files` section, from the scans of the graph parse.
    /// The result stays apart, so an error of `files` keeps its place after
    /// the errors of the other checks.
    files: Option<Result<Section, CliError>>,
    visibility: Option<Section>,
    public_items: Option<usize>,
    dead_public: Option<usize>,
    unused_deps: Option<usize>,
}

/// What the source thread gives back.
#[derive(Default)]
struct SourcePart {
    /// The file scores, when a flag asks for them.
    file_scores: Option<Vec<crate::hotspots::FileScore>>,
    size: Option<Section>,
    complexity: Option<Section>,
    dupes: Option<Section>,
    metrics: Metrics,
}

/// The findings of the `feature-unification-drift` rule, when `deps` runs.
fn check_drift(
    options: &Options,
    workspace: &Workspace,
    metadata: Option<&cargo_metadata::Metadata>,
) -> Result<Option<deps::drift::Drift>, CliError> {
    metadata
        .map(|resolved| {
            deps::feature_drift(
                workspace,
                resolved,
                options.manifest_path.as_deref(),
                &options.cargo_flags,
            )
        })
        .transpose()
}

/// Loads the workspace. `deps` needs the resolve of `cargo metadata`.
fn load_workspace(
    options: &Options,
    deps: bool,
    progress: Progress,
) -> Result<(Workspace, Option<cargo_metadata::Metadata>), CliError> {
    if deps {
        progress.step("Resolving the workspace with cargo metadata");
        let (workspace, metadata) =
            workspace::load_resolved(options.manifest_path.as_deref(), &options.cargo_flags)?;
        return Ok((workspace, Some(metadata)));
    }
    progress.step("Reading cargo metadata");
    Ok((
        workspace::load(options.manifest_path.as_deref(), &options.cargo_flags)?,
        None,
    ))
}

/// Runs the selected checks and joins their sections.
pub fn execute(options: &Options) -> Result<report::CheckReport, CliError> {
    let selected = selected_checks(options)?;
    let runs = |name: CheckName| selected.contains(&name);
    let enforced = options.fail_on_issues;
    let progress = Progress::new(options.quiet);
    let (workspace, metadata) = load_workspace(options, runs(CheckName::Deps), progress)?;
    let scope = Scope::load(&workspace, &options.scope)?.narrowed(options.growth_run());
    // Read each configuration table before the work starts, so an invalid
    // table stops the run at once.
    let visibility_options = if runs(CheckName::Visibility) {
        Some(visibility::config::resolve(
            &workspace,
            &visibility_flags(options, enforced),
        )?)
    } else {
        None
    };
    let settings = SourceSettings {
        scores: options.health.needs_functions(),
        changes: growth_changes(options, &workspace, runs(CheckName::Size))?,
        coverage: read_coverage(options, &workspace, runs(CheckName::Complexity))?,
        ..SourceSettings::read(&workspace, &runs)?
    };

    let mut sections: Vec<(CheckName, Section)> = Vec::new();
    if runs(CheckName::Parse) {
        progress.step("Parsing target root files");
        sections.push((
            CheckName::Parse,
            parse_section(&workspace, enforced || options.fail_on_parse_error),
        ));
    }
    let needs_graph = runs(CheckName::Deps) || runs(CheckName::Visibility);
    // The deps and boundary penalties count per source file, so the walk runs
    // for them too.
    let needs_source = [
        CheckName::Deps,
        CheckName::Boundaries,
        CheckName::Size,
        CheckName::Complexity,
        CheckName::Dupes,
    ]
    .into_iter()
    .any(runs);
    let drift = check_drift(options, &workspace, metadata.as_ref())?;
    let inputs = GraphInputs {
        workspace: &workspace,
        metadata: metadata.as_ref(),
        drift: drift.as_ref(),
        visibility: visibility_options.as_ref(),
        files: runs(CheckName::Files),
        enforced,
        scope: &scope,
        progress,
    };
    let (graph_part, source_part, boundaries, files) = thread::scope(|threads| {
        let graph_thread = needs_graph.then(|| threads.spawn(|| graph_checks(&inputs)));
        let source_thread = needs_source.then(|| {
            threads.spawn(|| source_checks(&workspace, &settings, enforced, &scope, progress))
        });
        let boundaries_thread = runs(CheckName::Boundaries)
            .then(|| threads.spawn(|| boundaries::section(&workspace, enforced, &scope, progress)));
        // With the graph, the files check reads the scans of the graph parse.
        let files_thread = (inputs.files && !needs_graph).then(|| {
            threads.spawn(|| files::section(&workspace, enforced, &scope, progress, None))
        });
        let mut graph_part = join(graph_thread)?;
        let source_part = join(source_thread)?;
        let boundaries = join(boundaries_thread)?;
        let files = take_files(graph_part.as_mut(), files_thread)?;
        Ok::<_, CliError>((graph_part, source_part, boundaries, files))
    })?;
    let graph_part = graph_part.unwrap_or_default();
    let source_part = source_part.unwrap_or_default();

    let metrics = run_metrics(
        &workspace,
        source_part.metrics,
        &graph_part,
        boundaries.as_ref(),
    );
    let parts = [
        (CheckName::Deps, graph_part.deps),
        (CheckName::Files, files),
        (CheckName::Visibility, graph_part.visibility),
        (CheckName::Boundaries, boundaries),
        (CheckName::Size, source_part.size),
        (CheckName::Complexity, source_part.complexity),
        (CheckName::Dupes, source_part.dupes),
    ];
    sections.extend(
        parts
            .into_iter()
            .filter_map(|(name, section)| section.map(|section| (name, section))),
    );
    attach_suppressions(&scope, &mut sections);
    let scores = source_part
        .file_scores
        .map(|scores| crate::hotspots::with_dead_counts(scores, graph_part.dead_counts.as_ref()));
    finish(options, workspace, sections, metrics, &scope, scores)
}

/// The checks that run, or an input error when the flags leave none or when
/// a health flag needs a check that does not run.
fn selected_checks(options: &Options) -> Result<Vec<CheckName>, CliError> {
    let selected = options.selected();
    if selected.is_empty() {
        return Err(CliError::new(
            ErrorCode::InvalidArgument,
            "`--only` and `--skip` leave no check to run",
        )
        .with_help("Remove a value from `--skip`, or add one to `--only`."));
    }
    options
        .health
        .validate(selected.contains(&CheckName::Complexity))?;
    Ok(selected)
}

/// Builds the report, and adds the parts of the health flags.
fn finish(
    options: &Options,
    workspace: Workspace,
    sections: Vec<(CheckName, Section)>,
    metrics: Metrics,
    scope: &Scope,
    scores: Option<Vec<crate::hotspots::FileScore>>,
) -> Result<report::CheckReport, CliError> {
    let root = workspace.root.clone().into_std_path_buf();
    let report = report::CheckReport::new(workspace, sections, scoped_health(metrics, scope))
        .with_scope(scope)?;
    crate::hotspots::attach(report, &root, &options.health, scores)
}

/// The inputs of the health score: the source counts with the counts of the
/// graph checks and the boundaries check.
fn run_metrics(
    workspace: &Workspace,
    mut metrics: Metrics,
    graph_part: &GraphPart,
    boundaries: Option<&Section>,
) -> Metrics {
    metrics.public_items = graph_part.public_items;
    metrics.dead_public = graph_part.dead_public;
    metrics.unused_deps = graph_part.unused_deps;
    metrics.declared_dependencies = graph_part.unused_deps.map(|_| {
        workspace
            .members
            .iter()
            .map(|member| member.links.len())
            .sum()
    });
    metrics.boundary_violations = boundaries.map(|section| section.findings.len());
    metrics
}

/// The health part of the report. The penalties count the whole workspace, so
/// a scoped run gives no score.
fn scoped_health(metrics: Metrics, scope: &Scope) -> Option<Health> {
    health::score(metrics).map(|health| {
        if let Some(filter) = scope.filter() {
            Health::Scoped(ScopedHealth::filtered(filter))
        } else if scope.has_baseline() {
            Health::Scoped(ScopedHealth::baseline())
        } else {
            Health::Score(Box::new(health))
        }
    })
}

/// Adds the suppression results to each section except the parse gate.
fn attach_suppressions(scope: &Scope, sections: &mut [(CheckName, Section)]) {
    for (name, section) in sections {
        if *name != CheckName::Parse {
            scope.attach_suppressions(section);
            scope.apply_rules(section);
        }
    }
}

/// The result of a thread, or `None` when the thread did not start. A panic
/// of a check thread is an error of the run.
fn join<T>(
    handle: Option<thread::ScopedJoinHandle<'_, Result<T, CliError>>>,
) -> Result<Option<T>, CliError> {
    handle
        .map(|handle| {
            handle.join().unwrap_or_else(|_| {
                Err(CliError::new(
                    ErrorCode::OutputFailed,
                    "a check stopped with an internal error",
                ))
            })
        })
        .transpose()
}

/// The flags of a `visibility` run in the fast mode, with no flag that the
/// configuration table can set.
fn visibility_flags(options: &Options, enforced: bool) -> VisibilityOptions {
    VisibilityOptions {
        manifest_path: options.manifest_path.clone(),
        format: options.format,
        quiet: options.quiet,
        production: Vec::new(),
        exclude: Vec::new(),
        public: None,
        uniform_fields: false,
        kinds: Vec::new(),
        fail_on_issues: enforced,
        scope: crate::scope::ScopeArgs::default(),
        precise: None,
        cargo_flags: options.cargo_flags.clone(),
        trace: None,
    }
}

/// The `files` section: from the graph thread when it ran there, else from
/// its own thread. Its error comes after the errors of the other checks.
fn take_files(
    graph: Option<&mut GraphPart>,
    own: Option<thread::ScopedJoinHandle<'_, Result<Section, CliError>>>,
) -> Result<Option<Section>, CliError> {
    match graph.and_then(|part| part.files.take()) {
        Some(files) => files.map(Some),
        None => join(own),
    }
}

/// The inputs of the checks on the module graph.
struct GraphInputs<'a> {
    workspace: &'a Workspace,
    /// The resolve, when `deps` runs.
    metadata: Option<&'a cargo_metadata::Metadata>,
    /// The findings of the `feature-unification-drift` rule, when `deps` runs.
    drift: Option<&'a deps::drift::Drift>,
    /// The options, when `visibility` runs.
    visibility: Option<&'a VisibilityOptions>,
    /// True when `files` runs. It then reads the scans of the graph parse.
    files: bool,
    enforced: bool,
    scope: &'a Scope,
    progress: Progress,
}

/// Builds the module graph once and runs `deps`, `visibility` and `files` on
/// its parse.
fn graph_checks(inputs: &GraphInputs<'_>) -> Result<GraphPart, CliError> {
    let GraphInputs {
        workspace,
        visibility: visibility_options,
        enforced,
        scope,
        progress,
        ..
    } = *inputs;
    progress.step("Building the module graph");
    // Only the visibility check reads the production selection.
    let production = graph::Production {
        bins: visibility_options
            .map(|options| options.production.clone())
            .unwrap_or_default(),
    };
    let (module_graph, scans) = if inputs.files {
        let (module_graph, scans) = graph::build_with_scans(workspace, &production);
        (module_graph, Some(scans))
    } else {
        (graph::build(workspace, &production), None)
    };
    thread::scope(|threads| {
        let files_thread = scans.map(|scans| {
            threads.spawn(|| files::section(workspace, enforced, scope, progress, Some(scans)))
        });
        let mut part = graph_sections(inputs, &module_graph)?;
        part.files = join(files_thread).transpose();
        Ok(part)
    })
}

/// The `deps` and `visibility` sections on the module graph.
fn graph_sections(inputs: &GraphInputs<'_>, module_graph: &Graph) -> Result<GraphPart, CliError> {
    let GraphInputs {
        workspace,
        metadata,
        drift,
        visibility: visibility_options,
        enforced,
        scope,
        progress,
        ..
    } = *inputs;
    let mut part = GraphPart::default();
    if let Some(metadata) = metadata {
        progress.step("Checking the dependencies");
        let none = deps::drift::Drift::default();
        let drift = drift.unwrap_or(&none);
        let report = deps::report_with(workspace, metadata, module_graph, enforced, scope, drift);
        part.unused_deps = Some(
            report
                .findings
                .iter()
                .filter(|finding| finding.severity == deps::report::Severity::Error)
                // The denominator counts member dependencies only.
                .filter(|finding| finding.rule != deps::report::Rule::UnusedWorkspaceDependency)
                .count(),
        );
        part.deps = Some(deps::section(&report)?);
    }
    if let Some(options) = visibility_options {
        progress.step("Checking the visibility");
        let report = visibility::fast_report(module_graph, options, workspace, scope);
        part.public_items = Some(public_items(module_graph, options));
        part.dead_public = Some(report.summary.dead_public);
        // A scope, a baseline or a kind filter hides findings, so the ratio
        // would read too low. The file scores then have no ratio.
        let all_dead = scope.is_full()
            && !scope.has_baseline()
            && (options.kinds.is_empty()
                || options.kinds.contains(&visibility::FindingKind::DeadPublic));
        part.dead_counts = all_dead.then(|| dead_counts(module_graph, options, &report));
        part.visibility = Some(report.section()?);
    }
    Ok(part)
}

/// The `pub` items and the `dead_public` findings of each file, for the
/// file scores.
fn dead_counts(
    module_graph: &Graph,
    options: &VisibilityOptions,
    report: &visibility::report::VisibilityReport,
) -> crate::hotspots::DeadCounts {
    let mut counts = crate::hotspots::DeadCounts::new();
    for item in &module_graph.items {
        if item.vis == Vis::Public && !is_api(&module_graph.crates[item.krate], options) {
            counts.entry(item.file.clone()).or_default().0 += 1;
        }
    }
    for finding in &report.findings {
        if finding.kind == visibility::FindingKind::DeadPublic.label() {
            counts.entry(finding.file.clone()).or_default().1 += 1;
        }
    }
    counts
}

/// The `pub` items of the crates whose public items are not an external API.
fn public_items(module_graph: &Graph, options: &VisibilityOptions) -> usize {
    module_graph
        .items
        .iter()
        .filter(|item| item.vis == Vis::Public)
        .filter(|item| !is_api(&module_graph.crates[item.krate], options))
        .count()
}

/// True when the public items of the crate are an external API.
fn is_api(krate: &graph::Crate, options: &VisibilityOptions) -> bool {
    visibility::is_api(krate, &options.exclude, options.public.as_deref())
}

/// Walks the source once and runs `size`, `complexity` and `dupes` on it.
fn source_checks(
    workspace: &Workspace,
    settings: &SourceSettings,
    enforced: bool,
    scope: &Scope,
    progress: Progress,
) -> Result<SourcePart, CliError> {
    progress.step("Reading the source files");
    let measures = settings.size.is_some() || settings.complexity.is_some();
    let loaded = source::load(
        workspace,
        Needs {
            functions: measures,
            tokens: settings
                .dupes
                .as_ref()
                .map(|dupes| (dupes.mode, dupes.ignore_imports)),
        },
    );
    let mut part = SourcePart {
        metrics: function_metrics(&loaded, settings),
        file_scores: settings
            .complexity
            .as_ref()
            .filter(|_| settings.scores)
            .map(|complexity| crate::hotspots::file_scores(&loaded, &complexity.exclude)),
        ..SourcePart::default()
    };
    if let Some(size) = &settings.size {
        progress.step("Measuring the files");
        part.size = Some(size::section(&size::Inputs {
            workspace,
            loaded: &loaded,
            settings: size,
            changes: settings.changes.as_ref(),
            enforced,
            scope,
        })?);
    }
    if let Some(complexity) = &settings.complexity {
        progress.step("Measuring the complexity");
        let coverage = settings
            .coverage
            .as_ref()
            .map(|lcov| crap::join(lcov, &loaded, workspace.root.as_std_path()))
            .transpose()?;
        part.complexity = Some(complexity::section(
            &loaded,
            complexity,
            enforced,
            scope,
            coverage.as_ref(),
        )?);
    }
    if let Some(dupes) = &settings.dupes {
        let gates = dupes::Enforce {
            clones: enforced,
            threshold: enforced,
        };
        part.dupes = Some(dupes::section(&loaded, dupes, gates, scope, progress)?);
        part.metrics.duplication_percentage = part.dupes.as_ref().and_then(|section| {
            section.json["summary"]["stats"]["duplication_percentage"].as_f64()
        });
    }
    Ok(part)
}

/// The function counts of the health score. Each count leaves out the files
/// that the `exclude` list of its own check names.
fn function_metrics(loaded: &Loaded, settings: &SourceSettings) -> Metrics {
    let count = |exclude: &[String], keep: &dyn Fn(&Function) -> bool| -> (usize, usize) {
        let functions = loaded
            .files
            .iter()
            .filter(|file| !matches_prefix(&file.path, exclude))
            .flat_map(|file| &file.functions);
        functions.fold((0, 0), |(all, kept), function| {
            (all + 1, kept + usize::from(keep(function)))
        })
    };
    let complexity = settings.complexity.as_ref().map(|complexity| {
        count(&complexity.exclude, &|function| {
            function.cyclomatic >= CRITICAL_CYCLOMATIC
        })
    });
    let size = settings.size.as_ref().map(|size| {
        count(&size.exclude, &|function| {
            function.line_count() > LARGE_FUNCTION_LINES
        })
    });
    Metrics {
        total_files: loaded.files.len(),
        complexity_functions: complexity.map(|(all, _)| all),
        critical_functions: complexity.map(|(_, kept)| kept),
        size_functions: size.map(|(all, _)| all),
        large_functions: size.map(|(_, kept)| kept),
        ..Metrics::default()
    }
}

/// The section of the parse gate.
fn parse_section(workspace: &Workspace, enforced: bool) -> Section {
    let gates::Evaluation {
        outcomes,
        unreadable,
    } = gates::evaluate(workspace, enforced);
    for file in &unreadable {
        render::warn(&format!(
            "cannot read `{}`, so the parse-error gate does not judge it: {}",
            file.path, file.error
        ));
    }
    let gates = outcomes;
    let findings = gates
        .get(&RustGateName::ParseError)
        .map(|outcome| {
            outcome
                .files
                .iter()
                .map(|file| {
                    Finding::new(Draft {
                        check: CheckName::Parse.as_str(),
                        rule: RustGateName::ParseError.as_str(),
                        severity: Severity::Error,
                        message: render::parse_message(file).to_owned(),
                        location: Location {
                            path: file.path.clone(),
                            line: None,
                            column: None,
                            end_line: None,
                        },
                        identity: &[&file.path],
                        category: Category::BugRisk,
                        impact: Impact::Major,
                    })
                    .with_actions(vec![Action::new(
                        "fix-syntax",
                        false,
                        "Fix the syntax error so the file parses",
                    )])
                })
                .collect()
        })
        .unwrap_or_default();
    Section {
        human: render::parse_human(&gates),
        json: serde_json::Value::Null,
        sarif: render::parse_sarif(&gates),
        findings,
        gates,
    }
}
