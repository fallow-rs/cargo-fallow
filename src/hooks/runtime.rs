//! `cargo fallow hooks gate-runtime`: the work of the agent gate.
//!
//! The gate script in `.claude/hooks/` or `.codex/hooks/` sends the payload of
//! the harness on stdin to `gate-runtime run`. The runtime finds the git
//! write, runs `check` on the changed files of each workspace that the write
//! can reach, and blocks when an enforced gate fails.
//!
//! The exit code is the protocol with the script: 0 is a pass, 1 is a block,
//! and each other code is an error. The script keeps the gate open on an
//! error. A runtime error also gives exit 0 with one notice line on stderr,
//! so that each skip stays visible.

use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::classify::{GitWrite, WriteKind, git_write};
use super::report::{self, ReportTarget};
use crate::check::{self, report::CheckReport};
use crate::cli::Format;
use crate::diff::git;
use crate::finding::{Finding, Severity};
use crate::report::TOOL_VERSION;

/// The name of the protocol between the gate script and the runtime. The
/// probe prints it with the version.
pub const PROTOCOL: &str = "cargo-fallow-gate-v1";

/// The exit code of a block.
const EXIT_BLOCK: u8 = 1;

/// The largest payload that the runtime reads.
const MAX_PAYLOAD_BYTES: u64 = 1024 * 1024;

/// The most findings that the block message lists. The report holds each one.
const MAX_LISTED_FINDINGS: usize = 50;

/// The prefix of each line that the gate writes.
const PREFIX: &str = "cargo-fallow-gate";

/// The name of the manifest file.
const MANIFEST: &str = "Cargo.toml";

/// The line on stdout that tells the script to block. A block needs this
/// line and exit code 1.
pub const BLOCK_LINE: &str = "cargo-fallow-gate-v1 block";

/// The variable with the manifest that install recorded, for a session
/// folder with no `Cargo.toml` above it.
pub const MANIFEST_VARIABLE: &str = "CARGO_FALLOW_GATE_MANIFEST";

/// The operations of `gate-runtime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Operation {
    /// Print the protocol name and the version.
    Probe,
    /// Read the payload of an agent harness on stdin and run the gate.
    Run,
    /// Run the commit gate in the current folder, for a git pre-commit hook.
    PreCommit,
}

#[derive(Default, Deserialize)]
struct ToolInput {
    command: Option<String>,
}

#[derive(Deserialize)]
struct Payload {
    cwd: Option<String>,
    tool_input: Option<ToolInput>,
}

/// Runs one operation and returns the exit code.
pub fn run(operation: Operation) -> u8 {
    match operation {
        Operation::Probe => {
            // A probe that cannot write gives no protocol line, which the
            // script reads as a binary without the runtime.
            let _ = writeln!(io::stdout().lock(), "{PROTOCOL} {TOOL_VERSION}");
            0
        }
        Operation::Run => {
            one_line_panics();
            let outcome = gate(&mut io::stdin().lock());
            finish(&outcome, true)
        }
        Operation::PreCommit => {
            one_line_panics();
            let session = session_dir(None);
            let write = GitWrite {
                kinds: vec![WriteKind::Commit],
                targets: Vec::new(),
            };
            finish(&decide_write(&session, &write), false)
        }
    }
}

/// Writes the outcome and returns the exit code. `block_line` is true for
/// the agent script, which reads the block line on stdout.
fn finish(outcome: &Outcome, block_line: bool) -> u8 {
    // A failed write to stderr does not change the decision.
    let _ = io::stderr().lock().write_all(outcome.message.as_bytes());
    if !outcome.block {
        return 0;
    }
    if block_line && writeln!(io::stdout().lock(), "{BLOCK_LINE}").is_err() {
        return 0;
    }
    EXIT_BLOCK
}

/// Makes a panic print one line. The script keeps the gate open on the exit
/// code of the panic.
fn one_line_panics() {
    std::panic::set_hook(Box::new(|info| {
        let place = info
            .location()
            .map(|place| format!(" at {}:{}", place.file(), place.line()))
            .unwrap_or_default();
        let _ = writeln!(
            io::stderr().lock(),
            "{PREFIX}: internal error{place}, skipping the check."
        );
    }));
}

