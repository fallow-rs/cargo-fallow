//! Starts and stops child processes.
//!
//! A test process that a mutant controls can spawn children and can grow without
//! limit. So each child runs through [`run_to_files`], which has a time limit and
//! a place under the [`Guard`]. nextest puts each test in its own process group,
//! so a stop walks the process tree and does not signal one group.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::mutation::guard::{Guard, KillReason, Scope};
use crate::mutation::probe::{Row, system_probe};
#[cfg(unix)]
use crate::mutation::schema::{MUTANT_FD, mutant_record};

/// The time between two checks of a running child.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The compiler flags of each build. The first flag keeps each lint at the warning
/// level. A switch has parentheses and can hold a comparison that the compiler can
/// see through, so the other flags keep those warnings out of the build output.
const BUILD_FLAGS: &str = "--cap-lints=warn -Aunused_parens -Aunused_comparisons";
/// The flag that keeps each lint of a doctest build at the warning level.
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
/// The environment makes two builds with the same command comparable: the tool's
/// own compiler wrapper, a fixed job count, and no active mutant. A mutant often
/// starts a lint, for example an unused variable. In a crate with
/// `deny(warnings)` that says nothing about the mutant, so lints stay warnings.
pub fn cargo(dir: &Path, target_dir: &Path, jobs: usize) -> Command {
    cargo_with_flags(dir, target_dir, jobs, "")
}

/// True when the cargo flags of the user forbid the network.
pub fn offline(cargo_flags: &[String]) -> bool {
    cargo_flags
        .iter()
        .any(|flag| flag == "--offline" || flag == "--frozen")
}

/// Forbids the network in `command` and in each cargo call of its children, for
/// example a test that starts cargo. A child does not get the flags of the user.
pub fn stay_offline(command: &mut Command) {
    command.env("CARGO_NET_OFFLINE", "true");
}

/// The same command with more compiler flags for each crate of the selected package.
pub fn cargo_with_flags(dir: &Path, target_dir: &Path, jobs: usize, extra: &str) -> Command {
    let mut command = new_command("cargo");
    command
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", target_dir)
        .env("CARGO_BUILD_JOBS", jobs.to_string())
        // The output goes to files and into the report, never to a terminal.
        .env("CARGO_TERM_COLOR", "never")
        .env_remove("FALLOW_MUTANT");
    add_compiler_flags(&mut command, target_dir, extra);
    command
}

/// Adds compiler flags through a small wrapper of the compiler.
///
/// cargo takes its flags from one source only: `CARGO_ENCODED_RUSTFLAGS`, else
/// `RUSTFLAGS`, else the `[target]` keys of the config, else `build.rustflags`. So
/// an environment variable can replace the flags of the project, or the project can
/// replace the flags of this tool. A wrapper does neither. The flags of the tool go
/// first, so `--cap-lints` of cargo for a registry package stays in force. `extra`
/// reaches only the crates of the selected package. A wrapper
/// of the user does not run: a compile cache only adds doubt to a comparison.
#[cfg(unix)]
fn add_compiler_flags(command: &mut Command, target_dir: &Path, extra: &str) {
    let rustc = target_dir.with_extension("rustc.sh");
    let rustdoc = target_dir.with_extension("rustdoc.sh");
    let real_rustdoc = std::env::var("RUSTDOC").unwrap_or_else(|_| "rustdoc".to_owned());
    // A failed write gives a clear error from cargo at the first build.
    let _ = write_wrapper(
        &rustc,
        &format!(
            "rustc=\"$1\"\nshift\n\
             if [ -n \"$CARGO_PRIMARY_PACKAGE\" ]; then\n\
             exec \"$rustc\" {BUILD_FLAGS} {extra} \"$@\"\n\
             fi\n\
             exec \"$rustc\" {BUILD_FLAGS} \"$@\"\n"
        ),
    );
    // A doctest build reads the lint levels of the crate too, and the switch can
    // start a lint that the crate forbids. This cap has no other flags: the flags
    // of a coverage build do not belong in the doctests.
    let _ = write_wrapper(
        &rustdoc,
        &format!("exec '{real_rustdoc}' {CAP_LINTS} \"$@\"\n"),
    );
    command.env("RUSTC_WRAPPER", rustc).env("RUSTDOC", rustdoc);
}

/// The wrappers that this process wrote, with their text. cargo runs a wrapper as
/// the compiler, so a file that exists before the run is never trusted: the first
/// use always writes it.
#[cfg(unix)]
static WRITTEN: std::sync::Mutex<std::collections::BTreeMap<std::path::PathBuf, String>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

