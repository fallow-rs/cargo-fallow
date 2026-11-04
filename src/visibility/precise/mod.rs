//! `--precise` mode of the `visibility` command.
//!
//! The fast check lists in `undecided` the candidates that only an untyped
//! credit keeps: a method call or a field access on an unknown type, or a name
//! in macro tokens or in an attribute. This mode decides only those candidates.
//! A worker process loads the workspace into rust-analyzer, with build scripts
//! and proc-macros, and finds the uses of each candidate with name resolution and
//! type inference.
//!
//! The worker runs under a time limit and the memory guard of the `mutants`
//! command. It writes one line for each decision, so a stopped worker still
//! gives the decisions that it made. A candidate without a decision stays
//! undecided, and it is not a finding.
//!
//! Only a build with the `precise` Cargo feature has the worker. The default
//! build has no `ra_ap_*` crate beyond the parser.

#[cfg(feature = "precise")]
mod engine;

use std::fs;
#[cfg(feature = "precise")]
use std::io::Write;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fallow_report_protocol::exit_code::{EXIT_INVALID_INPUT, EXIT_OK};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::error::{CliError, ErrorCode};
use crate::graph::Graph;
use crate::mutation::guard::{self, Guard, Scope};
use crate::mutation::process::{self, Exit};
use crate::visibility::{Analysis, Finding, FindingKind};

/// The default time limit of the mode, in seconds.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 1200;
/// The default memory limit of the worker, in MiB. The worker needs about
/// 4.4 GiB on a workspace of 40 MB of Rust.
pub const DEFAULT_MEMORY_MIB: u64 = 8192;
/// The default size limit of a searched file, in KiB. Generated tables are
/// larger, and a name search in them can take minutes. A large test file
/// written by hand stays under the limit.
pub const DEFAULT_FILE_LIMIT_KIB: u64 = 1024;
/// The time after the limit that the worker gets to stop by itself.
const STOP_GRACE: Duration = Duration::from_secs(30);
/// The most lines of the worker's stderr that a failure shows.
const STDERR_LINES: usize = 20;
/// The most bytes at the end of the worker's stderr that a failure reads.
const STDERR_TAIL_BYTES: u64 = 8 * 1024;

/// The limits of one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The time limit of the worker.
    pub timeout: Duration,
    /// The memory limit of the worker, in MiB.
    pub memory_mib: u64,
    /// The size limit of a searched file, in KiB.
    pub file_limit_kib: u64,
}

/// The arguments of the hidden worker command.
#[derive(Debug, Clone, clap::Args)]
pub struct WorkerArgs {
    /// The request, as JSON.
    #[arg(long)]
    pub input: PathBuf,
    /// The file for the events, one JSON object on each line.
    #[arg(long)]
    pub output: PathBuf,
}

/// The request to the worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// The `Cargo.toml` of the workspace.
    pub manifest: PathBuf,
    /// The absolute workspace root.
    pub root: PathBuf,
    /// The absolute target folder. Its files are not searched.
    pub target_dir: PathBuf,
    /// The size limit of a searched file, in bytes.
    pub file_limit_bytes: u64,
    /// The time limit of the search, in milliseconds from the start.
    pub deadline_ms: u64,
    /// The candidates.
    pub candidates: Vec<Candidate>,
}

/// One candidate to decide.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    /// The index in the request.
    pub id: usize,
    /// The finding kind that the fast check gives without untyped credits.
    pub kind: String,
    /// The file, relative to the workspace root.
    pub file: String,
    /// The one-based line of the name.
    pub line: u32,
    /// The one-based column of the name, in characters.
    pub column: u32,
    /// The name.
    pub name: String,
}

/// One line of the worker output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// The workspace is in the database.
    Loaded {
        /// The milliseconds of the load.
        ms: u64,
    },
    /// The decision for one candidate.
    Decided {
        /// The candidate.
        id: usize,
        /// The decision.
        decision: Decision,
    },
    /// The worker reached the time limit, and it stopped the search.
    TimeLimit,
    /// A part of the load is missing, so the worker cannot decide. The worker
    /// stops after this event.
    LoadFailed {
        /// The missing part.
        problem: LoadProblem,
        /// The text for the user.
        detail: String,
    },
    /// The worker ended the search.
    Done {
        /// The milliseconds of the search.
        ms: u64,
        /// The workspace files over the size limit.
        large_files: usize,
    },
}

