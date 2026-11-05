//! The memory guard: a product feature, on by default.
//!
//! A mutant can allocate without limit, and a test timeout does not protect the
//! machine: on 2026-10-28 two mutants grew to 116 GB and 70 GB within 180 s, and
//! the Mac had a kernel panic. So two threads watch each process tree.
//!
//! The decision thread reads the footprint:
//!
//! - It kills a tree when a test process passes the per-process limit.
//! - It kills the largest tree when the free memory drops under the floor.
//! - It kills the trees when it cannot read the memory data, so a failure of the
//!   guard does not leave a mutant without a limit.
//!
//! A read of the footprint takes up to 0.7 s through the `top` fallback, and a
//! mutant can write more than 5 GiB in each second (both measured). So the second thread reads the
//! cheap RSS value often, and it stops a process above the limit until the decision
//! thread has the footprint. RSS never kills a process: it only stops growth.
//!
//! A watchdog process kills the trees when this process dies, because a test
//! process has no other limit after that.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::mutation::probe::{Probe, Row, system_probe};
use crate::mutation::process::{
    descendants, kill_tree, list_commands, new_command, processes_from, processes_under, signal,
};

/// The default limit for one test process: 4 GiB.
pub const DEFAULT_LIMIT_MIB: u64 = 4096;
/// The default floor of the free memory.
pub const DEFAULT_LOW_PERCENT: u32 = 25;
/// The time between two reads of RSS.
const RSS_TICK: Duration = Duration::from_millis(200);
/// The time between two searches for an orphan of a test.
const ORPHAN_TICK: Duration = Duration::from_secs(1);
/// The same time when a process is a candidate.
const CANDIDATE_RSS_TICK: Duration = Duration::from_millis(50);
/// The time between two checks of the decision thread.
const DECISION_TICK: Duration = Duration::from_millis(100);
/// The decision thread uses a table of the RSS thread up to this age. An older
/// table gets a new read.
const TABLE_AGE: Duration = Duration::from_millis(300);
/// The longest time between two reads of the footprint of a test tree. A
/// process with most of its pages compressed has a low RSS, so a candidate
/// filter alone can miss it.
const FOOTPRINT_EVERY: Duration = Duration::from_secs(5);
/// The same time when a process is a candidate.
const CANDIDATE_FOOTPRINT_EVERY: Duration = Duration::from_secs(1);
/// Under the floor plus this margin, each check reads the footprint.
const PRESSURE_MARGIN: u32 = 15;
/// The checks in a row without memory data before the guard stops the trees.
const BLIND_LIMIT: u32 = 5;

/// The settings of the guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The limit for one test process, in KiB.
    pub process_kib: u64,
    /// The floor of the free memory, in percent.
    pub low_percent: u32,
}

impl Limits {
    /// The settings from the values of the command line.
    pub const fn new(limit_mib: u64, low_percent: u32) -> Self {
        Self {
            process_kib: limit_mib.saturating_mul(1024),
            low_percent,
        }
    }

    /// A process at this RSS is a candidate for a read of the footprint.
    const fn candidate_kib(&self) -> u64 {
        self.process_kib / 8
    }
}

/// What the guard watches in a process tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// A build. The per-process limit does not apply, because a compiler is
    /// trusted and can need more. The floor of the free memory applies.
    Build,
    /// Test processes. All limits apply.
    Tests,
}

/// Why the guard killed a process tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillReason {
    /// A process passed the per-process limit.
    MemoryLimit {
        /// The footprint in KiB.
        footprint_kib: u64,
        /// The limit in KiB.
        limit_kib: u64,
    },
    /// The free memory dropped under the floor, and this tree was the largest.
    LowMemory {
        /// The free memory in percent.
        free_percent: u32,
        /// The floor in percent.
        floor_percent: u32,
        /// The footprint of the largest process, in KiB.
        footprint_kib: u64,
    },
    /// The guard could not read the memory data.
    Blind,
}

impl fmt::Display for KillReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MemoryLimit {
                footprint_kib,
                limit_kib,
            } => write!(
                f,
                "memory guard: a test process reached {} MiB, over the limit of {} MiB",
                footprint_kib / 1024,
                limit_kib / 1024
            ),
            Self::LowMemory {
                free_percent,
                floor_percent,
                footprint_kib,
            } => write!(
                f,
                "memory guard: free memory is {free_percent}%, under the floor of \
                 {floor_percent}%, and this process was the largest at {} MiB",
                footprint_kib / 1024
            ),
            Self::Blind => write!(f, "memory guard: it cannot read the memory data"),
        }
    }
}

/// One watched tree in a check.
#[derive(Debug, Clone)]
struct Tree {
    ticket: u64,
    scope: Scope,
    pids: Vec<u32>,
}

