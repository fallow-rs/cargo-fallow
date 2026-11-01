//! Starts and stops child processes.
//!
//! A test process that a mutant controls can spawn children and can grow without
//! limit. So each child runs through [`run_to_files`], which has a time limit and
//! a place under the [`Guard`]. nextest puts each test in its own process group,
//! so a stop walks the process tree and does not signal one group.

use std::fs::File;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::mutation::guard::{Guard, KillReason, Scope};
use crate::mutation::probe::{Row, system_probe};

/// The time between two checks of a running child.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The compiler flag that keeps each lint at the warning level.
const CAP_LINTS: &str = "--cap-lints=warn";

/// How a child process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    /// The child ended. The code is `None` when a signal stopped it.
    Code(Option<i32>),
    /// The child passed the time limit, and the runner killed its tree.
    TimedOut,
    /// The memory guard killed the tree of the child.
    Killed(KillReason),
}

/// A `cargo` command that runs in `dir` and builds into `target_dir`.
///
/// The environment makes two builds with the same command comparable: no
/// compiler wrapper, a fixed job count, and no active mutant. A mutant often
/// starts a lint, for example an unused variable. In a crate with
/// `deny(warnings)` that says nothing about the mutant, so lints stay warnings.
pub fn cargo(dir: &Path, target_dir: &Path, jobs: usize) -> Command {
    let mut command = new_command("cargo");
    command
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", target_dir)
        .env("CARGO_BUILD_JOBS", jobs.to_string())
        .env("RUSTC_WRAPPER", "")
        .env_remove("FALLOW_MUTANT");
    // RUSTFLAGS replaces the flags of the Cargo config, so it gets an addition.
    // Without it, the config variable joins the flags of the Cargo config.
    match std::env::var("RUSTFLAGS") {
        Ok(flags) => command.env("RUSTFLAGS", format!("{flags} {CAP_LINTS}")),
        Err(_) => command.env("CARGO_BUILD_RUSTFLAGS", CAP_LINTS),
    };
    command
}

/// The one place in the mutation module that creates a process.
pub fn new_command(program: &str) -> Command {
    #[expect(
        clippy::disallowed_methods,
        reason = "the mutation runner spawns cargo, reads memory data and stops process trees"
    )]
    Command::new(program)
}

/// Runs `command` with its output in two files. After `limit`, the runner kills
/// the child and all its descendants. The guard watches the tree in `scope`.
pub fn run_to_files(
    mut command: Command,
    stdout: &Path,
    stderr: &Path,
    limit: Duration,
    guard: &Guard,
    scope: Scope,
) -> io::Result<Exit> {
    // A child in a background process group stops when it reads the terminal.
    command
        .stdin(Stdio::null())
        .stdout(File::create(stdout)?)
        .stderr(File::create(stderr)?);
    start_new_group(&mut command);
    let mut child = command.spawn()?;
    let watch = guard.watch(child.id(), scope);
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(watch
                .finish()
                .map_or_else(|| Exit::Code(status.code()), Exit::Killed));
        }
        if Instant::now() >= deadline {
            let reason = watch.finish();
            kill_tree(child.id());
            child.kill()?;
            child.wait()?;
            return Ok(reason.map_or(Exit::TimedOut, Exit::Killed));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn start_new_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn start_new_group(_command: &mut Command) {}

/// Stops `root`, so that it starts no new child, and then kills it and all its
/// descendants. A process that is gone is no error.
pub fn kill_tree(root: u32) {
    let Some(probe) = system_probe() else {
        return;
    };
    signal("-STOP", &[root]);
    let mut pids = probe
        .table()
        .map(|rows| descendants(&rows, root))
        .unwrap_or_default();
    pids.push(root);
    signal("-KILL", &pids);
}

/// Sends a signal through the shell builtin, which needs no unsafe code and no
/// `kill` program in the image.
fn signal(signal: &str, pids: &[u32]) {
    let pids: Vec<String> = pids.iter().map(u32::to_string).collect();
    // A process that is gone gives an error status, and that changes nothing.
    let _ = new_command("sh")
        .args(["-c", "kill \"$@\" 2>/dev/null", "sh", signal])
        .args(&pids)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// The descendants of `root` in a process table, without `root`.
pub fn descendants(rows: &[Row], root: u32) -> Vec<u32> {
    let mut found = Vec::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for row in rows.iter().filter(|row| row.ppid == parent) {
            // A table can hold a loop only through a reused process id.
            if row.pid != root && !found.contains(&row.pid) {
                found.push(row.pid);
                frontier.push(row.pid);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn run(script: &str, limit: Duration) -> (Exit, String) {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let (out, err) = (dir.path().join("out"), dir.path().join("err"));
        let mut command = new_command("sh");
        command.args(["-c", script]);
        let guard = Guard::disabled();
        let Ok(exit) = run_to_files(command, &out, &err, limit, &guard, Scope::Build) else {
            panic!("sh must start");
        };
        (exit, std::fs::read_to_string(out).unwrap_or_default())
    }

    #[cfg(unix)]
    fn alive(pattern: &str) -> bool {
        new_command("pgrep")
            .args(["-f", pattern])
            .output()
            .is_ok_and(|output| !output.stdout.is_empty())
    }

    #[cfg(unix)]
    #[test]
    fn exit_code_and_output_are_returned() {
        let (exit, out) = run("echo hello; exit 3", Duration::from_secs(60));
        assert_eq!(exit, Exit::Code(Some(3)));
        assert_eq!(out, "hello\n");
    }

    #[cfg(unix)]
    #[test]
    fn child_over_the_limit_is_killed_with_its_group() {
        let start = Instant::now();
        let (exit, _) = run("sleep 30 & wait", Duration::from_millis(200));
        assert_eq!(exit, Exit::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(20));
    }

    /// A test process of nextest has its own process group, so a signal to the
    /// group of the root does not reach it.
    #[cfg(unix)]
    #[test]
    fn a_descendant_in_another_group_is_killed_at_the_limit() {
        // The value is unique, so `pgrep` finds only this sleep.
        let marker = "sleep 47.3127";
        let script = format!("perl -e 'setpgrp(0,0); exec @ARGV' {marker} & wait");
        let (exit, _) = run(&script, Duration::from_millis(500));
        assert_eq!(exit, Exit::TimedOut);
        thread::sleep(Duration::from_millis(200));
        assert!(!alive(marker), "the descendant must be gone");
    }

    #[test]
    fn descendants_are_found_at_each_depth() {
        let row = |pid, ppid| Row {
            pid,
            ppid,
            rss_kib: 0,
        };
        let rows = [row(2, 1), row(3, 2), row(4, 3), row(5, 1), row(6, 9)];
        let mut found = descendants(&rows, 2);
        found.sort_unstable();
        assert_eq!(found, [3, 4]);
    }

    #[test]
    fn a_loop_in_the_table_ends_the_walk() {
        let row = |pid, ppid| Row {
            pid,
            ppid,
            rss_kib: 0,
        };
        assert_eq!(descendants(&[row(2, 1), row(1, 2)], 1), [2]);
    }

    #[test]
    fn missing_program_is_an_error() {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let result = run_to_files(
            new_command("cargo-fallow-no-such-program"),
            &dir.path().join("out"),
            &dir.path().join("err"),
            Duration::from_secs(1),
            &Guard::disabled(),
            Scope::Build,
        );
        assert!(result.is_err());
    }
}
