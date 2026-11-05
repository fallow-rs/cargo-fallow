//! The `fix` command: applies the `auto_fixable` actions of the findings.
//!
//! The command runs the `deps` and the `visibility` checks, or the checks
//! that `--only` and `--skip` select. It edits each
//! `Cargo.toml` with an error finding: it removes an unused entry, or moves a
//! misplaced entry to `[dev-dependencies]`. It narrows the visibility keyword
//! of each `visibility` finding. A warning stays. `--dry-run` writes nothing.
//!
//! After the edits, the command loads the workspace with `cargo metadata`
//! again. With a visibility edit, it also runs `cargo check --workspace
//! --all-targets`, before and after the edits. When a step after the edits
//! fails, the command writes back the old bytes of each file and exits 2.

pub mod check;
pub mod deps;
pub mod visibility;

use std::fmt::Write as _;
use std::fs;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

use clap::Args;
use fallow_report_protocol::exit_code::EXIT_OK;
use rustc_hash::FxHashMap;
use serde::Serialize;

use crate::cli::{Format, VisibilityOptions};
use crate::error::{CliError, ErrorCode};
use crate::render::{self, Progress};
use crate::report::TOOL_VERSION;
use crate::scope::Scope;
use crate::section::pretty;
use crate::workspace::{self, Workspace};

/// The version of the report shape of `fix`.
pub const SCHEMA_VERSION: u32 = 1;

/// The flags of `fix`.
#[derive(Debug, Clone, Args)]
pub struct FixArgs {
    /// Path to the Cargo.toml of the workspace.
    #[arg(long, value_name = "PATH")]
    manifest_path: Option<PathBuf>,
    /// The output format: `human` or `json`.
    #[arg(long, default_value = "human", value_parser = crate::cli::human_or_json())]
    format: Format,
    /// Hide the progress lines on stderr.
    #[arg(long)]
    quiet: bool,
    /// Show the edits and write nothing.
    #[arg(long)]
    dry_run: bool,
    /// Confirm the edits. A process without a terminal on stdin needs this
    /// flag. In a terminal, the command applies the edits also without it.
    #[arg(long, alias = "force")]
    yes: bool,
    // `fix` keeps its own flags, not `CargoArgs`: after the edits,
    // `Cargo.lock` can change, so the later calls drop `--locked`.
    /// Give `--locked` to the `cargo metadata` call of the analysis.
    #[arg(long)]
    locked: bool,
    /// Give `--offline` to each cargo call.
    #[arg(long)]
    offline: bool,
    /// Give `--frozen` to the `cargo metadata` call of the analysis.
    #[arg(long)]
    frozen: bool,
    /// Apply only the edits in the files of this workspace member. The flag
    /// can repeat. The analysis still reads the whole workspace.
    #[arg(long = "package", short = 'p', value_name = "NAME")]
    packages: Vec<String>,
    /// Apply only the edits of these checks. The flag takes a list with
    /// commas and can repeat.
    #[arg(
        long,
        value_enum,
        value_name = "CHECK",
        value_delimiter = ',',
        conflicts_with = "skip"
    )]
    only: Vec<FixCheck>,
    /// Apply no edits of these checks. The flag takes a list with commas and
    /// can repeat.
    #[arg(long, value_enum, value_name = "CHECK", value_delimiter = ',')]
    skip: Vec<FixCheck>,
}

/// A check that has edits: the values of `--only` and `--skip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum FixCheck {
    /// The `Cargo.toml` edits of the `deps` check.
    Deps,
    /// The visibility keyword edits of the `visibility` check.
    Visibility,
}

impl FixArgs {
    /// The output format.
    pub const fn format(&self) -> Format {
        self.format
    }

    /// The cargo flags. `locked` is false for a call after the edits: the
    /// edits change `Cargo.lock`, so `--locked` and `--frozen` cannot hold.
    fn cargo_flags(&self, locked: bool) -> Vec<String> {
        [
            (locked && self.locked, "--locked"),
            (self.offline || self.frozen, "--offline"),
            (locked && self.frozen, "--frozen"),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, flag)| flag.to_owned())
        .collect()
    }

    /// True when `--only` and `--skip` select the check.
    fn selects(&self, check: FixCheck) -> bool {
        (self.only.is_empty() || self.only.contains(&check)) && !self.skip.contains(&check)
    }
}