/// The decision of the gate and the text for stderr.
#[derive(Debug, Default)]
pub struct Outcome {
    /// True when the gate blocks the command.
    pub block: bool,
    /// The lines for stderr, each with a newline.
    pub message: String,
}

impl Outcome {
    fn notice(text: &str) -> Self {
        Self {
            block: false,
            message: format!("{PREFIX}: {}\n", clean(text)),
        }
    }
}

/// Reads the payload and decides.
pub fn gate(input: &mut impl Read) -> Outcome {
    let payload = match read_payload(input) {
        Ok(payload) => payload,
        Err(detail) => return Outcome::notice(&format!("{detail}, skipping the check.")),
    };
    let command = payload
        .tool_input
        .unwrap_or_default()
        .command
        .unwrap_or_default();
    let Some(write) = git_write(&command) else {
        return Outcome::default();
    };
    let session = session_dir(payload.cwd.as_deref());
    decide_write(&session, &write)
}

/// Checks each folder that the write can reach, and decides.
fn decide_write(session: &Path, write: &GitWrite) -> Outcome {
    let mut notices = Vec::new();
    let mut blocks = Vec::new();
    let mut roots: Vec<PathBuf> = Vec::new();
    for dir in audit_dirs(session, write) {
        let Some(manifest) = nearest_manifest(&dir).or_else(recorded_manifest) else {
            continue;
        };
        match audit(&manifest, write, &mut roots) {
            Audit::Pass | Audit::Nothing => {}
            Audit::Skipped(text) => notices.push(text),
            Audit::Block(text) => blocks.push(text),
        }
    }
    if !blocks.is_empty() {
        return Outcome {
            block: true,
            message: blocks.concat(),
        };
    }
    if notices.is_empty() {
        return Outcome::default();
    }
    Outcome::notice(&notices.join("; "))
}

/// Reads and parses the payload. The error names the problem.
fn read_payload(input: &mut impl Read) -> Result<Payload, String> {
    let mut bytes = Vec::new();
    input
        .take(MAX_PAYLOAD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("cannot read the tool input ({err})"))?;
    if bytes.len() as u64 > MAX_PAYLOAD_BYTES {
        return Err(format!(
            "the tool input is over the limit of {MAX_PAYLOAD_BYTES} bytes"
        ));
    }
    serde_json::from_slice(&bytes).map_err(|err| format!("invalid tool input ({err})"))
}

/// The session folder: the `cwd` of the payload when it is a folder, else the
/// folder of the process.
fn session_dir(cwd: Option<&str>) -> PathBuf {
    cwd.filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The folders to check: the session folder, then each target of the write
/// that is a folder. Each folder is a canonical path, without repeats.
fn audit_dirs(session: &Path, write: &GitWrite) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let candidates =
        std::iter::once(session.to_path_buf()).chain(write.targets.iter().map(|target| {
            let target = Path::new(target);
            if target.is_absolute() {
                target.to_path_buf()
            } else {
                session.join(target)
            }
        }));
    for candidate in candidates {
        if let Ok(dir) = candidate.canonicalize()
            && dir.is_dir()
            && !dirs.contains(&dir)
        {
            dirs.push(dir);
        }
    }
    dirs
}

/// The result of the check of one folder.
enum Audit {
    Pass,
    /// The workspace is not one to check: an earlier audit of the command
    /// checked it, or it is the home folder.
    Nothing,
    /// The check did not run. The text says why.
    Skipped(String),
    /// The check failed. The text is the block message.
    Block(String),
}

/// The manifest that install recorded, when it is a file.
fn recorded_manifest() -> Option<PathBuf> {
    std::env::var_os(MANIFEST_VARIABLE)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_file())
}

