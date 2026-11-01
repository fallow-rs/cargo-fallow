//! Runs a child process with a time limit and with its output in files.

use std::fs::File;
use std::io;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// The time between two checks of a running child.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The number of parallel build jobs and parallel test processes.
///
/// A mutant can allocate without limit. A low number limits the damage, and
/// the spike must leave the rest of the machine usable.
pub const JOBS: &str = "2";

/// How a child process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The child ended. The code is `None` when a signal stopped it.
    Code(Option<i32>),
    /// The child passed the time limit, and the runner killed it.
    TimedOut,
}

/// A `cargo` command that runs in `dir` and builds into `target_dir`.
///
/// The environment makes two builds with the same command comparable: no
/// compiler wrapper, a fixed job count, and no active mutant.
pub fn cargo(dir: &Path, target_dir: &Path) -> Command {
    let mut command = new_command("cargo");
    command
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", target_dir)
        .env("CARGO_BUILD_JOBS", JOBS)
        .env("RUSTC_WRAPPER", "")
        .env_remove("FALLOW_MUTANT");
    command
}

/// The one place in the mutation module that creates a process.
fn new_command(program: &str) -> Command {
    #[expect(
        clippy::disallowed_methods,
        reason = "the mutation runner spawns cargo and stops a test process group"
    )]
    Command::new(program)
}

/// Runs `command` with its output in two files. After `limit`, the runner kills
/// the child and all processes in its group.
pub fn run_to_files(
    mut command: Command,
    stdout: &Path,
    stderr: &Path,
    limit: Duration,
) -> io::Result<Exit> {
    command
        .stdout(File::create(stdout)?)
        .stderr(File::create(stderr)?);
    start_new_group(&mut command);
    let mut child = command.spawn()?;
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Exit::Code(status.code()));
        }
        if Instant::now() >= deadline {
            kill_group(child.id());
            child.kill()?;
            child.wait()?;
            return Ok(Exit::TimedOut);
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

/// Kills the process group. The `kill` program needs no unsafe code.
#[cfg(unix)]
fn kill_group(leader: u32) {
    // The caller also kills the leader, and a group that is gone is no error.
    let _ = new_command("kill")
        .args(["-KILL", "--", &format!("-{leader}")])
        .status();
}

#[cfg(not(unix))]
fn kill_group(_leader: u32) {}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn run(script: &str, limit: Duration) -> (Exit, String) {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let (out, err) = (dir.path().join("out"), dir.path().join("err"));
        let mut command = new_command("sh");
        command.args(["-c", script]);
        let Ok(exit) = run_to_files(command, &out, &err, limit) else {
            panic!("sh must start");
        };
        (exit, fs::read_to_string(out).unwrap_or_default())
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
        );
        assert!(result.is_err());
    }
}