/// One edit of the report.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "FixEntry")]
pub enum Fix {
    /// An edit of a `Cargo.toml`.
    Dependency(deps::Edit),
    /// An edit of a visibility keyword.
    Visibility(visibility::Edit),
}

impl Fix {
    const fn applied(&self) -> bool {
        match self {
            Self::Dependency(edit) => edit.applied,
            Self::Visibility(edit) => edit.applied,
        }
    }

    const fn mark_applied(&mut self) {
        match self {
            Self::Dependency(edit) => edit.applied = true,
            Self::Visibility(edit) => edit.applied = true,
        }
    }
}

/// The report of a `fix` run. The field order is the wire order.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct FixReport {
    /// The version of the report shape.
    pub schema_version: u32,
    /// The version of cargo-fallow.
    pub version: &'static str,
    /// Always `fix`.
    pub kind: &'static str,
    /// Always `rust`.
    pub language: &'static str,
    /// True when `--dry-run` was given and no file changed.
    pub dry_run: bool,
    /// The edits, applied or planned: the `Cargo.toml` edits first, then the
    /// visibility edits.
    pub fixes: Vec<Fix>,
    /// The number of edits with `applied: true`.
    pub total_fixed: usize,
    /// The findings that `fix` does not change, for example a warning. A check
    /// that `--only` or `--skip` leaves out adds nothing.
    pub skipped: usize,
    /// The packages that the `deps` check did not judge, so `fix` has no edit
    /// for them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped_packages: Vec<crate::deps::report::SkippedPackage>,
}

/// The planned edits of one run.
struct Plan {
    deps: Vec<deps::Edit>,
    visibility: Vec<visibility::Edit>,
    /// The manifests of the consumer workspaces. The guard checks them too.
    consumers: Vec<PathBuf>,
    skipped: usize,
    skipped_packages: Vec<crate::deps::report::SkippedPackage>,
}

/// Runs `fix` and returns the process exit code.
pub fn run(args: &FixArgs) -> Result<u8, CliError> {
    if !args.selects(FixCheck::Deps) && !args.selects(FixCheck::Visibility) {
        return Err(CliError::new(
            ErrorCode::InvalidArgument,
            "`--skip` leaves out each check of `fix`, so no edit is possible",
        )
        .with_help("Remove a value from `--skip`. The values are `deps` and `visibility`."));
    }
    if !args.dry_run && !args.yes && !std::io::stdin().is_terminal() {
        return Err(CliError::new(
            ErrorCode::InvalidArgument,
            "`fix` needs `--yes` in a process without a terminal",
        )
        .with_help(
            "Run `cargo fallow fix --dry-run` to see the edits. Then run it again with `--yes`.",
        ));
    }
    let progress = Progress::new(args.quiet);
    progress.step("Resolving the workspace with cargo metadata");
    let (workspace, metadata) =
        workspace::load_resolved(args.manifest_path.as_deref(), &args.cargo_flags(true))?;
    let plan = plan(args, &workspace, &metadata, progress)?;
    let mut fixes: Vec<Fix> = plan
        .deps
        .iter()
        .cloned()
        .map(Fix::Dependency)
        .chain(plan.visibility.iter().cloned().map(Fix::Visibility))
        .collect();
    let mut skipped = plan.skipped;
    if !args.dry_run && !fixes.is_empty() {
        let applied = apply_all(args, &workspace, &plan, progress)?;
        mark(&mut fixes, &applied);
        skipped += plan.visibility.len() - applied.kept.len();
    }
    let total_fixed = fixes.iter().filter(|fix| fix.applied()).count();
    let report = FixReport {
        schema_version: SCHEMA_VERSION,
        version: TOOL_VERSION,
        kind: "fix",
        language: "rust",
        dry_run: args.dry_run,
        fixes,
        total_fixed,
        skipped,
        skipped_packages: plan.skipped_packages,
    };
    let text = match args.format {
        Format::Json => pretty(&report)?,
        _ => human(&report),
    };
    render::write_stdout(text.trim_end()).map_err(|err| CliError::output(&err))?;
    Ok(EXIT_OK)
}