/// A missing part of the load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadProblem {
    /// The toolchain has no source of the standard library (`rust-src`).
    MissingStdSource,
    /// A proc-macro crate did not load: no proc-macro server, or the crate
    /// did not build.
    ProcMacro,
}

impl LoadProblem {
    /// The help text of the error.
    const fn help(self) -> &'static str {
        match self {
            Self::MissingStdSource => {
                "Install the source of the standard library with `rustup component add \
                 rust-src`, or run without `--precise`."
            }
            Self::ProcMacro => {
                "Make sure that `cargo check --all-targets --all-features` builds the \
                 proc-macro crates, and that the toolchain has the proc-macro server of the \
                 `rustc` component. Or run without `--precise`."
            }
        }
    }
}

/// A fault that the worker simulates, for the tests. Only a debug build reads
/// the `CARGO_FALLOW_PRECISE_TEST_FAULT` environment variable.
#[cfg(feature = "precise")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The worker stops at once with a failure.
    Abort,
    /// The worker writes its process id to the file in
    /// `CARGO_FALLOW_PRECISE_TEST_PID_FILE` and then waits forever.
    Hang,
    /// The load uses an empty folder as the source of the standard library.
    NoStdSource,
    /// The load starts no proc-macro server.
    NoProcMacroServer,
}

#[cfg(feature = "precise")]
impl Fault {
    /// The fault of this process, or `None`.
    pub fn from_env() -> Option<Self> {
        if !cfg!(debug_assertions) {
            return None;
        }
        match std::env::var("CARGO_FALLOW_PRECISE_TEST_FAULT")
            .ok()?
            .as_str()
        {
            "abort" => Some(Self::Abort),
            "hang" => Some(Self::Hang),
            "no-std-source" => Some(Self::NoStdSource),
            "no-proc-macro-server" => Some(Self::NoProcMacroServer),
            _ => None,
        }
    }
}

/// The decision for one candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// The candidate is a finding of this kind.
    Report {
        /// The finding kind label.
        kind: String,
    },
    /// The item keeps its visibility.
    Keep {
        /// Why.
        reason: KeepReason,
    },
    /// The database has no definition at the position of the candidate.
    Unresolved,
}

/// Why an item keeps its visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepReason {
    /// Another crate uses the item.
    OtherCrate,
    /// A `pub` signature names the item.
    PublicSignature,
    /// Code outside the module of a restricted item uses it.
    OutsideModule,
    /// The name is in a file over the size limit, which the search skips.
    LargeFile,
}

/// The result of the mode in the report.
#[derive(Debug, Clone, Default, Serialize, schemars::JsonSchema)]
pub struct PreciseSummary {
    /// `complete`, `nothing_to_decide`, `time_limit`, `memory_limit` or
    /// `failed`. Each status other than `complete` and `nothing_to_decide`
    /// leaves some candidates undecided.
    #[schemars(extend("enum" = ["complete", "nothing_to_decide", "time_limit", "memory_limit", "failed"]))]
    pub status: &'static str,
    /// The candidates of the fast check.
    pub candidates: usize,
    /// The candidates that became findings.
    pub reported: usize,
    /// The candidates whose item keeps its visibility.
    pub kept: usize,
    /// The kept candidates whose name is in a file over the size limit.
    pub kept_large_file: usize,
    /// The candidates that the database could not find.
    pub unresolved: usize,
    /// The candidates without a decision.
    pub left: usize,
    /// The workspace files over the size limit.
    pub large_files: usize,
    /// The milliseconds of the load, with build scripts and proc-macros.
    pub load_ms: Option<u64>,
    /// The milliseconds of the search.
    pub search_ms: Option<u64>,
    /// The milliseconds of the whole mode.
    pub elapsed_ms: u64,
    /// The reason of a stop, or the last lines of the worker's stderr.
    pub message: Option<String>,
}