/// Selects the trees to kill. A tree can only have one reason.
fn decide(
    trees: &[Tree],
    footprints: &BTreeMap<u32, u64>,
    free_percent: Option<u32>,
    limits: Limits,
) -> Vec<(u64, KillReason)> {
    let largest = |tree: &Tree| {
        tree.pids
            .iter()
            .filter_map(|pid| footprints.get(pid).copied())
            .max()
            .unwrap_or(0)
    };
    let over: Vec<(u64, KillReason)> = trees
        .iter()
        .filter(|tree| tree.scope == Scope::Tests)
        .filter(|tree| largest(tree) > limits.process_kib)
        .map(|tree| {
            let reason = KillReason::MemoryLimit {
                footprint_kib: largest(tree),
                limit_kib: limits.process_kib,
            };
            (tree.ticket, reason)
        })
        .collect();
    if !over.is_empty() {
        return over;
    }
    let Some(free) = free_percent.filter(|&free| free < limits.low_percent) else {
        return Vec::new();
    };
    trees
        .iter()
        .max_by_key(|tree| largest(tree))
        .map(|tree| {
            let reason = KillReason::LowMemory {
                free_percent: free,
                floor_percent: limits.low_percent,
                footprint_kib: largest(tree),
            };
            (tree.ticket, reason)
        })
        .into_iter()
        .collect()
}

#[derive(Debug)]
struct Entry {
    root: u32,
    scope: Scope,
    reason: Option<KillReason>,
}

#[derive(Default)]
struct Registry {
    next: u64,
    entries: BTreeMap<u64, Entry>,
    kills: u32,
    /// The processes that the RSS thread stopped. The decision thread resumes the
    /// ones that it does not kill.
    stopped: BTreeSet<u32>,
    /// The RSS of each process with a small footprint at its last check. The RSS
    /// thread stops such a process again only after more growth.
    cleared: BTreeMap<u32, u64>,
    /// The file for the watchdog. It holds the root of each tree.
    roots_file: Option<PathBuf>,
    /// The folder of the run.
    run_folder: Option<PathBuf>,
}

impl Registry {
    /// Writes the roots through a rename, so that the watchdog never reads half
    /// a file. A failed write leaves the last list, and the guard still runs.
    fn write_roots(&self) {
        let Some(path) = &self.roots_file else {
            return;
        };
        let text: Vec<String> = self.entries.values().map(|e| e.root.to_string()).collect();
        let temporary = path.with_extension("tmp");
        if fs::write(&temporary, text.join("\n")).is_ok() {
            let _ = fs::rename(&temporary, path);
        }
    }
}

struct Shared {
    registry: Mutex<Registry>,
    stop: AtomicBool,
    /// The last table of the RSS thread, with the time of the read.
    table: Mutex<Option<(Instant, Arc<[Row]>)>>,
    /// The last value of the free memory, with the time of the read.
    free: Mutex<Option<(Instant, Option<u32>)>>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn keep_table(&self, rows: Vec<Row>) -> Arc<[Row]> {
        let rows: Arc<[Row]> = Arc::from(rows);
        *self.table.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((Instant::now(), Arc::clone(&rows)));
        rows
    }

    /// Reads the free memory and keeps the value for the other thread.
    fn read_free(&self, probe: &dyn Probe) -> Option<u32> {
        let free = probe.free_percent();
        *self.free.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), free));
        free
    }

    /// The last value of the free memory when it is new enough, else a new read.
    /// Each read starts `sysctl` on macOS. Free memory can drop fast during a
    /// build, so a value is new enough for one decision tick only.
    fn fresh_free(&self, probe: &dyn Probe) -> Option<u32> {
        let kept = *self.free.lock().unwrap_or_else(PoisonError::into_inner);
        match kept {
            Some((at, free)) if at.elapsed() <= DECISION_TICK => free,
            _ => self.read_free(probe),
        }
    }

    /// The last table of the RSS thread when it is new enough, else a new read.
    /// Each read starts `ps`, and `ps` reads each process of the system.
    fn fresh_table(&self, probe: &dyn Probe) -> io::Result<Arc<[Row]>> {
        let kept = self
            .table
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .filter(|(at, _)| at.elapsed() <= TABLE_AGE)
            .map(|(_, rows)| Arc::clone(rows));
        match kept {
            Some(rows) => Ok(rows),
            None => probe.table().map(|rows| self.keep_table(rows)),
        }
    }
}

/// The running guard.
pub struct Guard {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    watchdog: Option<Child>,
    probe: Option<Arc<dyn Probe>>,
    limits: Limits,
}

impl Guard {
    /// A guard that watches nothing. A time limit still applies to each child.
    pub fn disabled() -> Self {
        Self {
            shared: Arc::new(Shared {
                registry: Mutex::new(Registry::default()),
                stop: AtomicBool::new(false),
                table: Mutex::new(None),
                free: Mutex::new(None),
            }),
            threads: Vec::new(),
            watchdog: None,
            probe: None,
            limits: Limits::new(DEFAULT_LIMIT_MIB, DEFAULT_LOW_PERCENT),
        }
    }

    /// Starts the guard with the probe of this system.
    ///
    /// Returns an error when the system has no probe, or when the probe cannot
    /// read this process: a run with no memory data must not start.
    pub fn start(limits: Limits) -> io::Result<Self> {
        let probe = system_probe().ok_or_else(|| {
            io::Error::other("the memory guard has no probe for this operating system")
        })?;
        let me = std::process::id();
        if !probe.footprints(&[me])?.contains_key(&me) {
            return Err(io::Error::other("the memory guard cannot read a footprint"));
        }
        Ok(Self::with_probe(limits, Arc::from(probe)))
    }