/// Marks each written edit as applied, and gives each left-out edit its
/// reason.
fn mark(fixes: &mut [Fix], applied: &Applied) {
    for fix in fixes {
        let written = match fix {
            Fix::Dependency(_) => true,
            Fix::Visibility(edit) => {
                if let Some(other) = applied
                    .left_out
                    .iter()
                    .find(|other| visibility::same_place(edit, other))
                {
                    edit.skip_reason = other.skip_reason;
                    edit.diagnostic.clone_from(&other.diagnostic);
                }
                applied
                    .kept
                    .iter()
                    .any(|other| visibility::same_place(edit, other))
            }
        };
        if written {
            fix.mark_applied();
        }
    }
}

/// Runs the selected checks on one module graph and plans their edits.
fn plan(
    args: &FixArgs,
    workspace: &Workspace,
    metadata: &cargo_metadata::Metadata,
    progress: Progress,
) -> Result<Plan, CliError> {
    let options = if args.selects(FixCheck::Visibility) {
        Some(visibility_options(args, workspace)?)
    } else {
        None
    };
    let scope = Scope::for_fix(workspace, &args.packages)?;
    progress.step("Building the module graph");
    // Only the visibility check reads the production selection.
    let production = crate::graph::Production {
        bins: options
            .as_ref()
            .map(|options| options.production.clone())
            .unwrap_or_default(),
    };
    let module_graph = crate::graph::build(workspace, &production);
    let mut planned = Plan {
        deps: Vec::new(),
        visibility: Vec::new(),
        consumers: Vec::new(),
        skipped: 0,
        skipped_packages: Vec::new(),
    };
    if args.selects(FixCheck::Deps) {
        progress.step("Checking the dependencies");
        let report = crate::deps::report(workspace, metadata, &module_graph, false, &scope);
        planned.deps = deps::plan(&report);
        planned.skipped += report.findings.len().saturating_sub(planned.deps.len());
        // A package outside `--package` gets no edit, so its skip is no news.
        planned.skipped_packages = report
            .skipped
            .into_iter()
            .filter(|skipped| args.packages.is_empty() || args.packages.contains(&skipped.package))
            .collect();
    }
    if let Some(options) = &options {
        // The code of a consumer workspace counts as a use, so the visibility
        // check reads a graph with its crates.
        let extended = crate::visibility::consumers::extend(workspace, options)?;
        let own = extended
            .as_ref()
            .map(|extended| crate::graph::build(&extended.workspace, &production));
        let module_graph = own.as_ref().unwrap_or(&module_graph);
        planned.consumers = extended
            .map(|extended| extended.manifests)
            .unwrap_or_default();
        let (edits, skipped) = plan_visibility(workspace, module_graph, options, &scope, progress);
        planned.visibility = edits;
        planned.skipped += skipped;
    }
    Ok(planned)
}

/// The options of the visibility check: the table, with no flags.
fn visibility_options(
    args: &FixArgs,
    workspace: &Workspace,
) -> Result<VisibilityOptions, CliError> {
    crate::visibility::config::resolve(
        workspace,
        &VisibilityOptions {
            manifest_path: args.manifest_path.clone(),
            format: args.format,
            quiet: args.quiet,
            production: Vec::new(),
            exclude: Vec::new(),
            public: None,
            uniform_fields: false,
            consumers: None,
            kinds: Vec::new(),
            fail_on_issues: false,
            scope: crate::scope::ScopeArgs::default(),
            precise: None,
            // The `cargo metadata` call of a consumer workspace reads them.
            cargo_flags: args.cargo_flags(true),
            trace: None,
        },
    )
}

/// Runs the visibility check and plans its edits. Returns the edits and the
/// number of findings without an edit.
fn plan_visibility(
    workspace: &Workspace,
    module_graph: &crate::graph::Graph,
    options: &VisibilityOptions,
    scope: &Scope,
    progress: Progress,
) -> (Vec<visibility::Edit>, usize) {
    progress.step("Checking the visibility");
    let visibility_report = crate::visibility::fast_report(module_graph, options, workspace, scope);
    let mut visibility_edits = visibility::plan(&visibility_report);
    // An edit of a grouped `use` changes the names of the group that keep
    // their uses too, so it stays for a person.
    let root = workspace.root.as_std_path();
    visibility_edits.retain(|edit| {
        fs::read_to_string(root.join(&edit.path))
            .is_ok_and(|text| !visibility::changes_a_group(&text, edit))
    });
    let fixable_rows = visibility_report
        .findings
        .iter()
        .filter(|row| row.actions.iter().any(|action| action.auto_fixable))
        .count();
    let skipped = visibility_report
        .findings
        .len()
        .saturating_sub(fixable_rows);
    (visibility_edits, skipped)
}