/// Checks the workspace of one manifest. `roots` holds the workspace roots
/// that an earlier audit of the same command checked.
fn audit(manifest: &Path, write: &GitWrite, roots: &mut Vec<PathBuf>) -> Audit {
    let workspace = match crate::workspace::load(Some(manifest), &cargo_flags()) {
        Ok(workspace) => workspace,
        Err(err) => return Audit::Skipped(format!("cargo metadata failed ({})", err.message)),
    };
    let root = workspace.root.as_std_path().to_path_buf();
    if roots.contains(&root) || home_dir().is_some_and(|home| home == root) {
        return Audit::Nothing;
    }
    roots.push(root.clone());
    let (reference, commit) = match base_ref(&root, write) {
        Ok(pair) => pair,
        Err(text) => return Audit::Skipped(text),
    };
    let mut options = check_options(&root, write, &reference);
    match super::base::baseline(&root, &commit, &options) {
        Ok(path) => options.scope.baseline = Some(path),
        Err(text) => return Audit::Skipped(text),
    }
    let report = match check::execute(&options) {
        Ok(report) => report,
        Err(err) => {
            return Audit::Skipped(format!("cargo fallow check stopped ({})", err.message));
        }
    };
    if !report.fails_run() {
        return Audit::Pass;
    }
    Audit::Block(block_message(&root, &report))
}

/// The nearest `Cargo.toml` at or above `dir`. The walk stops at the first
/// folder with a `.git` entry.
fn nearest_manifest(dir: &Path) -> Option<PathBuf> {
    for folder in dir.ancestors() {
        let manifest = folder.join(MANIFEST);
        if manifest.is_file() {
            return Some(manifest);
        }
        if folder.join(".git").exists() {
            break;
        }
    }
    None
}

/// The flags of each cargo call of the gate: the gate never uses the
/// network.
fn cargo_flags() -> Vec<String> {
    vec!["--offline".to_owned()]
}

/// The home folder, as a canonical path.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|home| home.canonicalize().ok())
}

/// The ref that the changed files start from, and the commit of the base
/// run. A commit compares with `HEAD`: the staged, the unstaged and the
/// untracked changes count. A push compares with the upstream of the branch,
/// else with the default branch of `origin`, from their merge base with
/// `HEAD`.
fn base_ref(root: &Path, write: &GitWrite) -> Result<(String, String), String> {
    let commit_of = |name: &str| {
        git(
            root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{name}^{{commit}}"),
            ],
        )
        .ok()
        .map(|out| String::from_utf8_lossy(&out).trim().to_owned())
    };
    let Some(head) = commit_of("HEAD") else {
        return Err("the repository has no commit yet".to_owned());
    };
    if !write.kinds.contains(&WriteKind::Push) {
        return Ok(("HEAD".to_owned(), head));
    }
    let reference = push_ref(root)?;
    let fork = git(root, &["merge-base", &reference, "HEAD"])
        .map(|out| String::from_utf8_lossy(&out).trim().to_owned())
        .map_err(|_| format!("`{reference}` and HEAD have no merge base"))?;
    Ok((reference, fork))
}

/// The ref that a push compares with.
fn push_ref(root: &Path) -> Result<String, String> {
    let first_line = |args: &[&str]| {
        git(root, args).ok().and_then(|out| {
            let text = String::from_utf8_lossy(&out).trim().to_owned();
            (!text.is_empty()).then_some(text)
        })
    };
    first_line(&[
        "rev-parse",
        "--abbrev-ref",
        "--symbolic-full-name",
        "@{upstream}",
    ])
    .or_else(|| {
        first_line(&[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ])
    })
    .or_else(|| {
        ["origin/main", "origin/master", "main", "master"]
            .into_iter()
            .find(|name| git(root, &["rev-parse", "--verify", "--quiet", name]).is_ok())
            .map(str::to_owned)
    })
    .ok_or_else(|| "the push has no upstream and no default branch to compare with".to_owned())
}

/// The options of the check: each gate enforced, the findings of the changed
/// files only, and for a push the file-growth gate of the branch. A commit
/// gets no `--staged`: in `git add -A && git commit` the gate runs before the
/// add.
fn check_options(root: &Path, write: &GitWrite, reference: &str) -> check::Options {
    let push = write.kinds.contains(&WriteKind::Push);
    check::Options {
        manifest_path: Some(root.join(MANIFEST)),
        format: Format::Json,
        quiet: true,
        only: Vec::new(),
        skip: Vec::new(),
        fail_on_issues: true,
        fail_on_parse_error: false,
        base: push.then(|| reference.to_owned()),
        staged: false,
        cargo_flags: cargo_flags(),
        scope: crate::scope::ScopeArgs {
            changed_since: Some(reference.to_owned()),
            ..crate::scope::ScopeArgs::default()
        },
        health: crate::hotspots::HealthArgs::default(),
        coverage: crate::structure::crap::CoverageArgs::default(),
    }
}