#[cfg(unix)]
fn write_wrapper(path: &Path, body: &str) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let text = format!("#!/bin/sh\n{body}");
    if written_text(path).as_ref() == Some(&text) {
        return Ok(());
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    // A new file with a random name and an exclusive create, then a rename over the
    // old name: no other process can supply the text, and no call sees a half file.
    let mut file = tempfile::Builder::new()
        .prefix(".wrapper-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempfile_in(parent)?;
    file.write_all(text.as_bytes())?;
    file.persist(path).map_err(|err| err.error)?;
    // Two writers of one path are safe: each rename puts a whole file in place.
    written_wrappers().insert(path.to_path_buf(), text);
    Ok(())
}

/// The set of written wrappers. A poisoned lock only means that another thread
/// panicked, and the map stays valid.
#[cfg(unix)]
fn written_wrappers()
-> std::sync::MutexGuard<'static, std::collections::BTreeMap<std::path::PathBuf, String>> {
    WRITTEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The text that this process wrote to the wrapper at `path`.
#[cfg(unix)]
fn written_text(path: &Path) -> Option<String> {
    written_wrappers().get(path).cloned()
}

/// A system with no `sh` has no wrapper. There `RUSTFLAGS` replaces the flags of
/// the Cargo config, so it gets an addition. Without it, the config variable joins
/// the flags of the Cargo config.
#[cfg(not(unix))]
fn add_compiler_flags(command: &mut Command, _target_dir: &Path, extra: &str) {
    let flags = format!("{BUILD_FLAGS} {extra}");
    let flags = flags.trim_end();
    command.env("RUSTC_WRAPPER", "");
    match std::env::var("RUSTFLAGS") {
        Ok(own) => command.env("RUSTFLAGS", format!("{own} {flags}")),
        Err(_) => command.env("CARGO_BUILD_RUSTFLAGS", flags),
    };
    // rustdoc reads its own flags. A doctest build also reads the lint levels of
    // the crate, and the switch can start a lint that the crate denies.
    match std::env::var("RUSTDOCFLAGS") {
        Ok(own) => command.env("RUSTDOCFLAGS", format!("{own} {CAP_LINTS}")),
        Err(_) => command.env("CARGO_BUILD_RUSTDOCFLAGS", CAP_LINTS),
    };
}

/// Gives `command` the record of the active mutant.
///
/// On Unix a small shell opens the file on [`MUTANT_FD`] and then runs the command.
/// The process id stays the same. Each child inherits the descriptor, also a child
/// with an empty environment. Other systems read an environment variable.
#[cfg_attr(
    not(unix),
    expect(
        clippy::unnecessary_wraps,
        reason = "only the Unix form writes a file, and both forms have one signature"
    )
)]
pub fn with_active_mutant(command: &mut Command, record: &Path, mutant: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::write(record, mutant_record(mutant))?;
        let script = format!("exec {MUTANT_FD}<\"$1\"; shift; exec \"$@\"");
        let mut shell = new_command("sh");
        shell
            .args(["-c", &script, "sh"])
            .arg(record)
            .arg(command.get_program())
            .args(command.get_args());
        if let Some(dir) = command.get_current_dir() {
            shell.current_dir(dir);
        }
        for (key, value) in command.get_envs() {
            match value {
                Some(value) => shell.env(key, value),
                None => shell.env_remove(key),
            };
        }
        *command = shell;
    }
    #[cfg(not(unix))]
    {
        let _ = record;
        command.env("FALLOW_MUTANT", format!("{mutant:x}"));
    }
    Ok(())
}