/// The visibility edits after the check: the written edits, and the edits
/// that the compiler messages left out, with their reason.
struct Applied {
    kept: Vec<visibility::Edit>,
    left_out: Vec<visibility::Edit>,
}

/// Writes the edits and checks the workspace. On a failure after the first
/// write, writes back the old bytes of each file.
fn apply_all(
    args: &FixArgs,
    workspace: &Workspace,
    plan: &Plan,
    progress: Progress,
) -> Result<Applied, CliError> {
    let root = workspace.root.as_std_path();
    let manifest = args.manifest_path.as_deref();
    let flags = args.cargo_flags(false);
    let compiles = !plan.visibility.is_empty();
    // `cargo check` can write `Cargo.lock`, so a failed run writes back its
    // old bytes too.
    let lock = LockFile::read(root);
    let (all_features, known, consumers) = if compiles {
        progress.step("Checking that the workspace builds before the edits");
        let (all, warnings, consumers) =
            baseline(manifest, &plan.consumers, root, &args.cargo_flags(true))
                .inspect_err(|_| lock.restore())?;
        (Some(all), warnings, consumers)
    } else {
        (None, Vec::new(), Vec::new())
    };
    let formatted = formatted_files(workspace, &plan.visibility);
    let mut kept = plan.visibility.clone();
    let mut left_out = Vec::new();
    for round in 1..=CHECK_ROUNDS {
        // When the compiler messages left out each edit, the run ends with no
        // edit, and the files keep their old bytes.
        if kept.is_empty() && plan.deps.is_empty() {
            lock.restore();
            return Ok(Applied { kept, left_out });
        }
        let planned = planned_files(root, &plan.deps, &kept).inspect_err(|_| lock.restore())?;
        progress.step("Writing the edits");
        let mut compiler = None;
        let mut warned: Vec<check::Diagnostic> = Vec::new();
        let outcome = write_and_check(&planned, || {
            if !plan.deps.is_empty() {
                workspace::load_resolved(manifest, &flags).map_err(|err| {
                    format!(
                        "the workspace does not load after the edits: {}",
                        err.message
                    )
                })?;
            }
            if let Some(all_features) = all_features {
                progress.step("Checking that the workspace builds after the edits");
                let checked = check_all(manifest, &consumers, root, &flags, all_features);
                judge(checked, &known, &mut compiler, &mut warned)?;
            }
            Ok(())
        });
        let error = match outcome {
            Ok(()) => {
                format_files(workspace, &planned, &formatted);
                return Ok(Applied { kept, left_out });
            }
            // A file that keeps the edited bytes would be the old bytes of
            // the next round, so the run stops.
            Err(Failed::Stuck(error)) => {
                lock.restore();
                return Err(error);
            }
            Err(Failed::Restored(error)) => error,
        };
        // The compiler names the items that the edits broke. Each round
        // leaves out the edits of those items, and the next round starts from
        // the old bytes again.
        let left = if round < CHECK_ROUNDS {
            leave_out(&mut kept, compiler.as_deref(), &warned)
        } else {
            Vec::new()
        };
        if left.is_empty() {
            lock.restore();
            return Err(error);
        }
        left_out.extend(left);
        progress.step("Leaving out the edits that the compiler names, and checking again");
    }
    lock.restore();
    Err(failed("the workspace does not build after the edits"))
}

/// The bytes of each file after the edits of one round.
fn planned_files(
    root: &Path,
    deps: &[deps::Edit],
    kept: &[visibility::Edit],
) -> Result<Vec<Planned>, CliError> {
    let mut planned = Vec::new();
    for (path, edits) in deps::by_manifest(deps) {
        planned.push(edited(root, path, |text| deps::apply(text, &edits))?);
    }
    for (path, edits) in visibility::by_file(kept) {
        planned.push(edited(root, path, |text| visibility::apply(text, &edits))?);
    }
    Ok(planned)
}