    fn with_probe(limits: Limits, probe: Arc<dyn Probe>) -> Self {
        let mut guard = Self::disabled();
        guard.limits = limits;
        guard.probe = Some(Arc::clone(&probe));
        let (shared, rss_probe) = (Arc::clone(&guard.shared), Arc::clone(&probe));
        guard.threads.push(thread::spawn(move || {
            rss_loop(&shared, &*rss_probe, limits);
        }));
        let shared = Arc::clone(&guard.shared);
        guard.threads.push(thread::spawn(move || {
            decision_loop(&shared, &*probe, limits);
        }));
        guard
    }

    /// Starts the watchdog process. `exe` is this program, and `roots` is a new
    /// file for the list of roots. `folder` is the folder of the run. The watchdog
    /// removes it when `remove` is true and the parent dies.
    pub fn start_watchdog(
        &mut self,
        exe: &Path,
        roots: PathBuf,
        folder: &Path,
        remove: bool,
    ) -> io::Result<()> {
        fs::write(&roots, "")?;
        let mut command = new_command(exe);
        command
            .arg("mutation-watchdog")
            .arg(&roots)
            .arg("--sweep")
            .arg(folder)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if remove {
            command.arg("--remove");
        }
        // Ctrl-C goes to the foreground group. The watchdog must outlive it.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        self.watchdog = Some(command.spawn()?);
        self.shared.lock().roots_file = Some(roots);
        Ok(())
    }

    /// Adds a tree to the guard. The return value removes it again.
    pub fn watch(&self, root: u32, scope: Scope) -> Watch<'_> {
        let mut registry = self.shared.lock();
        registry.next += 1;
        let ticket = registry.next;
        registry.entries.insert(
            ticket,
            Entry {
                root,
                scope,
                reason: None,
            },
        );
        registry.write_roots();
        drop(registry);
        Watch {
            guard: self,
            ticket,
        }
    }

    /// Sets the folder of the run. A process that runs from this folder and has no
    /// watched parent is an orphan of a test.
    pub fn set_run_folder(&self, folder: &Path) {
        self.shared.lock().run_folder = Some(folder.to_path_buf());
    }

    /// Kills each orphan of a test. A test can leave a child that runs on after the
    /// test ends. That child has a new parent, so no tree holds it, and no limit
    /// watches it, but it can still grow.
    pub fn sweep_orphans(&self) {
        sweep_orphans(&self.shared, self.probe.as_deref());
    }

    /// The number of trees that the guard killed.
    pub fn kills(&self) -> u32 {
        self.shared.lock().kills
    }

    /// True when the free memory is above the floor, or when the guard is off.
    pub fn memory_is_free(&self) -> bool {
        self.probe.as_ref().is_none_or(|probe| {
            self.shared
                .fresh_free(&**probe)
                .is_none_or(|free| free >= self.limits.low_percent)
        })
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        for thread in self.threads.drain(..) {
            // A panic of the thread has no more effect here.
            let _ = thread.join();
        }
        let roots_file = {
            let mut registry = self.shared.lock();
            registry.entries.clear();
            registry.write_roots();
            registry.roots_file.take()
        };
        if let Some(mut watchdog) = self.watchdog.take() {
            // The closed pipe ends the watchdog, and the list is empty.
            drop(watchdog.stdin.take());
            let _ = watchdog.wait();
        }
        if let Some(path) = roots_file {
            let _ = fs::remove_file(path);
        }
    }
}

/// A watched process tree.
pub struct Watch<'a> {
    guard: &'a Guard,
    ticket: u64,
}

impl Watch<'_> {
    /// Removes the tree, and returns the reason when the guard killed it.
    pub fn finish(self) -> Option<KillReason> {
        let mut registry = self.guard.shared.lock();
        let entry = registry.entries.remove(&self.ticket);
        registry.write_roots();
        drop(registry);
        entry.and_then(|entry| entry.reason)
    }
}

/// The watched roots that the guard did not kill yet.
fn current_roots(shared: &Shared) -> Vec<(u64, u32, Scope)> {
    shared
        .lock()
        .entries
        .iter()
        .filter(|(_, entry)| entry.reason.is_none())
        .map(|(&ticket, entry)| (ticket, entry.root, entry.scope))
        .collect()
}

fn trees_of(rows: &[Row], roots: &[(u64, u32, Scope)]) -> Vec<Tree> {
    roots
        .iter()
        .map(|&(ticket, root, scope)| {
            let mut pids = descendants(rows, root);
            pids.push(root);
            Tree {
                ticket,
                scope,
                pids,
            }
        })
        .collect()
}

fn rss_of(rows: &[Row], pid: u32) -> u64 {
    rows.iter()
        .find(|row| row.pid == pid)
        .map_or(0, |row| row.rss_kib)
}

