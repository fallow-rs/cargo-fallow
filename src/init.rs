//! The `init` command: a starting configuration for a workspace.
//!
//! The command runs `check` and gives the tables of
//! `[workspace.metadata.fallow]` with the limits at the current maximum of the
//! workspace. With `--write`, it saves the findings of a run with the new
//! limits as a baseline, so the first `cargo fallow check --fail-on-issues`
//! passes, and a new finding later fails it. `--write` adds the tables to the root `Cargo.toml` with
//! `toml_edit` and writes the baseline file. A key that the manifest has
//! already keeps its value, so a second `--write` changes nothing. The printed
//! tables are the tables of the manifest after the edit.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use fallow_report_protocol::exit_code::EXIT_OK;
use serde::Serialize;
use serde_json::Value;
use toml_edit::{DocumentMut, Item, Table, value};

use crate::cli::Format;
use crate::error::{CliError, ErrorCode};
use crate::render::{self, Progress};
use crate::report::TOOL_VERSION;
use crate::section::pretty;
use crate::workspace;

/// The name of the baseline file in the workspace root.
pub const BASELINE_FILE: &str = "fallow-baseline.json";

/// The default limits of the checks, from the configuration modules.
const DEFAULT_CYCLOMATIC: u32 = 20;
const DEFAULT_COGNITIVE: u32 = 15;

/// A CI system for `--ci`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Ci {
    /// A GitHub Actions workflow that uses the cargo-fallow action.
    Github,
}

/// The flags of `init`.
#[derive(Debug, Clone, Args)]
pub struct InitArgs {
    /// Path to the Cargo.toml of the workspace.
    #[arg(long, value_name = "PATH")]
    manifest_path: Option<PathBuf>,
    /// The output format: `human` or `json`.
    #[arg(long, default_value = "human", value_parser = crate::cli::human_or_json())]
    format: Format,
    /// Hide the progress lines on stderr. The report on stdout does not change.
    #[arg(long)]
    quiet: bool,
    /// Add the tables to the root Cargo.toml and write the baseline file. A key
    /// that the manifest has keeps its value.
    #[arg(long)]
    write: bool,
    /// Print a CI workflow file that runs the checks, instead of the tables.
    #[arg(long, value_enum, value_name = "SYSTEM")]
    ci: Option<Ci>,
    /// Set up the repository for coding agents instead: the same as `cargo
    /// fallow agent install`.
    #[arg(long, conflicts_with_all = ["write", "ci"])]
    agents: bool,
    #[command(flatten)]
    cargo: crate::cli::CargoArgs,
}

impl InitArgs {
    /// The output format.
    pub const fn format(&self) -> Format {
        self.format
    }
}

/// The report of `init`. The field order is the wire order.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct InitReport {
    /// Always `init`.
    pub kind: &'static str,
    /// The version of cargo-fallow.
    pub version: &'static str,
    /// The `[workspace.metadata.fallow]` tables of the root manifest after
    /// `--write`, as TOML text, also in a run without `--write`.
    pub tables: String,
    /// The baseline file, relative to the workspace root.
    pub baseline: String,
    /// The number of entries in the baseline file that this run wrote, or
    /// `null` when the run wrote no baseline. A clone group has one entry.
    pub baseline_findings: Option<usize>,
    /// True when the command changed the manifest or wrote the baseline.
    pub written: bool,
}

/// The limits at the current maximum of the workspace. The table has no
/// `max-file-lines`: a larger file limit only turns off the growth rule of
/// `size`, and the default limit gives no finding without `--base`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Limits {
    max_cyclomatic: u32,
    max_cognitive: u32,
    threshold: f64,
}