impl PreciseSummary {
    /// True when the worker stopped before it decided each candidate: a
    /// failure, the time limit or the memory limit.
    pub fn incomplete(&self) -> bool {
        !matches!(self.status, "complete" | "nothing_to_decide")
    }

    /// One line for a person.
    pub fn human_line(&self) -> String {
        let seconds = self.elapsed_ms as f64 / 1000.0;
        let mut line = format!(
            "Precise mode: {}, {} candidates, {} reported, {} kept, {} left, in {seconds:.1} s.",
            self.status, self.candidates, self.reported, self.kept, self.left
        );
        if let Some(message) = self
            .message
            .as_deref()
            .filter(|text| !text.trim().is_empty())
        {
            line.push(' ');
            line.push_str(message);
        }
        line
    }
}

/// The paths and the workspace facts of one run.
pub struct Run<'a> {
    /// The `Cargo.toml` of the workspace.
    pub manifest: PathBuf,
    /// The absolute workspace root.
    pub root: &'a Path,
    /// The absolute target folder.
    pub target_dir: &'a Path,
    /// The limits.
    pub limits: Limits,
}

/// Decides the undecided candidates of `analysis` and merges the decisions.
pub fn decide(
    graph: &Graph,
    analysis: &mut Analysis,
    run: &Run<'_>,
) -> Result<PreciseSummary, CliError> {
    if !cfg!(feature = "precise") {
        return Err(CliError::new(
            ErrorCode::InvalidArgument,
            "this build of cargo-fallow has no `--precise` mode",
        )
        .with_help(
            "Install a build with the `precise` feature: `cargo install cargo-fallow --locked \
             --features precise`.",
        ));
    }
    let start = Instant::now();
    let candidates: Vec<Candidate> = analysis
        .undecided
        .iter()
        .enumerate()
        .filter(|(_, finding)| analysis.cfg_used.binary_search(&finding.item).is_err())
        .filter(|(_, finding)| analysis.opaque.binary_search(finding).is_err())
        .map(|(id, finding)| candidate(graph, id, finding))
        .collect();
    if candidates.is_empty() {
        return Ok(PreciseSummary {
            status: "nothing_to_decide",
            ..PreciseSummary::default()
        });
    }
    let folder = tempfile::tempdir().map_err(|err| failure("create a temporary folder", &err))?;
    let request = Request {
        manifest: run.manifest.clone(),
        root: run.root.to_path_buf(),
        target_dir: run.target_dir.to_path_buf(),
        file_limit_bytes: run.limits.file_limit_kib.saturating_mul(1024),
        deadline_ms: u64::try_from(run.limits.timeout.as_millis()).unwrap_or(u64::MAX),
        candidates,
    };
    let input = folder.path().join("request.json");
    let output = folder.path().join("events.jsonl");
    let text = serde_json::to_string(&request)
        .map_err(|err| failure("write the request", &io::Error::other(err)))?;
    fs::write(&input, text).map_err(|err| failure("write the request", &err))?;
    let exit = run_worker(folder.path(), &input, &output, run.limits)?;
    let events = read_events(&output);
    if let Some((problem, detail)) = events.iter().find_map(|event| match event {
        Event::LoadFailed { problem, detail } => Some((*problem, detail)),
        _ => None,
    }) {
        return Err(CliError::new(
            ErrorCode::PreciseFailed,
            format!("`--precise` cannot decide: {detail}"),
        )
        .with_help(problem.help()));
    }
    let mut summary = summarize(&request.candidates, &events, &exit, folder.path());
    let decisions: FxHashMap<usize, Decision> = events
        .into_iter()
        .filter_map(|event| match event {
            Event::Decided { id, decision } => Some((id, decision)),
            _ => None,
        })
        .collect();
    merge(analysis, &decisions, |item| {
        graph.items[item].kind == crate::graph::extract::ItemKind::Variant
    });
    summary.elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(summary)
}

fn candidate(graph: &Graph, id: usize, finding: &Finding) -> Candidate {
    let item = &graph.items[finding.item];
    Candidate {
        id,
        kind: finding.kind.label().to_owned(),
        file: item.file.clone(),
        line: item.line,
        column: item.column,
        name: item.name.clone(),
    }
}