fn sweep_orphans(shared: &Shared, probe: Option<&dyn Probe>) {
    let Some(folder) = shared.lock().run_folder.clone() else {
        return;
    };
    let roots = || -> Vec<u32> { shared.lock().entries.values().map(|e| e.root).collect() };
    // The search and the table come first. A tree that starts later has no
    // process in them. A failed search reads the command line of each process.
    let Some(found) = processes_from(&folder) else {
        let table = list_commands();
        signal("-KILL", &processes_under(&table, &folder, &roots()));
        return;
    };
    if found.is_empty() {
        return;
    }
    // The table of the parents comes after the search, so it holds each process
    // that the search found.
    let Some(rows) = probe.and_then(|probe| probe.table().ok()) else {
        let table = list_commands();
        signal("-KILL", &processes_under(&table, &folder, &roots()));
        return;
    };
    let roots = roots();
    let mut held = roots.clone();
    for root in &roots {
        held.extend(descendants(&rows, *root));
    }
    let me = std::process::id();
    let orphans: Vec<u32> = found
        .into_iter()
        .filter(|pid| *pid != me && !held.contains(pid))
        .collect();
    signal("-KILL", &orphans);
}

/// Reads RSS often and stops each process that needs a decision. A stopped process
/// cannot grow while the decision thread waits for the footprint.
fn rss_loop(shared: &Shared, probe: &dyn Probe, limits: Limits) {
    let mut blind = 0;
    let mut last_sweep = Instant::now();
    while !shared.stop.load(Ordering::SeqCst) {
        let roots = current_roots(shared);
        if roots.is_empty() {
            thread::sleep(RSS_TICK);
            continue;
        }
        // An orphan can grow during a long run, and no tree holds it.
        if last_sweep.elapsed() >= ORPHAN_TICK {
            last_sweep = Instant::now();
            sweep_orphans(shared, Some(probe));
        }
        thread::sleep(rss_check(shared, probe, &roots, limits, &mut blind));
    }
}

/// One RSS check of the running processes. The result is the time to the next
/// check. `blind` counts the checks in a row that could not read the process
/// table.
fn rss_check(
    shared: &Shared,
    probe: &dyn Probe,
    roots: &[(u64, u32, Scope)],
    limits: Limits,
    blind: &mut u32,
) -> Duration {
    let free = shared.read_free(probe);
    if !table_needed(shared, roots, free, limits) {
        *blind = 0;
        return RSS_TICK;
    }
    let Ok(rows) = probe.table() else {
        *blind += 1;
        if *blind >= BLIND_LIMIT {
            for &(ticket, _, _) in roots {
                kill(shared, ticket, KillReason::Blind);
            }
        }
        return RSS_TICK;
    };
    let rows = shared.keep_table(rows);
    *blind = 0;
    let trees = trees_of(&rows, roots);
    let (candidate, to_stop) = rss_decision(shared, &trees, &rows, free, limits);
    if !to_stop.is_empty() {
        signal("-STOP", &to_stop);
    }
    if candidate {
        CANDIDATE_RSS_TICK
    } else {
        RSS_TICK
    }
}

/// True when a check needs the process table. Only a test process has a limit of
/// its own. A build only counts under memory pressure, so a run of builds with free
/// memory does not read the table. Each read starts `ps`, and `ps` reads each
/// process of the system.
fn table_needed(
    shared: &Shared,
    roots: &[(u64, u32, Scope)],
    free: Option<u32>,
    limits: Limits,
) -> bool {
    roots.iter().any(|&(_, _, scope)| scope == Scope::Tests)
        || free.is_none_or(|free| free < limits.low_percent + PRESSURE_MARGIN)
        || !shared.lock().stopped.is_empty()
}

/// Returns true when a test process is a candidate, and the processes to stop.
fn rss_decision(
    shared: &Shared,
    trees: &[Tree],
    rows: &[Row],
    free: Option<u32>,
    limits: Limits,
) -> (bool, Vec<u32>) {
    let test_pids = || {
        trees
            .iter()
            .filter(|tree| tree.scope == Scope::Tests)
            .flat_map(|tree| tree.pids.iter().copied())
    };
    let candidate = test_pids().any(|pid| rss_of(rows, pid) >= limits.candidate_kib());
    let mut registry = shared.lock();
    let margin = limits.process_kib / 4;
    let mut to_stop: Vec<u32> = test_pids()
        .filter(|&pid| rss_of(rows, pid) > limits.process_kib)
        .filter(|pid| !registry.stopped.contains(pid))
        .filter(|pid| {
            registry
                .cleared
                .get(pid)
                .is_none_or(|&rss| rss_of(rows, *pid) >= rss + margin)
        })
        .collect();
    if free.is_some_and(|free| free < limits.low_percent) {
        let largest = trees
            .iter()
            .flat_map(|tree| tree.pids.iter().copied())
            .max_by_key(|&pid| rss_of(rows, pid));
        to_stop.extend(largest.filter(|pid| !registry.stopped.contains(pid)));
    }
    to_stop.sort_unstable();
    to_stop.dedup();
    registry.stopped.extend(to_stop.iter().copied());
    drop(registry);
    (candidate, to_stop)
}

fn decision_loop(shared: &Shared, probe: &dyn Probe, limits: Limits) {
    let mut state = WatchState {
        last_footprint: None,
        blind: 0,
    };
    while !shared.stop.load(Ordering::SeqCst) {
        state.tick(shared, probe, limits);
        thread::sleep(DECISION_TICK);
    }
}

struct WatchState {
    last_footprint: Option<Instant>,
    blind: u32,
}