/// The block message: the failed gates, one line for each finding that
/// caused the block, and the path of the full report.
fn block_message(root: &Path, report: &CheckReport) -> String {
    let report_path = match report::write(root, report) {
        ReportTarget::Written(path) | ReportTarget::Fallback(path) => path.display().to_string(),
        ReportTarget::Failed(detail) => format!("not written ({detail})"),
    };
    let failed: Vec<&str> = report
        .gate_outcomes
        .iter()
        .filter(|(_, outcome)| outcome.fails_run())
        .map(|(name, _)| name.as_str())
        .collect();
    let mut listed: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Error)
        .collect();
    if listed.is_empty() {
        listed = report.findings.iter().collect();
    }
    let mut text = String::new();
    let _ = writeln!(
        text,
        "{PREFIX}: blocked by cargo-fallow {TOOL_VERSION} in {}",
        clean(&root.display().to_string())
    );
    let _ = writeln!(text, "{PREFIX}: failed gates: {}", failed.join(", "));
    if !listed.is_empty() {
        let _ = writeln!(
            text,
            "{PREFIX}: {} finding(s) in the changed files caused the block:",
            listed.len()
        );
    }
    for finding in listed.iter().take(MAX_LISTED_FINDINGS) {
        let place = finding.location.line.map_or_else(
            || finding.location.path.clone(),
            |line| format!("{}:{line}", finding.location.path),
        );
        let _ = writeln!(
            text,
            "  {}/{} {} {}",
            finding.check,
            clean(&finding.rule),
            clean(&place),
            clean(&finding.message)
        );
    }
    if listed.len() > MAX_LISTED_FINDINGS {
        let _ = writeln!(
            text,
            "  ... and {} more in the full report",
            listed.len() - MAX_LISTED_FINDINGS
        );
    }
    let _ = writeln!(text, "{PREFIX}: full report: {}", clean(&report_path));
    let _ = writeln!(
        text,
        "{PREFIX}: fix the findings and try again. `cargo fallow explain <rule>` explains a rule, and `cargo fallow fix --dry-run --format json` lists the fixable actions."
    );
    text
}

/// Replaces each control character and each Unicode line or paragraph
/// separator with a space, so that a name from the workspace cannot split a
/// line or send a terminal escape sequence.
pub fn clean(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decide(payload: &str) -> Outcome {
        gate(&mut payload.as_bytes())
    }

    #[test]
    fn a_bad_payload_gives_one_notice_and_no_block() {
        for payload in ["", "not json", "[1]"] {
            let outcome = decide(payload);
            assert!(!outcome.block, "{payload}");
            assert_eq!(outcome.message.lines().count(), 1, "{payload}");
            assert!(outcome.message.contains("invalid tool input"), "{payload}");
        }
    }

    #[test]
    fn a_large_payload_is_refused() {
        let payload = format!(
            "{{\"x\":\"{}\"}}",
            "a".repeat(usize::try_from(MAX_PAYLOAD_BYTES).unwrap_or(0))
        );
        let outcome = decide(&payload);
        assert!(!outcome.block);
        assert!(outcome.message.contains("over the limit"));
    }

    #[test]
    fn a_command_that_is_not_a_git_write_passes_in_silence() {
        let outcome = decide(r#"{"cwd":"/","tool_input":{"command":"cargo test"}}"#);
        assert!(!outcome.block);
        assert!(outcome.message.is_empty());
    }

    #[test]
    fn a_git_write_outside_a_workspace_passes() {
        let dir = tempfile::tempdir().unwrap_or_else(|err| panic!("{err}"));
        let payload = serde_json::json!({
            "cwd": dir.path(),
            "tool_input": { "command": "git commit -m x" },
        });
        let outcome = decide(&payload.to_string());
        assert!(!outcome.block);
        assert!(outcome.message.is_empty());
    }

    #[test]
    fn clean_removes_control_characters() {
        assert_eq!(clean("a\nb\u{1b}[31m\u{2028}c"), "a b [31m c");
    }
}