/// Judges the check after the edits of one round. A failed build keeps its
/// compiler errors in `compiler`, and new warnings stay in `warned`, so
/// [`leave_out`] can read them. Either one fails the round.
fn judge(
    checked: Result<Vec<check::Diagnostic>, String>,
    known: &[check::Diagnostic],
    compiler: &mut Option<String>,
    warned: &mut Vec<check::Diagnostic>,
) -> Result<(), String> {
    let after = checked.map_err(|message| {
        let text = format!(
            "the workspace does not build after the edits: {}",
            first_lines(&message)
        );
        *compiler = Some(message);
        text
    })?;
    *warned = check::new_warnings(known, &after)
        .into_iter()
        .cloned()
        .collect();
    if warned.is_empty() {
        return Ok(());
    }
    let lines: Vec<String> = warned.iter().map(check::Diagnostic::line).collect();
    Err(format!(
        "the edits add compiler warnings: {}",
        first_lines(&lines.join("\n"))
    ))
}

/// The `skip_reason` of an edit that a compiler error left out.
const COMPILER_ERROR: &str = "compiler_error";
/// The `skip_reason` of an edit that a new compiler warning left out.
const COMPILER_WARNING: &str = "compiler_warning";

/// Leaves out the edits that the compiler errors or the new warnings name,
/// and returns them with their reason. A message names an edit by the item
/// name in backticks, or by a span at the line of the edit. A warning names
/// an item of its own file. A `private_interfaces` or `private_bounds`
/// warning names the type of another file too. When no message names an
/// edit, each edit in a file of a message goes, so one bad edit does not
/// stop the run.
fn leave_out(
    kept: &mut Vec<visibility::Edit>,
    errors: Option<&str>,
    warned: &[check::Diagnostic],
) -> Vec<visibility::Edit> {
    let mut left = Vec::new();
    let error_lines: Vec<&str> = errors
        .map(|text| text.lines().collect())
        .unwrap_or_default();
    for line in &error_lines {
        let names = visibility::named_items(line);
        let place = error_place(line);
        take(kept, &mut left, COMPILER_ERROR, line, |edit| {
            names.contains(visibility::last_segment(&edit.item))
                || place.is_some_and(|(file, at)| edit.path == file && u64::from(edit.line) == at)
        });
    }
    for warning in warned {
        let names = visibility::named_items(&warning.message);
        let any_file = warning.code.starts_with("private_");
        let by_name = |edit: &visibility::Edit| {
            names.contains(visibility::last_segment(&edit.item))
                && (any_file || edit.path == warning.file)
        };
        let by_span = |edit: &visibility::Edit| {
            warning.spans.iter().any(|(file, first, last)| {
                edit.path == *file && (*first..=*last).contains(&u64::from(edit.line))
            })
        };
        // Two items of one file can have one name, for example `new`. A span
        // at one of them picks it. A `private_` warning names the type of
        // another file, so its spans do not pick.
        let both = !any_file && kept.iter().any(|edit| by_name(edit) && by_span(edit));
        take(kept, &mut left, COMPILER_WARNING, &warning.line(), |edit| {
            if both {
                by_name(edit) && by_span(edit)
            } else {
                by_name(edit) || by_span(edit)
            }
        });
    }
    if !left.is_empty() {
        return left;
    }
    for line in &error_lines {
        if let Some((file, _)) = error_place(line) {
            take(kept, &mut left, COMPILER_ERROR, line, |edit| {
                edit.path == file
            });
        }
    }
    for warning in warned {
        take(kept, &mut left, COMPILER_WARNING, &warning.line(), |edit| {
            edit.path == warning.file || warning.spans.iter().any(|(file, _, _)| edit.path == *file)
        });
    }
    left
}

/// Moves each edit that `hit` matches from `kept` to `left`, with the reason
/// and the compiler message.
fn take(
    kept: &mut Vec<visibility::Edit>,
    left: &mut Vec<visibility::Edit>,
    reason: &'static str,
    diagnostic: &str,
    hit: impl Fn(&visibility::Edit) -> bool,
) {
    let (out, stay): (Vec<_>, Vec<_>) = kept.drain(..).partition(|edit| hit(edit));
    *kept = stay;
    left.extend(out.into_iter().map(|mut edit| {
        edit.skip_reason = Some(reason);
        edit.diagnostic = Some(diagnostic.to_owned());
        edit
    }));
}