/// The one place in the mutation module that creates a process.
pub fn new_command(program: impl AsRef<OsStr>) -> Command {
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
            let reason = watch.finish();
            if scope == Scope::Tests {
                guard.sweep_orphans();
            }
            return Ok(reason.map_or_else(|| Exit::Code(status.code()), Exit::Killed));
        }
        if Instant::now() >= deadline {
            let reason = watch.finish();
            kill_tree(child.id());
            child.kill()?;
            child.wait()?;
            if scope == Scope::Tests {
                guard.sweep_orphans();
            }
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
/// descendants. A process that is gone is no error. The process group of `root` also
/// gets the signal, so a failed read of the process table still stops most of a tree.
pub fn kill_tree(root: u32) {
    let table = system_probe().and_then(|probe| {
        signal("-STOP", &[root]);
        probe.table().ok()
    });
    kill_tree_with(root, table.as_deref());
}

fn kill_tree_with(root: u32, table: Option<&[Row]>) {
    let mut pids = table.map_or_else(Vec::new, |rows| descendants(rows, root));
    pids.push(root);
    signal("-KILL", &pids);
    kill_group(root);
}

/// The script that kills the process group `$1`. The `-s` form is POSIX: dash, the
/// `sh` of Debian and Ubuntu, refuses `--` after `-KILL`.
const KILL_GROUP: &str = "kill -s KILL -- \"-$1\" 2>/dev/null";

/// Kills the process group that a child of [`run_to_files`] leads.
fn kill_group(leader: u32) {
    // A process that is gone gives an error status, and that changes nothing.
    let _ = new_command("sh")
        .args(["-c", KILL_GROUP, "sh"])
        .arg(leader.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// One row of the table that [`list_commands`] reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRow {
    /// The process id.
    pub pid: u32,
    /// The id of the parent.
    pub ppid: u32,
    /// The command line.
    pub command: String,
}

/// Reads the command line of each process. A failed read gives an empty table,
/// because this table only finds more processes to stop.
pub fn list_commands() -> Vec<CommandRow> {
    new_command("ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()
        .map_or_else(
            |_| Vec::new(),
            |out| parse_commands(&String::from_utf8_lossy(&out.stdout)),
        )
}

fn parse_commands(text: &str) -> Vec<CommandRow> {
    text.lines()
        .filter_map(|line| {
            let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let (ppid, command) = rest.trim_start().split_once(char::is_whitespace)?;
            Some(CommandRow {
                pid: pid.parse().ok()?,
                ppid: ppid.parse().ok()?,
                command: command.trim_start().to_owned(),
            })
        })
        .collect()
}

/// The processes with a command line that starts in `folder`, through `pgrep`.
/// `pgrep` reads less data than `ps` with the command lines. `None` when the search
/// fails, so the caller reads the whole table.
pub fn processes_from(folder: &Path) -> Option<Vec<u32>> {
    let output = new_command("pgrep")
        .arg("-f")
        .arg(folder_pattern(folder))
        .stdin(Stdio::null())
        .output()
        .ok()?;
    match output.status.code() {
        Some(0) => Some(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| line.trim().parse().ok())
                .collect(),
        ),
        // No process matches.
        Some(1) => Some(Vec::new()),
        _ => None,
    }
}

/// The extended regular expression of a command line that starts in `folder`.
fn folder_pattern(folder: &Path) -> String {
    let mut pattern = String::from("^");
    for c in folder.to_string_lossy().chars() {
        if ".[]()*+?{}|^$\\".contains(c) {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('/');
    pattern
}

/// The processes that run a program from `folder` and that no tree in `roots` holds.
/// A test binary lives in that folder, so a mutated program cannot hide from this
/// list. A plain tool that a test starts can stay, and a plain tool has no mutant.
pub fn processes_under(table: &[CommandRow], folder: &Path, roots: &[u32]) -> Vec<u32> {
    let rows: Vec<Row> = table
        .iter()
        .map(|row| Row {
            pid: row.pid,
            ppid: row.ppid,
            rss_kib: 0,
        })
        .collect();
    let mut held: Vec<u32> = roots.to_vec();
    for root in roots {
        held.extend(descendants(&rows, *root));
    }
    let prefix = folder.to_string_lossy();
    table
        .iter()
        .filter(|row| {
            row.command
                .strip_prefix(prefix.as_ref())
                .is_some_and(|rest| rest.starts_with('/'))
                && !held.contains(&row.pid)
                && row.pid != std::process::id()
        })
        .map(|row| row.pid)
        .collect()
}

/// Sends a signal through the shell builtin, which needs no unsafe code and no
/// `kill` program in the image.
pub fn signal(signal: &str, pids: &[u32]) {
    if pids.is_empty() {
        return;
    }
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

    #[test]
    fn the_folder_pattern_escapes_each_special_character() {
        assert_eq!(
            folder_pattern(Path::new("/t/a.b+c(d)[e]{f}|g^h$i*j?k\\l")),
            "^/t/a\\.b\\+c\\(d\\)\\[e\\]\\{f\\}\\|g\\^h\\$i\\*j\\?k\\\\l/"
        );
    }

    /// The search finds a program that runs from the folder, and nothing in an
    /// empty folder.
    #[cfg(unix)]
    #[test]
    fn the_search_finds_a_program_that_runs_from_the_folder() {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let Ok(folder) = dir.path().canonicalize() else {
            panic!("a real path");
        };
        assert_eq!(processes_from(&folder), Some(Vec::new()));
        // A link starts the known program with a path in the folder. A new copy can
        // wait for a malware scan before its first start. The link keeps the name
        // `sleep`, because a multicall coreutils picks its tool from that name.
        let program = folder.join("sleep");
        let Ok(()) = std::os::unix::fs::symlink("/bin/sleep", &program) else {
            panic!("link a program");
        };
        let Ok(mut child) = new_command(&program).arg("47.4242").spawn() else {
            panic!("the link starts");
        };
        // The child can still be in its exec, so the search can need a moment.
        let start = Instant::now();
        let mut found = processes_from(&folder);
        while found.as_deref() != Some(&[child.id()]) && start.elapsed() < GONE_LIMIT {
            thread::sleep(POLL_INTERVAL);
            found = processes_from(&folder);
        }
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(found, Some(vec![child.id()]));
    }

    /// Each POSIX shell must run the group kill, also dash.
    #[cfg(unix)]
    #[test]
    fn each_shell_kills_the_group() {
        for shell in ["/bin/sh", "/bin/dash"] {
            if !Path::new(shell).exists() {
                continue;
            }
            let marker = format!("sleep 48.{}", shell.len());
            let mut command = new_command("sh");
            command.args(["-c", &format!("{marker} & wait")]);
            start_new_group(&mut command);
            let Ok(mut child) = command.spawn() else {
                panic!("sh must start");
            };
            thread::sleep(Duration::from_millis(200));
            assert!(alive(&marker), "the child must run");
            let _ = new_command(shell)
                .args(["-c", KILL_GROUP, "sh"])
                .arg(child.id().to_string())
                .status();
            let start = Instant::now();
            while child.try_wait().is_ok_and(|status| status.is_none()) {
                if start.elapsed() > GONE_LIMIT {
                    kill_tree_with(child.id(), None);
                    let _ = child.wait();
                    panic!("{shell} left the group alive");
                }
                thread::sleep(POLL_INTERVAL);
            }
            assert!(
                gone_within(&marker, GONE_LIMIT),
                "{shell} left the sleep alive"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_wrapper_with_new_text_replaces_the_old_text() {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let path = dir.path().join("target.rustc.sh");
        for body in ["exec first\n", "exec second\n"] {
            let Ok(()) = write_wrapper(&path, body) else {
                panic!("write the wrapper");
            };
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap_or_default(),
            "#!/bin/sh\nexec second\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_wrapper_replaces_a_file_that_exists_before_the_run() {
        use std::os::unix::fs::PermissionsExt;
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let path = dir.path().join("target.rustc.sh");
        let Ok(()) = std::fs::write(&path, "#!/bin/sh\necho planted\n") else {
            panic!("write the planted file");
        };
        let Ok(()) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)) else {
            panic!("set the mode");
        };
        let Ok(()) = write_wrapper(&path, "exec true\n") else {
            panic!("write the wrapper");
        };
        assert_eq!(
            std::fs::read_to_string(&path).unwrap_or_default(),
            "#!/bin/sh\nexec true\n"
        );
        let Ok(meta) = std::fs::metadata(&path) else {
            panic!("read the wrapper");
        };
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    }

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

    /// The time that a signalled process gets to end on a busy machine.
    #[cfg(unix)]
    const GONE_LIMIT: Duration = Duration::from_secs(5);

    #[cfg(unix)]
    fn gone_within(pattern: &str, limit: Duration) -> bool {
        let start = Instant::now();
        while alive(pattern) {
            if start.elapsed() > limit {
                return false;
            }
            thread::sleep(POLL_INTERVAL * 5);
        }
        true
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
        assert!(
            gone_within(marker, GONE_LIMIT),
            "the descendant must be gone"
        );
    }

    /// A failed read of the process table must still stop the test processes.
    #[cfg(unix)]
    #[test]
    fn with_no_process_table_the_group_of_the_root_gets_the_signal() {
        let marker = "sleep 47.3128";
        let mut command = new_command("sh");
        command.args(["-c", &format!("{marker} & wait")]);
        start_new_group(&mut command);
        let Ok(mut child) = command.spawn() else {
            panic!("sh must start");
        };
        thread::sleep(Duration::from_millis(200));
        assert!(alive(marker), "the child must run");
        kill_tree_with(child.id(), None);
        let _ = child.wait();
        assert!(gone_within(marker, GONE_LIMIT), "the child must be gone");
    }

    #[test]
    fn a_process_that_runs_from_the_folder_with_no_tree_is_an_orphan() {
        let table = parse_commands(
            "  10  1 /work/target/debug/deps/t-1 --exact a\n\
             \x20 20 30 /work/target/debug/deps/t-1 --exact b\n\
             \x20 40  1 /usr/bin/sleep 9\n\
             \x20 50  1 /work-other/x\n\
             not a row\n",
        );
        assert_eq!(table.len(), 4);
        assert_eq!(table[0].command, "/work/target/debug/deps/t-1 --exact a");
        let folder = Path::new("/work");
        assert_eq!(processes_under(&table, folder, &[30]), [10]);
        assert_eq!(processes_under(&table, folder, &[]), [10, 20]);
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