fn failure(what: &str, err: &io::Error) -> CliError {
    CliError::new(
        ErrorCode::InvalidArgument,
        format!("cannot {what} for `--precise`: {err}"),
    )
}

/// Starts the worker under the guard and waits for it.
fn run_worker(
    folder: &Path,
    input: &Path,
    output: &Path,
    limits: Limits,
) -> Result<Exit, CliError> {
    let exe = std::env::current_exe().map_err(|err| failure("find this program", &err))?;
    let mut guard = Guard::start(guard::Limits::new(
        limits.memory_mib,
        guard::DEFAULT_LOW_PERCENT,
    ))
    .map_err(|err| {
        CliError::new(
            ErrorCode::InvalidArgument,
            format!("`--precise` needs the memory guard: {err}"),
        )
    })?;
    guard
        .start_watchdog(&exe, folder.join("roots"), folder, false)
        .map_err(|err| failure("start the watchdog", &err))?;
    let mut command = process::new_command(&exe);
    command
        .arg("visibility-precise-worker")
        .arg("--input")
        .arg(input)
        .arg("--output")
        .arg(output);
    process::run_to_files(
        command,
        &folder.join("stdout.log"),
        &folder.join("stderr.log"),
        limits.timeout + STOP_GRACE,
        &guard,
        Scope::Tests,
    )
    .map_err(|err| failure("run the worker", &err))
}

/// The events in the output file. A line that does not parse ends the list: a
/// stopped worker can leave half a line.
fn read_events(path: &Path) -> Vec<Event> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(|line| line.ok().and_then(|line| serde_json::from_str(&line).ok()))
        .collect()
}

fn summarize(
    candidates: &[Candidate],
    events: &[Event],
    exit: &Exit,
    folder: &Path,
) -> PreciseSummary {
    let mut summary = PreciseSummary {
        status: "complete",
        candidates: candidates.len(),
        ..PreciseSummary::default()
    };
    let mut decided = 0;
    let mut time_limit = false;
    for event in events {
        match event {
            Event::Loaded { ms } => summary.load_ms = Some(*ms),
            Event::Decided { decision, .. } => {
                decided += 1;
                match decision {
                    Decision::Report { .. } => summary.reported += 1,
                    Decision::Keep { reason } => {
                        summary.kept += 1;
                        if *reason == KeepReason::LargeFile {
                            summary.kept_large_file += 1;
                        }
                    }
                    Decision::Unresolved => summary.unresolved += 1,
                }
            }
            Event::TimeLimit => time_limit = true,
            Event::LoadFailed { .. } => {}
            Event::Done { ms, large_files } => {
                summary.search_ms = Some(*ms);
                summary.large_files = *large_files;
            }
        }
    }
    summary.left = candidates.len().saturating_sub(decided);
    let finished = events
        .iter()
        .any(|event| matches!(event, Event::Done { .. }));
    match exit {
        Exit::Code(Some(0)) if finished && !time_limit => {}
        Exit::Code(Some(0)) if time_limit => {
            summary.status = "time_limit";
            summary.message = Some("The time limit stopped the search.".to_owned());
        }
        Exit::TimedOut => {
            summary.status = "time_limit";
            summary.message = Some("The time limit stopped the worker.".to_owned());
        }
        Exit::Killed(reason) => {
            summary.status = "memory_limit";
            summary.message = Some(format!("The memory guard stopped the worker: {reason:?}."));
        }
        Exit::Code(_) => {
            summary.status = "failed";
            summary.message = Some(stderr_tail(&folder.join("stderr.log")));
        }
    }
    summary
}

/// The last lines of the worker's stderr. Only the end of the file is read, so
/// a large log does not fill the memory.
fn stderr_tail(path: &Path) -> String {
    let Ok(mut file) = fs::File::open(path) else {
        return String::new();
    };
    let size = file.metadata().map_or(0, |meta| meta.len());
    let start = size.saturating_sub(STDERR_TAIL_BYTES);
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(start)).is_err() || file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    // The first line can start in the middle, so it does not count.
    let text = if start > 0 {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        &text
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(STDERR_LINES);
    lines[start..].join("\n")
}