/// The file and the line of a compiler message line in the short format
/// `file:line:column: error...`.
fn error_place(line: &str) -> Option<(&str, u64)> {
    let mut parts = line.splitn(4, ':');
    let file = parts.next()?;
    let at = parts.next()?.parse().ok()?;
    parts.next()?.parse::<u64>().ok()?;
    (!file.is_empty()).then_some((file, at))
}

/// The most write and check rounds. A round after a failed `cargo check`
/// leaves out the edits of the items that the compiler errors name.
const CHECK_ROUNDS: usize = 5;

/// The most compiler error lines in an error message.
const MESSAGE_LINES: usize = 10;

/// The first lines of a compiler message.
fn first_lines(message: &str) -> String {
    message
        .lines()
        .take(MESSAGE_LINES)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Runs `cargo check` on the workspace and on each consumer workspace, and
/// joins their messages.
fn check_all(
    manifest: Option<&Path>,
    consumers: &[PathBuf],
    root: &Path,
    flags: &[String],
    all_features: bool,
) -> Result<Vec<check::Diagnostic>, String> {
    let mut all = workspace::cargo_check(manifest, root, flags, all_features)?;
    for consumer in consumers {
        all.extend(workspace::cargo_check(Some(consumer), root, flags, false)?);
    }
    Ok(all)
}

/// The consumer workspaces that build before the edits, and their warnings.
/// The guard cannot judge a consumer that does not build, so it leaves it
/// out.
fn consumer_baseline(
    consumers: &[PathBuf],
    root: &Path,
    flags: &[String],
) -> (Vec<check::Diagnostic>, Vec<PathBuf>) {
    let mut warnings = Vec::new();
    let mut kept = Vec::new();
    for consumer in consumers {
        if let Ok(found) = workspace::cargo_check(Some(consumer), root, flags, false) {
            warnings.extend(found);
            kept.push(consumer.clone());
        }
    }
    (warnings, kept)
}

/// The result of the check before the edits: true for an all-features
/// build, the warnings, and the consumer workspaces that build.
type Baseline = (bool, Vec<check::Diagnostic>, Vec<PathBuf>);

/// Checks that the workspace builds before the edits, with all features when
/// it can, as the analysis reads it. Checks each consumer workspace too.
fn baseline(
    manifest: Option<&Path>,
    consumers: &[PathBuf],
    root: &Path,
    flags: &[String],
) -> Result<Baseline, CliError> {
    let (consumer_warnings, kept) = consumer_baseline(consumers, root, flags);
    let with_consumers = |all: bool, mut warnings: Vec<check::Diagnostic>| {
        warnings.extend(consumer_warnings.iter().cloned());
        (all, warnings, kept.clone())
    };
    if let Ok(warnings) = workspace::cargo_check(manifest, root, flags, true) {
        return Ok(with_consumers(true, warnings));
    }
    workspace::cargo_check(manifest, root, flags, false)
        .map(|warnings| with_consumers(false, warnings))
        .map_err(|message| {
            failed(&format!(
                "the workspace does not build before the edits, so `fix` cannot judge them: \
                 {}",
                first_lines(&message)
            ))
            .with_help(
                "No file changed. Make `cargo check --workspace --all-targets` pass, or run \
                 `cargo fallow fix --dry-run` and make the edits by hand.",
            )
        })
}

/// The edition of the package for `rustfmt`.
fn edition_of(workspace: &Workspace, package: &str) -> String {
    workspace
        .members
        .iter()
        .find(|member| member.name == package)
        .and_then(|member| member.targets.first())
        .map_or_else(|| "2021".to_owned(), |target| target.edition.clone())
}

/// The files of the visibility edits that `rustfmt` formats to the same text
/// before the edits, with their edition. Only these files get formatted after
/// the edits, so `fix` does not format a file that the workspace keeps
/// unformatted.
fn formatted_files(
    workspace: &Workspace,
    edits: &[visibility::Edit],
) -> FxHashMap<PathBuf, String> {
    let root = workspace.root.as_std_path();
    let mut files = FxHashMap::default();
    for (path, edits) in visibility::by_file(edits) {
        let full = root.join(path);
        let Ok(text) = fs::read_to_string(&full) else {
            continue;
        };
        let edition = edits.first().map_or_else(
            || "2021".to_owned(),
            |edit| edition_of(workspace, &edit.package),
        );
        let folder = full.parent().unwrap_or(root);
        if workspace::rustfmt(&text, &edition, folder).is_some_and(|out| out == text) {
            files.insert(full, edition);
        }
    }
    files
}

/// Formats each written file that `rustfmt` formatted before the edits. A
/// failure keeps the edited text.
fn format_files(
    workspace: &Workspace,
    planned: &[Planned],
    formatted: &FxHashMap<PathBuf, String>,
) {
    let root = workspace.root.as_std_path();
    for (path, _, new) in planned {
        let Some(edition) = formatted.get(path) else {
            continue;
        };
        let folder = path.parent().unwrap_or(root);
        if let Some(out) = workspace::rustfmt(new, edition, folder)
            && out != *new
        {
            let _ = replace(path, out.as_bytes());
        }
    }
}

/// The `Cargo.lock` of the workspace before the run.
struct LockFile {
    path: PathBuf,
    bytes: Option<Vec<u8>>,
}

impl LockFile {
    fn read(root: &Path) -> Self {
        let path = root.join("Cargo.lock");
        let bytes = fs::read(&path).ok();
        Self { path, bytes }
    }

    /// Writes back the old bytes, or removes a new lock file. A failure here
    /// leaves a lock file that the next cargo run corrects.
    fn restore(&self) {
        let _ = match &self.bytes {
            Some(bytes) => replace(&self.path, bytes),
            None => fs::remove_file(&self.path),
        };
    }
}

/// The path, the old bytes and the new text of one file.
type Planned = (PathBuf, Vec<u8>, String);

/// Reads one file and applies its edits in memory.
fn edited(
    root: &Path,
    path: &str,
    apply: impl FnOnce(&str) -> Result<String, String>,
) -> Result<Planned, CliError> {
    let full = root.join(path);
    let old = fs::read(&full).map_err(|err| unchanged(&format!("cannot read `{path}`: {err}")))?;
    let text =
        String::from_utf8(old.clone()).map_err(|_| unchanged(&format!("`{path}` is not UTF-8")))?;
    let new = apply(&text).map_err(|reason| unchanged(&format!("`{path}`: {reason}")))?;
    Ok((full, old, new))
}

/// A failed write or check.
enum Failed {
    /// Each file got its old bytes again.
    Restored(CliError),
    /// At least one file keeps the edited bytes.
    Stuck(CliError),
}

/// Writes the files and runs `check`. On a failure, writes back the old bytes
/// of each file.
fn write_and_check(
    planned: &[Planned],
    check: impl FnOnce() -> Result<(), String>,
) -> Result<(), Failed> {
    let mut written: Vec<(&Path, &[u8])> = Vec::new();
    let mut outcome = Ok(());
    for (path, old, new) in planned {
        // The list holds the file before the write, so a failed write is
        // restored too.
        written.push((path, old));
        if let Err(err) = replace(path, new.as_bytes()) {
            outcome = Err(format!("cannot write `{}`: {err}", path.display()));
            break;
        }
    }
    if outcome.is_ok() {
        outcome = check();
    }
    let Err(message) = outcome else {
        return Ok(());
    };
    let error = failed(&message);
    let kept: Vec<String> = written
        .into_iter()
        .filter(|(path, old)| replace(path, old).is_err())
        .map(|(path, _)| path.display().to_string())
        .collect();
    if kept.is_empty() {
        return Err(Failed::Restored(error.with_help(RESTORED_HELP)));
    }
    Err(Failed::Stuck(error.with_help(format!(
        "The command cannot write back the old bytes of {}. Undo the edits of these files by \
         hand.",
        kept.join(", ")
    ))))
}

/// The help of a failed run after each file got its old bytes again.
const RESTORED_HELP: &str = "The command wrote back the old bytes of each file. Run \
                             `cargo fallow fix --dry-run` to see the edits.";

/// Writes the bytes to a new file next to `path`, then renames it over
/// `path`. A stop between the two steps leaves the old file.
fn replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // A rename replaces a symlink with a plain file, so a symlink gets a
    // plain write through the link.
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return fs::write(path, bytes);
    }
    let folder = path.parent().unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(folder)?;
    std::io::Write::write_all(&mut file, bytes)?;
    if let Ok(metadata) = fs::metadata(path) {
        file.as_file().set_permissions(metadata.permissions())?;
    }
    file.persist(path).map(|_| ()).map_err(|err| err.error)
}