impl WatchState {
    fn tick(&mut self, shared: &Shared, probe: &dyn Probe, limits: Limits) {
        let roots = current_roots(shared);
        if roots.is_empty() {
            self.blind = 0;
            resume_stopped(shared);
            return;
        }
        let free = shared.fresh_free(probe);
        if !table_needed(shared, &roots, free, limits) {
            self.blind = 0;
            return;
        }
        let Ok(rows) = shared.fresh_table(probe) else {
            return self.count_blind(shared, &roots);
        };
        let trees = trees_of(&rows, &roots);
        if !self.needs_footprint(shared, &trees, &rows, free, limits) {
            return;
        }
        // A test process can end within the age of a shared table. A read of the
        // footprint of ended processes gives no data, so it gets a new table.
        let Ok(rows) = probe.table().map(|rows| shared.keep_table(rows)) else {
            return self.count_blind(shared, &roots);
        };
        let trees = trees_of(&rows, &roots);
        let pids: Vec<u32> = trees.iter().flat_map(|tree| tree.pids.clone()).collect();
        let read = probe.footprints(&pids);
        // A read can take 0.7 s, so a fast test run can end during the read.
        // Only a tree that ran through the whole read must have a row.
        let live = live_roots(shared, &roots);
        let footprints = match read {
            Ok(footprints) if !footprints.is_empty() => footprints,
            Ok(_) if live.is_empty() => return,
            _ => return self.count_blind(shared, &live),
        };
        self.blind = 0;
        self.last_footprint = Some(Instant::now());
        for (ticket, reason) in decide(&trees, &footprints, free, limits) {
            kill(shared, ticket, reason);
        }
        note_cleared(shared, &rows);
        resume_stopped(shared);
    }

    fn needs_footprint(
        &self,
        shared: &Shared,
        trees: &[Tree],
        rows: &[Row],
        free: Option<u32>,
        limits: Limits,
    ) -> bool {
        if !shared.lock().stopped.is_empty() {
            return true;
        }
        let pressure = free.is_some_and(|free| free < limits.low_percent + PRESSURE_MARGIN);
        let tests: Vec<&Tree> = trees.iter().filter(|t| t.scope == Scope::Tests).collect();
        let candidate = tests.iter().any(|tree| {
            tree.pids
                .iter()
                .any(|&pid| rss_of(rows, pid) >= limits.candidate_kib())
        });
        let period = if candidate {
            CANDIDATE_FOOTPRINT_EVERY
        } else {
            FOOTPRINT_EVERY
        };
        let due = !tests.is_empty()
            && self
                .last_footprint
                .is_none_or(|last| last.elapsed() >= period);
        pressure || due
    }

    /// Stops all trees after too many checks in a row without memory data.
    fn count_blind(&mut self, shared: &Shared, roots: &[(u64, u32, Scope)]) {
        self.blind += 1;
        if self.blind < BLIND_LIMIT {
            return;
        }
        for &(ticket, _, _) in roots {
            kill(shared, ticket, KillReason::Blind);
        }
    }
}

/// Remembers the RSS of each stopped process that the decision did not kill.
fn note_cleared(shared: &Shared, rows: &[Row]) {
    let mut registry = shared.lock();
    let stopped: Vec<u32> = registry.stopped.iter().copied().collect();
    for pid in stopped {
        let rss = rss_of(rows, pid);
        registry.cleared.insert(pid, rss);
    }
}

/// Resumes each stopped process. A process of a killed tree is gone, and a signal
/// to it changes nothing.
fn resume_stopped(shared: &Shared) {
    let mut registry = shared.lock();
    let stopped = std::mem::take(&mut registry.stopped);
    if registry.entries.is_empty() {
        registry.cleared.clear();
    }
    drop(registry);
    if !stopped.is_empty() {
        signal("-CONT", &stopped.into_iter().collect::<Vec<u32>>());
    }
}

/// The roots of a snapshot that are still watched.
fn live_roots(shared: &Shared, roots: &[(u64, u32, Scope)]) -> Vec<(u64, u32, Scope)> {
    let registry = shared.lock();
    roots
        .iter()
        .filter(|root| registry.entries.contains_key(&root.0))
        .copied()
        .collect()
}

/// Kills a tree that is still watched. The lock stays held during the kill, so
/// the watch cannot end, and the process id cannot belong to another process.
fn kill(shared: &Shared, ticket: u64, reason: KillReason) {
    let mut registry = shared.lock();
    let Some(entry) = registry.entries.get_mut(&ticket) else {
        return;
    };
    entry.reason = Some(reason);
    let root = entry.root;
    registry.kills += 1;
    kill_tree(root);
    drop(registry);
}