/// Runs `init` and returns the process exit code.
pub fn run(args: &InitArgs) -> Result<u8, CliError> {
    if args.agents {
        return crate::agent::install(&crate::agent::InstallArgs::defaults(
            args.manifest_path.clone(),
            args.format,
        ));
    }
    if args.ci == Some(Ci::Github) {
        render::write_stdout(github_workflow().trim_end()).map_err(|err| CliError::output(&err))?;
        return Ok(EXIT_OK);
    }
    let workspace = workspace::load(args.manifest_path.as_deref(), &args.cargo.flags())?;
    let root = workspace.root.as_std_path();
    let progress = Progress::new(args.quiet);
    progress.step("Running each static check");
    let probe = temp_file()?;
    let first = run_check(args, probe.path())?;
    let json = serde_json::to_value(&first)
        .map_err(|err| CliError::output(&std::io::Error::other(err)))?;
    let limits = limits(&json);
    let merged = merge(root, &limits)?;
    let tables = fallow_tables(&merged.doc);
    let mut findings = None;
    let mut written = false;
    let baseline = configured_baseline(&merged.doc)?;
    if args.write {
        written = merged.write(root)?;
        let path = root.join(&baseline);
        if fs::symlink_metadata(&path).is_ok() {
            render::warn(&format!(
                "kept the baseline `{baseline}` that exists. Run `cargo fallow check \
                 --save-baseline {baseline}` to save the current findings."
            ));
        } else {
            // The baseline comes from a run with the new limits, so it holds
            // no entry that the new limits make stale.
            progress.step("Saving the findings with the new limits");
            let saved = temp_file()?;
            run_check(args, saved.path())?;
            findings = Some(baseline_entries(saved.path())?);
            write_baseline(&path, saved.path())?;
            written = true;
        }
    }
    let report = InitReport {
        kind: "init",
        version: TOOL_VERSION,
        tables,
        baseline,
        baseline_findings: findings,
        written,
    };
    let text = match args.format {
        Format::Json => pretty(&report)?,
        _ => human(&report, args.write),
    };
    render::write_stdout(text.trim_end()).map_err(|err| CliError::output(&err))?;
    Ok(EXIT_OK)
}

/// The limits at the current maximum of the `check` report, never under the
/// defaults.
fn limits(report: &Value) -> Limits {
    let sections = &report["sections"];
    let max_of = |key: &str| {
        sections["complexity"]["findings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|finding| finding[key].as_u64())
            .max()
            .and_then(|max| u32::try_from(max).ok())
    };
    let percentage = sections["dupes"]["summary"]["stats"]["duplication_percentage"]
        .as_f64()
        .unwrap_or(0.0);
    Limits {
        max_cyclomatic: max_of("cyclomatic").unwrap_or(0).max(DEFAULT_CYCLOMATIC),
        max_cognitive: max_of("cognitive").unwrap_or(0).max(DEFAULT_COGNITIVE),
        // One decimal up, so the current percentage passes.
        threshold: (percentage * 10.0).ceil() / 10.0,
    }
}

/// The `[workspace.metadata.fallow]` tables of `doc` as TOML text, in the
/// order of the manifest.
fn fallow_tables(doc: &DocumentMut) -> String {
    let mut out = DocumentMut::new();
    if let Some(fallow) = doc
        .get("workspace")
        .and_then(|item| item.get("metadata"))
        .and_then(|item| item.get("fallow"))
    {
        let mut metadata = Table::new();
        metadata.set_implicit(true);
        metadata.insert("fallow", fallow.clone());
        let mut workspace = Table::new();
        workspace.set_implicit(true);
        workspace.insert("metadata", Item::Table(metadata));
        out.insert("workspace", Item::Table(workspace));
    }
    let mut text = out.to_string().trim().to_owned();
    text.push('\n');
    text
}

/// A new temporary file.
fn temp_file() -> Result<tempfile::NamedTempFile, CliError> {
    tempfile::NamedTempFile::new().map_err(|err| {
        CliError::new(
            ErrorCode::OutputFailed,
            format!("cannot make a file: {err}"),
        )
    })
}

/// Runs `check` with no gate and saves its findings to `save_to`. The
/// `--save-baseline` flag also keeps the run from reading a configured
/// baseline.
fn run_check(
    args: &InitArgs,
    save_to: &Path,
) -> Result<crate::check::report::CheckReport, CliError> {
    crate::check::execute(&crate::check::Options {
        manifest_path: args.manifest_path.clone(),
        format: Format::Json,
        quiet: true,
        only: Vec::new(),
        skip: Vec::new(),
        fail_on_issues: false,
        fail_on_parse_error: false,
        base: None,
        staged: false,
        health: crate::hotspots::HealthArgs::default(),
        coverage: crate::structure::crap::CoverageArgs::default(),
        cargo_flags: args.cargo.flags(),
        scope: crate::scope::ScopeArgs {
            save_baseline: Some(save_to.to_path_buf()),
            ..crate::scope::ScopeArgs::default()
        },
    })
}

/// The root manifest with the tables of `init` added.
struct Merged {
    /// The text of the manifest before the edit.
    before: String,
    /// The manifest after the edit.
    doc: DocumentMut,
}

impl Merged {
    /// Writes the manifest when the edit changed it. Returns true when the
    /// manifest changed.
    fn write(&self, root: &Path) -> Result<bool, CliError> {
        let new = self.doc.to_string();
        if new == self.before {
            return Ok(false);
        }
        let manifest = root.join("Cargo.toml");
        fs::write(&manifest, new).map_err(|err| failed(&manifest, &err))?;
        Ok(true)
    }
}

/// Adds the tables to the root manifest in memory. A key that exists keeps
/// its value.
fn merge(root: &Path, limits: &Limits) -> Result<Merged, CliError> {
    let manifest = root.join("Cargo.toml");
    let before = fs::read_to_string(&manifest).map_err(|err| failed(&manifest, &err))?;
    let mut doc: DocumentMut = before.parse().map_err(|err| {
        CliError::new(
            ErrorCode::InvalidConfig,
            format!("the root Cargo.toml does not parse: {err}"),
        )
    })?;
    let fallow = table(&mut doc, &["workspace", "metadata", "fallow"])?;
    keep_or_set(fallow, "baseline", value(BASELINE_FILE));
    let complexity = table(&mut doc, &["workspace", "metadata", "fallow", "complexity"])?;
    keep_or_set(
        complexity,
        "max-cyclomatic",
        value(i64::from(limits.max_cyclomatic)),
    );
    keep_or_set(
        complexity,
        "max-cognitive",
        value(i64::from(limits.max_cognitive)),
    );
    let dupes = table(&mut doc, &["workspace", "metadata", "fallow", "dupes"])?;
    keep_or_set(dupes, "threshold", value(limits.threshold));
    Ok(Merged { before, doc })
}

/// The number of entries in the baseline file at `path`. A clone group has
/// one entry, so the number can be lower than the number of findings.
fn baseline_entries(path: &Path) -> Result<usize, CliError> {
    let text = fs::read_to_string(path).map_err(|err| failed(path, &err))?;
    let file: crate::baseline::File =
        serde_json::from_str(&text).map_err(|err| CliError::output(&std::io::Error::other(err)))?;
    Ok(file.findings.len())
}

/// The `baseline` key of the root manifest after the edit.
fn configured_baseline(doc: &DocumentMut) -> Result<String, CliError> {
    let path = doc
        .get("workspace")
        .and_then(|item| item.get("metadata"))
        .and_then(|item| item.get("fallow"))
        .and_then(|item| item.get("baseline"))
        .and_then(Item::as_str)
        .unwrap_or(BASELINE_FILE)
        .to_owned();
    crate::scope::check_baseline_key(&path)?;
    Ok(path)
}

/// Writes the saved findings to a new baseline file. `create_new` does not
/// follow a link at the path.
fn write_baseline(path: &Path, saved: &Path) -> Result<(), CliError> {
    let bytes = fs::read(saved).map_err(|err| failed(saved, &err))?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|err| failed(path, &err))?;
    std::io::Write::write_all(&mut file, &bytes).map_err(|err| failed(path, &err))
}