fn failed(message: &str) -> CliError {
    CliError::new(ErrorCode::FixFailed, message)
}

/// An error before the first write.
fn unchanged(message: &str) -> CliError {
    failed(message).with_help("No file changed.")
}

fn human(report: &FixReport) -> String {
    let verb = if report.dry_run { "planned" } else { "applied" };
    let mut text = format!(
        "cargo fallow fix: {} {} {verb}, {} {} left for a person\n",
        report.fixes.len(),
        if report.fixes.len() == 1 {
            "edit"
        } else {
            "edits"
        },
        report.skipped,
        if report.skipped == 1 {
            "finding"
        } else {
            "findings"
        },
    );
    if report.fixes.is_empty() {
        text.push_str("No edits\n");
    }
    for fix in &report.fixes {
        let _ = writeln!(text, "  {}", line(fix));
    }
    text
}

/// The human line of one edit.
fn line(fix: &Fix) -> String {
    match fix {
        Fix::Dependency(fix) => {
            let table = fix.platform.as_ref().map_or_else(
                || format!("[{}]", fix.section),
                |platform| format!("[target.'{platform}'.{}]", fix.section),
            );
            let place = fix.line.map_or_else(
                || fix.manifest_path.clone(),
                |line| format!("{}:{line}", fix.manifest_path),
            );
            format!("{}: `{}` in {table} ({place})", fix.kind, fix.dependency)
        }
        Fix::Visibility(fix) => {
            let to = if fix.to.is_empty() {
                "private"
            } else {
                &fix.to
            };
            let reason = fix
                .diagnostic
                .as_ref()
                .map_or_else(String::new, |diagnostic| {
                    format!(", left out: {diagnostic}")
                });
            format!(
                "{}: `{}` from `{}` to `{to}` ({}:{}){reason}",
                fix.kind, fix.item, fix.from, fix.path, fix.line
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visibility::report::{CHANGE_ACTION, Range};

    fn edit(item: &str, path: &str) -> visibility::Edit {
        visibility::Edit {
            kind: CHANGE_ACTION,
            rule: "unnecessary-public",
            package: "app".to_owned(),
            item: item.to_owned(),
            path: path.to_owned(),
            line: 1,
            column: 1,
            from: "pub".to_owned(),
            to: "pub(crate)".to_owned(),
            item_kind: "function",
            applied: false,
            skip_reason: None,
            diagnostic: None,
            range: Range {
                line: 1,
                column: 1,
                end_line: 1,
                end_column: 4,
            },
        }
    }

    fn warning(code: &str, message: &str, file: &str) -> check::Diagnostic {
        check::Diagnostic {
            error: false,
            code: code.to_owned(),
            message: message.to_owned(),
            file: file.to_owned(),
            line: 1,
            column: 1,
            spans: Vec::new(),
        }
    }

    #[test]
    fn a_span_picks_the_edit_among_the_edits_of_one_name() {
        let mut first = edit("a::A::new", "src/a.rs");
        first.line = 2;
        let mut second = edit("a::B::new", "src/a.rs");
        second.line = 7;
        let mut kept = vec![first, second];
        let mut dead = warning(
            "dead_code",
            "associated function `new` is never used",
            "src/a.rs",
        );
        dead.spans = vec![("src/a.rs".to_owned(), 7, 7)];
        let left = leave_out(&mut kept, None, &[dead]);
        let items: Vec<&str> = left.iter().map(|edit| edit.item.as_str()).collect();
        assert_eq!(items, ["a::B::new"]);
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn a_warning_leaves_out_only_the_edits_of_its_file_or_of_a_private_type() {
        let mut kept = vec![
            edit("a::x", "src/a.rs"),
            edit("b::x", "src/b.rs"),
            edit("c::Data", "src/c.rs"),
            edit("c::other", "src/c.rs"),
        ];
        let warned = [
            warning("unused_variables", "unused variable: `x`", "src/a.rs"),
            warning(
                "private_interfaces",
                "type `Data` is more private than the item `run`",
                "src/a.rs",
            ),
        ];
        let left = leave_out(&mut kept, None, &warned);
        assert!(
            left.iter()
                .all(|edit| edit.skip_reason == Some(COMPILER_WARNING))
        );
        let items: Vec<&str> = kept.iter().map(|edit| edit.item.as_str()).collect();
        assert_eq!(items, ["b::x", "c::other"]);
    }
}
