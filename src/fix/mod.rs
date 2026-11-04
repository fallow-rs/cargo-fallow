//! The `fix` command: applies the `auto_fixable` actions of the findings.
//!
//! The command runs the `deps` check and edits each `Cargo.toml` with an
//! error finding: it removes an unused entry, or moves a misplaced entry to
//! `[dev-dependencies]`. A warning stays. `--dry-run` writes nothing. After
//! the edits, the command loads the workspace with `cargo metadata` again.
//! When the load fails, the command writes back the old bytes of each file
//! and exits 2.

pub mod deps;

use std::fmt::Write as _;
use std::fs;
use std::io::IsTerminal as _;
use std::path::PathBuf;

use clap::Args;
use fallow_report_protocol::exit_code::EXIT_OK;
use serde::Serialize;

use crate::cli::Format;
use crate::error::{CliError, ErrorCode};
use crate::render::{self, Progress};
use crate::report::TOOL_VERSION;
use crate::section::pretty;
use crate::workspace;

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
    /// Apply the edits without a question. A process without a terminal on
    /// stdin needs this flag.
    #[arg(long, alias = "force")]
    yes: bool,
    /// Give `--locked` to the `cargo metadata` call of the analysis.
    #[arg(long)]
    locked: bool,
    /// Give `--offline` to each `cargo metadata` call.
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
}

/// The report of a `fix` run. The field order is the wire order.
#[derive(Debug, Clone, Serialize)]
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
    /// The edits, applied or planned.
    pub fixes: Vec<deps::Edit>,
    /// The number of edits with `applied: true`.
    pub total_fixed: usize,
    /// The findings that `fix` does not change, for example a warning.
    pub skipped: usize,
}

/// Runs `fix` and returns the process exit code.
pub fn run(args: &FixArgs) -> Result<u8, CliError> {
    if matches!(args.format, Format::Sarif | Format::Codeclimate) {
        return Err(CliError::new(
            ErrorCode::InvalidArgument,
            "`fix` has no SARIF or CodeClimate output",
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
    let flags = |locked: bool| {
        [
            (locked && args.locked, "--locked"),
            (args.offline || args.frozen, "--offline"),
            (locked && args.frozen, "--frozen"),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, flag)| flag.to_owned())
        .collect::<Vec<_>>()
    };
    let report = crate::deps::run(&crate::deps::Options {
        manifest_path: args.manifest_path.clone(),
        format: args.format,
        quiet: args.quiet,
        fail_on_issues: false,
        cargo_flags: flags(true),
    })?;
    let mut fixes = deps::plan(&report);
    let skipped = report.findings.len() - fixes.len();
    if !args.dry_run && !fixes.is_empty() {
        let progress = Progress::new(args.quiet);
        progress.step("Editing the manifests");
        // The edits change `Cargo.lock`, so the check after the edits cannot
        // use `--locked` or `--frozen`.
        write_and_check(args.manifest_path.as_deref(), &fixes, &flags(false))?;
        for fix in &mut fixes {
            fix.applied = true;
        }
    }
    let total_fixed = fixes.iter().filter(|fix| fix.applied).count();
    let report = FixReport {
        schema_version: SCHEMA_VERSION,
        version: TOOL_VERSION,
        kind: "fix",
        language: "rust",
        dry_run: args.dry_run,
        fixes,
        total_fixed,
        skipped,
    };
    let text = match args.format {
        Format::Json => pretty(&report)?,
        _ => human(&report),
    };
    render::write_stdout(text.trim_end()).map_err(|err| CliError::output(&err))?;
    Ok(EXIT_OK)
}

/// Writes the edits and loads the workspace again. On a failure, writes back
/// the old bytes of each file.
fn write_and_check(
    manifest_path: Option<&std::path::Path>,
    fixes: &[deps::Edit],
    flags: &[String],
) -> Result<(), CliError> {
    let workspace = workspace::load(manifest_path)?;
    let root = workspace.root.as_std_path();
    let mut planned = Vec::new();
    for (manifest, edits) in deps::by_manifest(fixes) {
        let path = root.join(manifest);
        let old =
            fs::read(&path).map_err(|err| failed(&format!("cannot read `{manifest}`: {err}")))?;
        let text = String::from_utf8(old.clone())
            .map_err(|_| failed(&format!("`{manifest}` is not UTF-8")))?;
        let new = deps::apply(&text, &edits)
            .map_err(|reason| failed(&format!("`{manifest}`: {reason}")))?;
        planned.push((path, old, new));
    }
    let mut written: Vec<(&PathBuf, &Vec<u8>)> = Vec::new();
    let mut outcome = Ok(());
    for (path, old, new) in &planned {
        if let Err(err) = fs::write(path, new) {
            outcome = Err(failed(&format!("cannot write `{}`: {err}", path.display())));
            break;
        }
        written.push((path, old));
    }
    if outcome.is_ok() {
        outcome = workspace::load_resolved(manifest_path, flags)
            .map(|_| ())
            .map_err(|err| {
                failed(&format!(
                    "the workspace does not load after the edits: {}",
                    err.message
                ))
            });
    }
    if outcome.is_err() {
        for (path, old) in written {
            // A failed write back leaves the file with the edit. The error
            // names the edit, so a person can undo it.
            let _ = fs::write(path, old);
        }
    }
    outcome
}

fn failed(message: &str) -> CliError {
    CliError::new(ErrorCode::FixFailed, message).with_help(
        "The command wrote back the old bytes of each file. Run `cargo fallow fix --dry-run` \
         to see the edits.",
    )
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
        let table = fix.platform.as_ref().map_or_else(
            || format!("[{}]", fix.section),
            |platform| format!("[target.'{platform}'.{}]", fix.section),
        );
        let place = fix.line.map_or_else(
            || fix.manifest_path.clone(),
            |line| format!("{}:{line}", fix.manifest_path),
        );
        let _ = writeln!(
            text,
            "  {}: `{}` in {table} ({place})",
            fix.kind, fix.dependency
        );
    }
    text
}