/// The hidden `mutation-watchdog` command. It waits until the pipe on stdin
/// closes: that is the end of the parent, in any way. Then it kills each tree
/// that the file lists and each process that runs from the folder of the run, and it
/// removes that folder when `remove` is true. The parent can end through a signal,
/// so it cannot do this work itself. The folder can hold many GiB.
pub fn run_watchdog(roots: &Path, sweep: Option<&Path>, remove: bool) {
    let mut sink = Vec::new();
    // Each result of the read means that the parent is gone or that the pipe is
    // broken. Both end the wait.
    let _ = io::stdin().read_to_end(&mut sink);
    if let Ok(text) = fs::read_to_string(roots) {
        for root in text.lines().filter_map(|line| line.parse().ok()) {
            kill_tree(root);
        }
    }
    let Some(folder) = sweep else {
        return;
    };
    let table = list_commands();
    signal("-KILL", &processes_under(&table, folder, &[]));
    if remove {
        // A failed removal only leaves files in the temporary directory of the system.
        let _ = fs::remove_dir_all(folder);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU32;

    use super::*;
    #[cfg(unix)]
    use crate::mutation::process::{Exit, run_to_files};

    const LIMITS: Limits = Limits::new(100, 25);
    const OVER: u64 = 101 * 1024;

    fn tree(ticket: u64, scope: Scope, pids: &[u32]) -> Tree {
        Tree {
            ticket,
            scope,
            pids: pids.to_vec(),
        }
    }

    #[test]
    fn a_test_process_over_the_limit_is_killed_with_a_clear_reason() {
        let trees = [
            tree(1, Scope::Tests, &[10, 11]),
            tree(2, Scope::Tests, &[20]),
        ];
        let footprints = BTreeMap::from([(10, 1), (11, OVER), (20, 5)]);
        let kills = decide(&trees, &footprints, Some(80), LIMITS);
        assert_eq!(kills.len(), 1);
        assert_eq!(kills[0].0, 1);
        assert_eq!(
            kills[0].1.to_string(),
            "memory guard: a test process reached 101 MiB, over the limit of 100 MiB"
        );
    }

    #[test]
    fn the_limit_itself_does_not_kill() {
        let trees = [tree(1, Scope::Tests, &[10])];
        let footprints = BTreeMap::from([(10, 100 * 1024)]);
        assert!(decide(&trees, &footprints, Some(80), LIMITS).is_empty());
    }

    #[test]
    fn a_build_over_the_limit_is_not_killed() {
        let trees = [tree(1, Scope::Build, &[10])];
        let footprints = BTreeMap::from([(10, OVER)]);
        assert!(decide(&trees, &footprints, Some(80), LIMITS).is_empty());
    }

    #[test]
    fn low_memory_kills_only_the_largest_tree_of_any_scope() {
        let trees = [
            tree(1, Scope::Tests, &[10]),
            tree(2, Scope::Build, &[20]),
            tree(3, Scope::Tests, &[30]),
        ];
        let footprints = BTreeMap::from([(10, 5), (20, 900), (30, 7)]);
        let kills = decide(&trees, &footprints, Some(24), LIMITS);
        assert_eq!(kills.len(), 1);
        assert_eq!(kills[0].0, 2);
        assert!(matches!(
            kills[0].1,
            KillReason::LowMemory {
                free_percent: 24,
                floor_percent: 25,
                footprint_kib: 900
            }
        ));
    }

    #[test]
    fn the_floor_itself_kills_nothing_and_a_missing_value_kills_nothing() {
        let trees = [tree(1, Scope::Tests, &[10])];
        let footprints = BTreeMap::from([(10, 5)]);
        assert!(decide(&trees, &footprints, Some(25), LIMITS).is_empty());
        assert!(decide(&trees, &footprints, None, LIMITS).is_empty());
    }

    /// A probe with fixed answers.
    struct FakeProbe {
        rss_kib: u64,
        footprint: Result<u64, ()>,
        free: AtomicU32,
        footprint_reads: AtomicU32,
        table_reads: AtomicU32,
        free_reads: AtomicU32,
        /// The time of one read of the footprint. `top` takes about 0.7 s.
        delay: Duration,
        /// True when a read lists no process.
        empty: bool,
    }

    impl FakeProbe {
        fn new(rss_kib: u64, footprint: Result<u64, ()>, free: u32) -> Arc<Self> {
            Arc::new(Self {
                rss_kib,
                footprint,
                free: AtomicU32::new(free),
                footprint_reads: AtomicU32::new(0),
                table_reads: AtomicU32::new(0),
                free_reads: AtomicU32::new(0),
                delay: Duration::ZERO,
                empty: false,
            })
        }
    }

    impl Probe for FakeProbe {
        fn table(&self) -> io::Result<Vec<Row>> {
            self.table_reads.fetch_add(1, Ordering::SeqCst);
            // The real table holds the root, so the fake reads it from `ps`.
            let Some(real) = system_probe() else {
                return Ok(Vec::new());
            };
            Ok(real
                .table()?
                .into_iter()
                .map(|row| Row {
                    rss_kib: self.rss_kib,
                    ..row
                })
                .collect())
        }

        fn footprints(&self, pids: &[u32]) -> io::Result<BTreeMap<u32, u64>> {
            self.footprint_reads.fetch_add(1, Ordering::SeqCst);
            thread::sleep(self.delay);
            if self.empty {
                return Ok(BTreeMap::new());
            }
            let kib = self.footprint.map_err(|()| io::Error::other("fake"))?;
            Ok(pids.iter().map(|&pid| (pid, kib)).collect())
        }

        fn free_percent(&self) -> Option<u32> {
            self.free_reads.fetch_add(1, Ordering::SeqCst);
            Some(self.free.load(Ordering::SeqCst))
        }
    }

    #[cfg(unix)]
    fn sleep_under(guard: &Guard, scope: Scope, limit: Duration) -> Exit {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let mut command = new_command("sh");
        command.args(["-c", "sleep 30 & wait"]);
        let Ok(exit) = run_to_files(
            command,
            &dir.path().join("out"),
            &dir.path().join("err"),
            limit,
            guard,
            scope,
        ) else {
            panic!("sh must start");
        };
        exit
    }

    #[cfg(unix)]
    const LONG: Duration = Duration::from_secs(25);

    #[cfg(unix)]
    #[test]
    fn the_thread_kills_a_tree_over_the_limit() {
        let guard = Guard::with_probe(LIMITS, FakeProbe::new(OVER, Ok(OVER), 80));
        let start = Instant::now();
        let exit = sleep_under(&guard, Scope::Tests, LONG);
        assert!(
            matches!(exit, Exit::Killed(KillReason::MemoryLimit { .. })),
            "{exit:?}"
        );
        assert!(start.elapsed() < Duration::from_secs(15));
        assert_eq!(guard.kills(), 1);
    }

    /// A read of `top` takes about 0.7 s, and a test process without a sleep can
    /// grow by GiBs in that time. The RSS thread stops the process at the first
    /// table over the limit, before the slow read of the footprint ends.
    #[cfg(unix)]
    #[test]
    fn the_rss_thread_stops_a_process_before_a_slow_read_of_top_ends() {
        let read_time = Duration::from_secs(3);
        let probe = Arc::new(FakeProbe {
            rss_kib: OVER,
            footprint: Ok(OVER),
            free: AtomicU32::new(80),
            footprint_reads: AtomicU32::new(0),
            table_reads: AtomicU32::new(0),
            free_reads: AtomicU32::new(0),
            delay: read_time,
            empty: false,
        });
        let guard = Guard::with_probe(LIMITS, probe);
        let start = Instant::now();
        let (stopped_after, exit) = thread::scope(|scope| {
            let run = scope.spawn(|| sleep_under(&guard, Scope::Tests, LONG));
            let mut stopped_after = None;
            while stopped_after.is_none() && start.elapsed() < read_time {
                if !guard.shared.lock().stopped.is_empty() {
                    stopped_after = Some(start.elapsed());
                }
                thread::sleep(Duration::from_millis(10));
            }
            (stopped_after, run.join())
        });
        let Ok(exit) = exit else {
            panic!("the run thread must not panic");
        };
        let Some(stopped_after) = stopped_after else {
            panic!("no stop before the read of top ended: {exit:?}");
        };
        assert!(stopped_after < read_time / 2, "{stopped_after:?}");
        assert!(
            matches!(exit, Exit::Killed(KillReason::MemoryLimit { .. })),
            "{exit:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_thread_fails_closed_when_it_cannot_read_a_footprint() {
        let guard = Guard::with_probe(LIMITS, FakeProbe::new(OVER, Err(()), 80));
        let exit = sleep_under(&guard, Scope::Tests, LONG);
        assert_eq!(exit, Exit::Killed(KillReason::Blind));
    }

    /// A child that outlives its test can grow during the rest of the run.
    #[cfg(unix)]
    #[test]
    fn the_thread_kills_an_orphan_during_a_run() {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let Ok(folder) = dir.path().canonicalize() else {
            panic!("a real path");
        };
        let program = folder.join("copy-of-sleep");
        let Ok(_) = fs::copy("/bin/sleep", &program) else {
            panic!("copy a program");
        };
        let Ok(mut orphan) = new_command(&program).arg("47.3129").spawn() else {
            panic!("the copy starts");
        };
        let guard = Guard::with_probe(LIMITS, FakeProbe::new(0, Ok(0), 80));
        guard.set_run_folder(&folder);
        let exit = sleep_under(&guard, Scope::Tests, Duration::from_secs(4));
        assert_eq!(exit, Exit::TimedOut);
        let waited = orphan.try_wait();
        let _ = orphan.kill();
        let _ = orphan.wait();
        assert!(
            matches!(waited, Ok(Some(_))),
            "the orphan still runs: {waited:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_small_tree_stays_without_a_read_of_top_until_the_period_ends() {
        let probe = FakeProbe::new(1, Ok(1), 80);
        let reads = Arc::clone(&probe);
        let guard = Guard::with_probe(LIMITS, probe);
        let exit = sleep_under(&guard, Scope::Tests, Duration::from_millis(1500));
        assert_eq!(exit, Exit::TimedOut);
        // One read at the start, and the period is longer than this run.
        assert_eq!(reads.footprint_reads.load(Ordering::SeqCst), 1);
    }

    /// Each read of the table starts `ps`, and `ps` reads each process of the
    /// system. The two threads share one read.
    /// Free memory can drop fast during a build, so a shared value may be only one
    /// decision tick old.
    #[test]
    fn a_free_memory_value_older_than_a_decision_tick_is_read_again() {
        let probe = FakeProbe::new(1, Ok(1), 80);
        let guard = Guard::disabled();
        assert_eq!(guard.shared.fresh_free(probe.as_ref()), Some(80));
        probe.free.store(10, Ordering::SeqCst);
        thread::sleep(DECISION_TICK + Duration::from_millis(50));
        assert_eq!(guard.shared.fresh_free(probe.as_ref()), Some(10));
    }

    #[cfg(unix)]
    #[test]
    fn the_threads_share_the_reads_of_the_table() {
        let probe = FakeProbe::new(1, Ok(1), 80);
        let reads = Arc::clone(&probe);
        let guard = Guard::with_probe(LIMITS, probe);
        let exit = sleep_under(&guard, Scope::Tests, Duration::from_millis(2000));
        assert_eq!(exit, Exit::TimedOut);
        // The RSS thread reads at most 10 times in 2 s. The decision thread alone
        // reads up to 20 times.
        let count = reads.table_reads.load(Ordering::SeqCst);
        assert!(count <= 12, "{count} reads of the table");
        // Each read of the free memory starts `sysctl`, so the threads share it too.
        // A value is new enough for one decision tick, so the decision thread
        // reads about 20 times in 2 s and the RSS thread mostly reuses its value.
        let free = reads.free_reads.load(Ordering::SeqCst);
        assert!(free <= 26, "{free} reads of the free memory");
    }

    #[cfg(unix)]
    #[test]
    fn a_build_with_free_memory_gets_no_read_of_top() {
        let probe = FakeProbe::new(OVER, Ok(OVER), 80);
        let reads = Arc::clone(&probe);
        let guard = Guard::with_probe(LIMITS, probe);
        let exit = sleep_under(&guard, Scope::Build, Duration::from_millis(600));
        assert_eq!(exit, Exit::TimedOut);
        assert_eq!(reads.footprint_reads.load(Ordering::SeqCst), 0);
        // A build has no per-process limit, so with free memory the guard does not
        // need the table.
        assert_eq!(reads.table_reads.load(Ordering::SeqCst), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_tree_under_low_memory_is_killed() {
        let guard = Guard::with_probe(LIMITS, FakeProbe::new(1, Ok(1), 10));
        let exit = sleep_under(&guard, Scope::Build, LONG);
        assert!(
            matches!(exit, Exit::Killed(KillReason::LowMemory { .. })),
            "{exit:?}"
        );
    }

    /// `top` reports nothing for a process that is gone, and a fast test run
    /// ends before `top` does. That says nothing about the guard, and a kill of
    /// the old process id can hit another process.
    #[cfg(unix)]
    #[test]
    fn a_tree_that_ends_during_a_read_is_no_reason_to_kill() {
        let mut fake = FakeProbe::new(OVER, Ok(1), 80);
        let Some(probe) = Arc::get_mut(&mut fake) else {
            panic!("the fake has one owner");
        };
        probe.delay = Duration::from_millis(60);
        probe.empty = true;
        let guard = Guard::with_probe(LIMITS, fake);
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(3) {
            let Ok(dir) = tempfile::tempdir() else {
                panic!("a temporary directory");
            };
            let mut command = new_command("sh");
            command.args(["-c", "exit 0"]);
            let Ok(exit) = run_to_files(
                command,
                &dir.path().join("out"),
                &dir.path().join("err"),
                LONG,
                &guard,
                Scope::Tests,
            ) else {
                panic!("sh must start");
            };
            assert_eq!(exit, Exit::Code(Some(0)));
        }
        assert_eq!(guard.kills(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_read_with_no_row_for_a_live_tree_is_blind() {
        let mut fake = FakeProbe::new(OVER, Ok(1), 80);
        let Some(probe) = Arc::get_mut(&mut fake) else {
            panic!("the fake has one owner");
        };
        probe.empty = true;
        let guard = Guard::with_probe(LIMITS, fake);
        let exit = sleep_under(&guard, Scope::Tests, LONG);
        assert_eq!(exit, Exit::Killed(KillReason::Blind));
    }

    /// A large file mapping counts in RSS and not in the footprint. RSS stops the
    /// process only until `top` has the footprint.
    #[cfg(unix)]
    #[test]
    fn a_process_with_a_large_rss_and_a_small_footprint_runs_on() {
        let probe = FakeProbe::new(OVER, Ok(1), 80);
        let reads = Arc::clone(&probe);
        let guard = Guard::with_probe(LIMITS, probe);
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let mut command = new_command("sh");
        command.args(["-c", "sleep 1; exit 7"]);
        let Ok(exit) = run_to_files(
            command,
            &dir.path().join("out"),
            &dir.path().join("err"),
            Duration::from_secs(20),
            &guard,
            Scope::Tests,
        ) else {
            panic!("sh must start");
        };
        assert_eq!(exit, Exit::Code(Some(7)));
        assert!(reads.footprint_reads.load(Ordering::SeqCst) >= 1);
        assert_eq!(guard.kills(), 0);
    }

    #[test]
    fn free_memory_blocks_a_start_only_under_the_floor() {
        assert!(Guard::disabled().memory_is_free());
        let low = Guard::with_probe(LIMITS, FakeProbe::new(1, Ok(1), 24));
        assert!(!low.memory_is_free());
        let free = Guard::with_probe(LIMITS, FakeProbe::new(1, Ok(1), 25));
        assert!(free.memory_is_free());
    }

    #[test]
    fn kill_reasons_name_the_cause() {
        let low = KillReason::LowMemory {
            free_percent: 10,
            floor_percent: 25,
            footprint_kib: 2048,
        };
        assert_eq!(
            low.to_string(),
            "memory guard: free memory is 10%, under the floor of 25%, and this process was the largest at 2 MiB"
        );
        assert_eq!(
            KillReason::Blind.to_string(),
            "memory guard: it cannot read the memory data"
        );
    }
}