/// The table at `path`, made when it is missing. The parent tables stay
/// implicit, so the manifest gets no empty `[workspace.metadata]` header.
fn table<'a>(doc: &'a mut DocumentMut, path: &[&str]) -> Result<&'a mut Table, CliError> {
    let mut current = doc.as_table_mut();
    for (depth, key) in path.iter().enumerate() {
        let last = depth + 1 == path.len();
        let item = current.entry(key).or_insert_with(|| {
            let mut table = Table::new();
            table.set_implicit(!last);
            Item::Table(table)
        });
        current = item.as_table_mut().ok_or_else(|| {
            CliError::new(
                ErrorCode::InvalidConfig,
                format!(
                    "`{}` in the root Cargo.toml is not a table",
                    path[..=depth].join(".")
                ),
            )
        })?;
    }
    Ok(current)
}

/// Sets a key that the table does not have.
fn keep_or_set(table: &mut Table, key: &str, item: Item) {
    if !table.contains_key(key) {
        table.insert(key, item);
    }
}

fn failed(path: &Path, err: &std::io::Error) -> CliError {
    CliError::new(
        ErrorCode::OutputFailed,
        format!("cannot write `{}`: {err}", path.display()),
    )
}

fn human(report: &InitReport, write: bool) -> String {
    let mut text = String::new();
    if write {
        let what = if report.written {
            "added the tables to Cargo.toml and wrote the baseline"
        } else {
            "changed nothing: Cargo.toml and the baseline are up to date"
        };
        let _ = writeln!(text, "cargo fallow init: {what}\n");
    } else {
        let _ = writeln!(
            text,
            "# The tables after `cargo fallow init --write`. A key that Cargo.toml has keeps its value."
        );
    }
    text.push_str(&report.tables);
    let baseline = &report.baseline;
    let _ = match (write, report.baseline_findings) {
        (_, Some(findings)) => writeln!(
            text,
            "\n# The baseline `{baseline}` holds {findings} entries for the current findings, so the\n\
             # first `cargo fallow check --fail-on-issues` passes. A new finding fails it."
        ),
        (true, None) => writeln!(
            text,
            "\n# The baseline `{baseline}` exists, so the command kept it as it was."
        ),
        (false, None) => writeln!(
            text,
            "\n# `cargo fallow init --write` also saves the current findings to `{baseline}`,\n\
             # so the first `cargo fallow check --fail-on-issues` passes. A new finding fails it."
        ),
    };
    text
}