/// Merges the decisions into the findings. A `report` decision replaces a
/// finding of the fast check on the same item, because the fast check can give
/// a weaker kind: a method call by name keeps a dead method live. A `keep`
/// decision removes the findings on its item. A candidate without a decision
/// stays undecided.
pub fn merge(
    analysis: &mut Analysis,
    decisions: &FxHashMap<usize, Decision>,
    is_variant: impl Fn(crate::graph::ItemId) -> bool,
) {
    let undecided = std::mem::take(&mut analysis.undecided);
    for (id, candidate) in undecided.into_iter().enumerate() {
        let public = candidate.kind != FindingKind::UnnecessaryRestricted;
        let same_family = |finding: &Finding| {
            finding.item == candidate.item
                && (finding.kind != FindingKind::UnnecessaryRestricted) == public
        };
        match decisions.get(&id) {
            Some(Decision::Report { kind }) => {
                let Some(kind) = FindingKind::from_label(kind) else {
                    analysis.undecided.push(candidate);
                    continue;
                };
                analysis.findings.retain(|finding| !same_family(finding));
                // A variant has no keyword, so a use in its own crate keeps it.
                if kind == FindingKind::UnnecessaryPublic && is_variant(candidate.item) {
                    continue;
                }
                analysis.findings.push(Finding { kind, ..candidate });
            }
            Some(Decision::Keep { .. }) => {
                analysis.findings.retain(|finding| !same_family(finding));
            }
            Some(Decision::Unresolved) | None => analysis.undecided.push(candidate),
        }
    }
    analysis.findings.sort();
    analysis.findings.dedup();
}

/// Runs the hidden worker command and returns the exit code.
pub fn run_worker_command(args: &WorkerArgs) -> u8 {
    match worker(args) {
        Ok(()) => EXIT_OK,
        Err(message) => {
            // The parent reads the stderr file. A failed write changes nothing.
            let _ = crate::render::write_stderr(&format!("error: {message}"));
            EXIT_INVALID_INPUT
        }
    }
}

#[cfg(feature = "precise")]
fn worker(args: &WorkerArgs) -> Result<(), String> {
    match Fault::from_env() {
        Some(Fault::Abort) => return Err("the test fault stopped the worker".to_owned()),
        Some(Fault::Hang) => hang(),
        _ => {}
    }
    let text = fs::read_to_string(&args.input).map_err(|err| format!("read the request: {err}"))?;
    let request: Request =
        serde_json::from_str(&text).map_err(|err| format!("parse the request: {err}"))?;
    let file = fs::File::create(&args.output).map_err(|err| format!("create the output: {err}"))?;
    engine::run(&request, &mut EventSink { file })
}

#[cfg(not(feature = "precise"))]
fn worker(_args: &WorkerArgs) -> Result<(), String> {
    Err("this build has no `precise` feature".to_owned())
}

