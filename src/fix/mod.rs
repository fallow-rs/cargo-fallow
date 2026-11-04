//! The `fix` command: applies the `auto_fixable` actions of the findings.
//!
//! The command runs the `deps` and the `visibility` checks. It edits each
//! `Cargo.toml` with an error finding: it removes an unused entry, or moves a
//! misplaced entry to `[dev-dependencies]`. It narrows the visibility keyword
//! of each `visibility` finding. A warning stays. `--dry-run` writes nothing.
//!
//! After the edits, the command loads the workspace with `cargo metadata`
//! again. With a visibility edit, it also runs `cargo check --workspace
//! --all-targets`, before and after the edits. When a step after the edits
//! fails, the command writes back the old bytes of each file and exits 2.

pub mod deps;
pub mod visibility;

use std::fmt::Write as _;
use std::fs;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

use clap::Args;
use fallow_report_protocol::exit_code::EXIT_OK;
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
    #[arg(long, value_enum, default_value_t = Format::Human)]
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
    /// Give `--locked` to the `cargo metadata` call of the analysis.
    #[arg(long)]
    locked: bool,
    /// Give `--offline` to each cargo call.
    #[arg(long)]
    offline: bool,
    /// Give `--frozen` to the `cargo metadata` call of the analysis.
    #[arg(long)]
    frozen: bool,
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
    /// The findings that `fix` does not change, for example a warning.
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
    skipped: usize,
    skipped_packages: Vec<crate::deps::report::SkippedPackage>,
}

/// Runs `fix` and returns the process exit code.
pub fn run(args: &FixArgs) -> Result<u8, CliError> {
    if matches!(
        args.format,
        Format::Sarif | Format::Codeclimate | Format::Badge
    ) {
        return Err(CliError::new(
            ErrorCode::InvalidArgument,
            "`fix` has no SARIF, CodeClimate or badge output",
        )
        .with_help("Use `--format human` or `--format json`."));
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
        let kept = apply_all(args, &workspace, &plan, progress)?;
        for fix in &mut fixes {
            let applied = match fix {
                Fix::Dependency(_) => true,
                Fix::Visibility(edit) => {
                    kept.iter().any(|other| visibility::same_place(edit, other))
                }
            };
            if applied {
                fix.mark_applied();
            }
        }
        skipped += plan.visibility.len() - kept.len();
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

/// Runs both checks on one module graph and plans their edits.
fn plan(
    args: &FixArgs,
    workspace: &Workspace,
    metadata: &cargo_metadata::Metadata,
    progress: Progress,
) -> Result<Plan, CliError> {
    let options = crate::visibility::config::resolve(
        workspace,
        &VisibilityOptions {
            manifest_path: args.manifest_path.clone(),
            format: args.format,
            quiet: args.quiet,
            production: Vec::new(),
            exclude: Vec::new(),
            uniform_fields: false,
            kinds: Vec::new(),
            fail_on_issues: false,
            scope: crate::scope::ScopeArgs::default(),
            precise: None,
        },
    )?;
    progress.step("Building the module graph");
    // Only the visibility check reads the production selection.
    let production = crate::graph::Production {
        bins: options.production.clone(),
    };
    let module_graph = crate::graph::build(workspace, &production);
    progress.step("Checking the dependencies");
    let scope = Scope::default();
    let report = crate::deps::report(workspace, metadata, &module_graph, false, &scope);
    let deps_edits = deps::plan(&report);
    progress.step("Checking the visibility");
    let visibility_report =
        crate::visibility::fast_report(&module_graph, &options, workspace, &scope);
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
    Ok(Plan {
        skipped: report.findings.len().saturating_sub(deps_edits.len())
            + visibility_report
                .findings
                .len()
                .saturating_sub(fixable_rows),
        deps: deps_edits,
        visibility: visibility_edits,
        skipped_packages: report.skipped,
    })
}

/// Writes the edits and checks the workspace. On a failure after the first
/// write, writes back the old bytes of each file.
fn apply_all(
    args: &FixArgs,
    workspace: &Workspace,
    plan: &Plan,
    progress: Progress,
) -> Result<Vec<visibility::Edit>, CliError> {
    let root = workspace.root.as_std_path();
    let manifest = args.manifest_path.as_deref();
    let flags = args.cargo_flags(false);
    let compiles = !plan.visibility.is_empty();
    // `cargo check` can write `Cargo.lock`, so a failed run writes back its
    // old bytes too.
    let lock = LockFile::read(root);
    let all_features = if compiles {
        progress.step("Checking that the workspace builds before the edits");
        Some(baseline(manifest, root, &args.cargo_flags(true)).inspect_err(|_| lock.restore())?)
    } else {
        None
    };
    let mut kept = plan.visibility.clone();
    for round in 1..=CHECK_ROUNDS {
        let mut planned = Vec::new();
        for (path, edits) in deps::by_manifest(&plan.deps) {
            planned.push(edited(root, path, |text| deps::apply(text, &edits))?);
        }
        for (path, edits) in visibility::by_file(&kept) {
            planned.push(edited(root, path, |text| visibility::apply(text, &edits))?);
        }
        progress.step("Writing the edits");
        let mut compiler = None;
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
                workspace::cargo_check(manifest, root, &flags, all_features).map_err(
                    |message| {
                        let text = format!(
                            "the workspace does not build after the edits: {}",
                            first_lines(&message)
                        );
                        compiler = Some(message);
                        text
                    },
                )?;
            }
            Ok(())
        });
        let Err(error) = outcome else {
            return Ok(kept);
        };
        // The compiler names the items that the edits broke. Each round
        // leaves out the edits of those items, and the next round starts from
        // the old bytes again.
        let before = kept.len();
        if let Some(message) = compiler.filter(|_| round < CHECK_ROUNDS) {
            let names = visibility::named_items(&message);
            kept.retain(|edit| !names.contains(visibility::last_segment(&edit.item)));
        }
        if kept.len() == before || (kept.is_empty() && plan.deps.is_empty()) {
            lock.restore();
            return Err(error);
        }
        progress.step("Leaving out the edits that the compiler names, and checking again");
    }
    lock.restore();
    Err(failed("the workspace does not build after the edits"))
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

/// Checks that the workspace builds before the edits, with all features when
/// it can, as the analysis reads it. Returns true for an all-features build.
fn baseline(manifest: Option<&Path>, root: &Path, flags: &[String]) -> Result<bool, CliError> {
    if workspace::cargo_check(manifest, root, flags, true).is_ok() {
        return Ok(true);
    }
    workspace::cargo_check(manifest, root, flags, false)
        .map(|()| false)
        .map_err(|message| {
            unchanged(&format!(
                "the workspace does not build before the edits, so `fix` cannot judge them: \
                 {}",
                first_lines(&message)
            ))
        })
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

/// Writes the files and runs `check`. On a failure, writes back the old bytes
/// of each file.
fn write_and_check(
    planned: &[Planned],
    check: impl FnOnce() -> Result<(), String>,
) -> Result<(), CliError> {
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
        return Err(error.with_help(RESTORED_HELP));
    }
    Err(error.with_help(format!(
        "The command cannot write back the old bytes of {}. Undo the edits of these files by \
         hand.",
        kept.join(", ")
    )))
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
            format!(
                "{}: `{}` from `{}` to `{to}` ({}:{})",
                fix.kind, fix.item, fix.from, fix.path, fix.line
            )
        }
    }
}