/// A GitHub Actions workflow that runs `check` on each pull request and push.
fn github_workflow() -> String {
    "# .github/workflows/cargo-fallow.yml
# Pin each action to a commit before you use this file.
name: cargo-fallow
on:
  pull_request:
  push:
    branches: [main]
permissions:
  contents: read
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          persist-credentials: false
          fetch-depth: 0
      - uses: dtolnay/rust-toolchain@stable
      - uses: fallow-rs/cargo-fallow@v0
        with:
          command: check
"
    .to_owned()
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "a test fails loudly")]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        max_cyclomatic: 31,
        max_cognitive: 15,
        threshold: 3.5,
    };

    #[test]
    fn the_printed_tables_parse_and_hold_the_limits() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        fs::write(dir.path().join("Cargo.toml"), "[workspace]\nmembers = []\n")
            .expect("write the manifest");
        let merged = merge(dir.path(), &LIMITS).expect("the merge works");
        let doc: DocumentMut = fallow_tables(&merged.doc)
            .parse()
            .expect("the tables parse");
        let fallow = &doc["workspace"]["metadata"]["fallow"];
        assert_eq!(fallow["baseline"].as_str(), Some(BASELINE_FILE));
        assert_eq!(
            fallow["complexity"]["max-cyclomatic"].as_integer(),
            Some(31)
        );
        assert!(fallow.get("size").is_none());
        assert_eq!(fallow["dupes"]["threshold"].as_float(), Some(3.5));
        assert!(doc.get("package").is_none());
    }

    #[test]
    fn limits_take_the_maximum_and_keep_the_defaults() {
        let report = serde_json::json!({
            "sections": {
                "complexity": { "findings": [{ "cyclomatic": 31, "cognitive": 9 }] },
                "size": { "summary": { "large_files": [{ "lines": 2400 }] } },
                "dupes": { "summary": { "stats": { "duplication_percentage": 3.42 } } },
            }
        });
        assert_eq!(limits(&report), LIMITS);
        let empty = limits(&serde_json::json!({}));
        assert_eq!(empty.max_cyclomatic, DEFAULT_CYCLOMATIC);
        assert!(empty.threshold.abs() < f64::EPSILON);
    }

    #[test]
    fn a_second_write_changes_nothing_and_a_kept_key_stays() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let manifest = "[workspace]\nmembers = []\n\n[workspace.metadata.fallow.complexity]\nmax-cyclomatic = 50 # kept\n";
        fs::write(dir.path().join("Cargo.toml"), manifest).expect("write the manifest");
        let write = || {
            merge(dir.path(), &LIMITS)
                .and_then(|merged| merged.write(dir.path()))
                .expect("the write works")
        };
        assert!(write());
        let first = fs::read_to_string(dir.path().join("Cargo.toml")).expect("read the manifest");
        assert!(first.contains("max-cyclomatic = 50 # kept"), "{first}");
        assert!(first.contains("max-cognitive = 15"), "{first}");
        assert!(!first.contains("[workspace.metadata]\n"), "{first}");
        assert!(!write());
        let second = fs::read_to_string(dir.path().join("Cargo.toml")).expect("read the manifest");
        assert_eq!(first, second);
        assert_eq!(
            configured_baseline(&merge(dir.path(), &LIMITS).expect("a merge").doc).expect("a key"),
            BASELINE_FILE
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_link_at_the_baseline_path_is_not_followed() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let saved = dir.path().join("saved.json");
        fs::write(&saved, "{}").expect("write a baseline");
        let target = dir.path().join("elsewhere.txt");
        let link = dir.path().join(BASELINE_FILE);
        std::os::unix::fs::symlink(&target, &link).expect("make a link");
        assert!(write_baseline(&link, &saved).is_err());
        assert!(!target.exists(), "the write followed the link");
    }
}