/// Writes the process id for the test, and then waits forever.
#[cfg(feature = "precise")]
fn hang() -> ! {
    if let Some(path) = std::env::var_os("CARGO_FALLOW_PRECISE_TEST_PID_FILE") {
        // The test reads the file. A failed write makes the test fail.
        let _ = fs::write(path, std::process::id().to_string());
    }
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// Writes each event as one line, and flushes it at once.
#[cfg(feature = "precise")]
pub struct EventSink {
    file: fs::File,
}

#[cfg(feature = "precise")]
impl EventSink {
    /// Writes one event.
    pub fn send(&mut self, event: &Event) -> Result<(), String> {
        let line = serde_json::to_string(event).map_err(|err| err.to_string())?;
        writeln!(self.file, "{line}")
            .and_then(|()| self.file.flush())
            .map_err(|err| format!("write an event: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(kind: FindingKind, item: usize) -> Finding {
        Finding {
            kind,
            item,
            test_only: false,
        }
    }

    #[test]
    fn a_report_replaces_the_weaker_kind_of_the_fast_check() {
        let mut analysis = Analysis {
            findings: vec![
                finding(FindingKind::UnnecessaryPublic, 1),
                finding(FindingKind::UnnecessaryPublic, 2),
            ],
            undecided: vec![
                finding(FindingKind::DeadPublic, 1),
                finding(FindingKind::DeadPublic, 2),
                finding(FindingKind::UnnecessaryRestricted, 3),
                finding(FindingKind::DeadPublic, 4),
                finding(FindingKind::DeadPublic, 5),
            ],
            cfg_used: Vec::new(),
            opaque: Vec::new(),
        };
        let decisions: FxHashMap<usize, Decision> = [
            (
                0,
                Decision::Report {
                    kind: "dead_public".to_owned(),
                },
            ),
            (
                1,
                Decision::Keep {
                    reason: KeepReason::OtherCrate,
                },
            ),
            (
                2,
                Decision::Report {
                    kind: "unnecessary_restricted_visibility".to_owned(),
                },
            ),
            (
                4,
                Decision::Report {
                    kind: "unnecessary_public".to_owned(),
                },
            ),
        ]
        .into_iter()
        .collect();
        // Item 5 is an enum variant: it has no keyword, so only `dead_public`
        // applies to it.
        merge(&mut analysis, &decisions, |item| item == 5);
        assert_eq!(
            analysis.findings,
            vec![
                finding(FindingKind::DeadPublic, 1),
                finding(FindingKind::UnnecessaryRestricted, 3),
            ]
        );
        assert_eq!(
            analysis.undecided,
            vec![finding(FindingKind::DeadPublic, 4)]
        );
    }

    #[test]
    fn a_half_line_ends_the_events() {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let path = dir.path().join("events.jsonl");
        let Ok(()) = fs::write(
            &path,
            "{\"event\":\"loaded\",\"ms\":5}\n{\"event\":\"decided\",\"id\":0,\"decision\":{\"decision\":\"unresolved\"}}\n{\"event\":\"deci",
        ) else {
            panic!("write the events");
        };
        assert_eq!(
            read_events(&path),
            vec![
                Event::Loaded { ms: 5 },
                Event::Decided {
                    id: 0,
                    decision: Decision::Unresolved
                }
            ]
        );
    }

    #[test]
    fn the_stderr_tail_reads_only_the_end_of_a_large_file() {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let path = dir.path().join("stderr.log");
        // Bytes that are not UTF-8 at the start, far before the tail.
        let mut text = vec![0xff, 0xfe, b'\n'];
        for line in 0..100_000 {
            text.extend_from_slice(format!("line {line}\n").as_bytes());
        }
        let Ok(()) = fs::write(&path, text) else {
            panic!("write the file");
        };
        let tail = stderr_tail(&path);
        assert_eq!(tail.lines().count(), STDERR_LINES);
        assert!(tail.ends_with("line 99999"), "{tail}");
    }

    #[test]
    fn an_empty_message_adds_no_space_to_the_line() {
        let summary = PreciseSummary {
            status: "failed",
            message: Some(String::new()),
            ..PreciseSummary::default()
        };
        assert!(
            summary.human_line().ends_with(" s."),
            "{}",
            summary.human_line()
        );
    }

    #[test]
    fn a_stopped_worker_leaves_the_rest_undecided() {
        let candidates: Vec<Candidate> = (0..3)
            .map(|id| Candidate {
                id,
                kind: "dead_public".to_owned(),
                file: "a.rs".to_owned(),
                line: 1,
                column: 1,
                name: "a".to_owned(),
            })
            .collect();
        let events = vec![
            Event::Loaded { ms: 1 },
            Event::Decided {
                id: 1,
                decision: Decision::Keep {
                    reason: KeepReason::LargeFile,
                },
            },
            Event::Decided {
                id: 0,
                decision: Decision::Report {
                    kind: "dead_public".to_owned(),
                },
            },
            Event::TimeLimit,
            Event::Done {
                ms: 2,
                large_files: 1,
            },
        ];
        let summary = summarize(&candidates, &events, &Exit::Code(Some(0)), Path::new("."));
        assert_eq!(summary.status, "time_limit");
        assert_eq!(
            (summary.reported, summary.kept_large_file, summary.left),
            (1, 1, 1)
        );
        let killed = summarize(&candidates, &events[..3], &Exit::TimedOut, Path::new("."));
        assert_eq!(killed.status, "time_limit");
    }
}
