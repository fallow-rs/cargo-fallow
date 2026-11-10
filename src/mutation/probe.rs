//! Reads the process table and the memory data of the operating system.
//!
//! macOS has no stable-Rust API for the footprint without unsafe code, so the
//! probe reads the `footprint` tool. The footprint counts compressed pages, and
//! RSS does not. RSS let a mutant fill the machine on 2026-10-28, so a limit
//! never uses it. `footprint` reads only the given processes and costs about
//! 20 ms of CPU. `top` reads each process and costs about one second of CPU
//! (measured), so it is only the fallback. The guard reads RSS from `ps` first.
//!
//! Linux reads `/proc`: the footprint is RSS plus swap, and the free memory comes
//! from the cgroup when it has a limit, else from `/proc/meminfo`.

use std::collections::BTreeMap;
#[cfg(any(test, target_os = "linux"))]
use std::fs;
use std::io;
#[cfg(any(test, target_os = "linux"))]
use std::path::Path;
#[cfg(target_os = "macos")]
use std::thread;
#[cfg(target_os = "macos")]
use std::time::Duration;

#[cfg(target_os = "macos")]
use crate::mutation::process::new_command;

/// One row of the process table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// The process id.
    pub pid: u32,
    /// The id of the parent.
    pub ppid: u32,
    /// The resident size in KiB. This value only selects candidates.
    pub rss_kib: u64,
}

/// The memory data of one operating system.
pub trait Probe: Send + Sync {
    /// Reads the process table.
    fn table(&self) -> io::Result<Vec<Row>>;
    /// Reads the footprint in KiB of each process that still runs.
    fn footprints(&self, pids: &[u32]) -> io::Result<BTreeMap<u32, u64>>;
    /// The free memory as a percentage, when the system reports it.
    fn free_percent(&self) -> Option<u32>;
}

/// The probe of this operating system, or `None` when it has no probe.
///
/// Each probe is compiled only for its own system, because it reads tools or
/// files of that system. The tests check the parsers on each host.
#[cfg_attr(
    any(target_os = "macos", target_os = "linux"),
    expect(
        clippy::unnecessary_wraps,
        reason = "a system with no probe gets `None`"
    )
)]
pub fn system_probe() -> Option<Box<dyn Probe>> {
    #[cfg(target_os = "macos")]
    {
        Some(Box::new(MacProbe))
    }
    #[cfg(target_os = "linux")]
    {
        Some(Box::new(LinuxProbe {
            proc: Path::new("/proc"),
            cgroup: Path::new("/sys/fs/cgroup"),
        }))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
struct MacProbe;

#[cfg(target_os = "macos")]
impl Probe for MacProbe {
    fn table(&self) -> io::Result<Vec<Row>> {
        let output = new_command("ps")
            .args(["-axo", "pid=,ppid=,rss="])
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other("ps failed"));
        }
        Ok(parse_ps(&String::from_utf8_lossy(&output.stdout)))
    }

    fn footprints(&self, pids: &[u32]) -> io::Result<BTreeMap<u32, u64>> {
        footprint_tool(pids).map_or_else(|| top_footprints(pids), Ok)
    }

    fn free_percent(&self) -> Option<u32> {
        let output = new_command("sysctl")
            .args(["-n", "kern.memorystatus_level"])
            .output()
            .ok()?;
        String::from_utf8_lossy(&output.stdout).trim().parse().ok()
    }
}

/// Reads the footprints through `footprint`. It reads only the given processes,
/// so it costs about 20 ms of CPU, and `top` costs about one second. Returns
/// `None` when the tool is missing or fails in another way, so that `top` reads
/// the values. A process that ended has no row.
#[cfg(target_os = "macos")]
fn footprint_tool(pids: &[u32]) -> Option<BTreeMap<u32, u64>> {
    // `footprint` cannot read a process during an exec. A second read a moment
    // later is still much cheaper than `top`.
    footprint_read(pids).or_else(|| {
        thread::sleep(FOOTPRINT_RETRY);
        footprint_read(pids)
    })
}

/// The wait before the second read of `footprint`.
#[cfg(target_os = "macos")]
const FOOTPRINT_RETRY: Duration = Duration::from_millis(50);

#[cfg(target_os = "macos")]
fn footprint_read(pids: &[u32]) -> Option<BTreeMap<u32, u64>> {
    let mut command = new_command(FOOTPRINT);
    command.args(["-f", "bytes"]);
    for pid in pids {
        command.arg("-p").arg(pid.to_string());
    }
    let output = command.output().ok()?;
    let rows = parse_footprint(&String::from_utf8_lossy(&output.stdout));
    if output.status.success() && !rows.is_empty() {
        return Some(rows);
    }
    // The tool fails when each process ended before the read.
    let all_ended = String::from_utf8_lossy(&output.stderr).contains(NO_PROCESS);
    all_ended.then(BTreeMap::new)
}

/// The footprint tool of macOS.
#[cfg(target_os = "macos")]
const FOOTPRINT: &str = "/usr/bin/footprint";
/// The message of `footprint` when no given process exists.
#[cfg(target_os = "macos")]
const NO_PROCESS: &str = "Unable to find any processes";

/// Reads the footprints with `top`.
#[cfg(target_os = "macos")]
fn top_footprints(pids: &[u32]) -> io::Result<BTreeMap<u32, u64>> {
    let mut command = new_command("top");
    command.args(["-l", "1", "-stats", "pid,mem"]);
    for pid in pids {
        command.arg("-pid").arg(pid.to_string());
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(io::Error::other("top failed"));
    }
    Ok(parse_top(&String::from_utf8_lossy(&output.stdout)))
}

/// Reads the header line of each process in the text of `footprint -f bytes`,
/// for example `sleep [42]: 64-bit    Footprint: 1032432 B (16384 bytes per page)`.
/// The value is in KiB. A name can hold brackets, so the pid comes from the last
/// bracket pair before the colon.
#[cfg(any(test, target_os = "macos"))]
fn parse_footprint(text: &str) -> BTreeMap<u32, u64> {
    text.lines()
        .filter_map(|line| {
            let (head, rest) = line.split_once("]: ")?;
            let pid = head.rsplit_once('[')?.1.parse().ok()?;
            let value = rest.split_once("Footprint: ")?.1;
            let (bytes, unit) = value.split_once(' ')?;
            if !unit.starts_with("B ") && unit != "B" {
                return None;
            }
            let bytes: u64 = bytes.parse().ok()?;
            Some((pid, bytes / 1024))
        })
        .collect()
}

/// Reads `pid ppid rss` lines. A line with another shape does not count.
#[cfg(any(test, target_os = "macos"))]
fn parse_ps(text: &str) -> Vec<Row> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some(Row {
                pid: fields.next()?.parse().ok()?,
                ppid: fields.next()?.parse().ok()?,
                rss_kib: fields.next()?.parse().ok()?,
            })
        })
        .collect()
}

/// Reads the `pid mem` rows of `top`. The header lines do not start with a
/// number, so they do not count.
#[cfg(any(test, target_os = "macos"))]
fn parse_top(text: &str) -> BTreeMap<u32, u64> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let kib = parse_top_size(fields.next()?)?;
            Some((pid, kib))
        })
        .collect()
}

/// Reads a size of `top`, such as `1808K`, `29M+` or `12G-`, as KiB.
#[cfg(any(test, target_os = "macos"))]
fn parse_top_size(text: &str) -> Option<u64> {
    let text = text.trim_end_matches(['+', '-']);
    let digits = text.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let number: u64 = digits.parse().ok()?;
    Some(match &text[digits.len()..] {
        "B" | "" => number / 1024,
        "K" => number,
        "M" => number * 1024,
        "G" => number * 1024 * 1024,
        "T" => number * 1024 * 1024 * 1024,
        _ => return None,
    })
}

#[cfg(any(test, target_os = "linux"))]
struct LinuxProbe<'a> {
    proc: &'a Path,
    cgroup: &'a Path,
}

#[cfg(any(test, target_os = "linux"))]
impl Probe for LinuxProbe<'_> {
    fn table(&self) -> io::Result<Vec<Row>> {
        let mut rows = Vec::new();
        for entry in fs::read_dir(self.proc)? {
            let entry = entry?;
            let pid = entry.file_name().to_string_lossy().parse::<u32>();
            let (Ok(pid), Ok(status)) = (pid, fs::read_to_string(entry.path().join("status")))
            else {
                continue;
            };
            rows.extend(parse_status(&status).map(|parsed| Row {
                pid,
                ppid: parsed.ppid,
                rss_kib: parsed.rss_kib,
            }));
        }
        Ok(rows)
    }

    fn footprints(&self, pids: &[u32]) -> io::Result<BTreeMap<u32, u64>> {
        Ok(pids
            .iter()
            .filter_map(|&pid| {
                let path = self.proc.join(pid.to_string()).join("status");
                let status = parse_status(&fs::read_to_string(path).ok()?)?;
                Some((pid, status.rss_kib + status.swap_kib))
            })
            .collect())
    }

    fn free_percent(&self) -> Option<u32> {
        let meminfo = fs::read_to_string(self.proc.join("meminfo")).ok();
        let system = meminfo.as_deref().and_then(meminfo_free_percent);
        let group = fs::read_to_string(self.proc.join("self/cgroup"))
            .ok()
            .and_then(|text| cgroup_free_percent(self.cgroup, &text));
        match (system, group) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

#[cfg(any(test, target_os = "linux"))]
struct Status {
    ppid: u32,
    rss_kib: u64,
    swap_kib: u64,
}

/// Reads `/proc/<pid>/status`. A kernel thread has no memory lines.
#[cfg(any(test, target_os = "linux"))]
fn parse_status(text: &str) -> Option<Status> {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
    };
    Some(Status {
        ppid: u32::try_from(field("PPid")?).ok()?,
        rss_kib: field("VmRSS").unwrap_or(0),
        swap_kib: field("VmSwap").unwrap_or(0),
    })
}

#[cfg(any(test, target_os = "linux"))]
fn meminfo_free_percent(text: &str) -> Option<u32> {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
    };
    percent(field("MemAvailable")?, field("MemTotal")?)
}

/// Reads the limit of a cgroup v2 from the `0::/path` line of `/proc/self/cgroup`.
/// A cgroup with the limit `max` gives no value.
#[cfg(any(test, target_os = "linux"))]
fn cgroup_free_percent(root: &Path, self_cgroup: &str) -> Option<u32> {
    let path = self_cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?;
    let dir = root.join(path.trim_start_matches('/'));
    let read = |name: &str| -> Option<u64> {
        fs::read_to_string(dir.join(name)).ok()?.trim().parse().ok()
    };
    let limit = read("memory.max")?;
    let used = read("memory.current")?;
    // The kernel can drop inactive page cache, so it is free memory.
    let cache = fs::read_to_string(dir.join("memory.stat"))
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("inactive_file ")?
                    .trim()
                    .parse::<u64>()
                    .ok()
            })
        })
        .unwrap_or(0);
    percent(limit.saturating_sub(used.saturating_sub(cache)), limit)
}

#[cfg(any(test, target_os = "linux"))]
fn percent(part: u64, whole: u64) -> Option<u32> {
    (whole > 0).then(|| u32::try_from(part.saturating_mul(100) / whole).unwrap_or(100))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_lines_are_read() {
        let rows = parse_ps("  10 1 2048\nbad line\n 11   10 0\n");
        assert_eq!(
            rows,
            [
                Row {
                    pid: 10,
                    ppid: 1,
                    rss_kib: 2048
                },
                Row {
                    pid: 11,
                    ppid: 10,
                    rss_kib: 0
                }
            ]
        );
    }

    /// The text is the shape of `top -l 1 -pid 1 -stats pid,mem` on macOS 27.
    #[test]
    fn top_rows_are_read_and_the_header_does_not_count() {
        let text = "Processes: 995 total\nPID    MEM  \n10095  1792K\n1      29M+\n7 12G-\n8 2T\n9 2048B\n";
        let rows = parse_top(text);
        assert_eq!(rows.get(&10095), Some(&1792));
        assert_eq!(rows.get(&1), Some(&(29 * 1024)));
        assert_eq!(rows.get(&7), Some(&(12 * 1024 * 1024)));
        assert_eq!(rows.get(&8), Some(&(2 * 1024 * 1024 * 1024)));
        assert_eq!(rows.get(&9), Some(&2));
        assert_eq!(rows.len(), 5);
    }

    /// The text is the shape of `footprint -f bytes -p 59509 -p 1` on macOS 27.
    #[test]
    fn footprint_headers_are_read_and_other_lines_do_not_count() {
        let text = "footprint: Unable to find pid for process matching '99999'\n\
            ======\n\
            sleep [59509]: 64-bit    Footprint: 1032432 B (16384 bytes per page)\n\
            ======\n\
            \x20     Dirty  Clean\n\
            \x20  1032432 B  0 B  10  Untagged\n\
            Shared with sleep [59508]:\n\
            \x20   phys_footprint: 1032432 B\n\
            a [b] [1]: 64-bit (translated)    Footprint: 2048 B (4096 bytes per page)\n";
        let rows = parse_footprint(text);
        assert_eq!(rows.get(&59509), Some(&1008));
        assert_eq!(rows.get(&1), Some(&2));
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn an_unknown_top_unit_gives_no_value() {
        assert_eq!(parse_top_size("12Q"), None);
        assert_eq!(parse_top_size("M"), None);
    }

    /// Writes a fake `/proc` and a fake cgroup tree.
    fn fake_linux(dir: &Path, limit: &str) -> Result<(), io::Error> {
        let proc = dir.join("proc");
        fs::create_dir_all(proc.join("42"))?;
        fs::create_dir_all(proc.join("7"))?;
        fs::create_dir_all(proc.join("self"))?;
        fs::create_dir_all(proc.join("not-a-pid"))?;
        fs::write(
            proc.join("42/status"),
            "Name:\tt\nPPid:\t7\nVmRSS:\t  1000 kB\nVmSwap:\t  24 kB\n",
        )?;
        fs::write(proc.join("7/status"), "Name:\tkthread\nPPid:\t0\n")?;
        fs::write(proc.join("self/cgroup"), "0::/user/a\n")?;
        fs::write(
            proc.join("meminfo"),
            "MemTotal: 1000 kB\nMemAvailable: 500 kB\n",
        )?;
        let group = dir.join("cg/user/a");
        fs::create_dir_all(&group)?;
        fs::write(group.join("memory.max"), limit)?;
        fs::write(group.join("memory.current"), "800\n")?;
        fs::write(group.join("memory.stat"), "anon 600\ninactive_file 100\n")?;
        Ok(())
    }

    fn linux_probe(dir: &tempfile::TempDir, limit: &str, check: impl Fn(&LinuxProbe<'_>)) {
        let Ok(()) = fake_linux(dir.path(), limit) else {
            panic!("the fake tree must be written");
        };
        let (proc, cgroup) = (dir.path().join("proc"), dir.path().join("cg"));
        check(&LinuxProbe {
            proc: &proc,
            cgroup: &cgroup,
        });
    }

    fn temp() -> tempfile::TempDir {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        dir
    }

    #[test]
    fn linux_footprint_is_rss_plus_swap() {
        linux_probe(&temp(), "max\n", |probe| {
            let Ok(map) = probe.footprints(&[42, 7, 99]) else {
                panic!("the fake tree is readable");
            };
            assert_eq!(map.get(&42), Some(&1024));
            assert_eq!(map.get(&7), Some(&0));
            assert_eq!(map.get(&99), None);
        });
    }

    #[test]
    fn linux_table_reads_each_process() {
        linux_probe(&temp(), "max\n", |probe| {
            let Ok(mut rows) = probe.table() else {
                panic!("the fake tree is readable");
            };
            rows.sort_by_key(|row| row.pid);
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[1].ppid, 7);
            assert_eq!(rows[1].rss_kib, 1000);
        });
    }

    #[test]
    fn linux_free_memory_is_the_lower_of_the_cgroup_and_the_system() {
        linux_probe(&temp(), "1000\n", |probe| {
            assert_eq!(probe.free_percent(), Some(30));
        });
        linux_probe(&temp(), "max\n", |probe| {
            assert_eq!(probe.free_percent(), Some(50));
        });
    }

    /// A build fills the page cache, and the kernel can drop these pages. They
    /// must not look like memory pressure.
    #[test]
    fn page_cache_does_not_count_as_used_memory_of_a_cgroup() {
        let dir = temp();
        let group = dir.path().join("a");
        let Ok(()) = fs::create_dir_all(&group)
            .and_then(|()| fs::write(group.join("memory.max"), "1000\n"))
            .and_then(|()| fs::write(group.join("memory.current"), "900\n"))
        else {
            panic!("the fake tree must be written");
        };
        assert_eq!(cgroup_free_percent(dir.path(), "0::/a\n"), Some(10));
        let Ok(()) = fs::write(group.join("memory.stat"), "inactive_file 500\n") else {
            panic!("the fake tree must be written");
        };
        assert_eq!(cgroup_free_percent(dir.path(), "0::/a\n"), Some(60));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_real_probe_sees_this_process() {
        let Some(probe) = system_probe() else {
            panic!("macOS has a probe");
        };
        let me = std::process::id();
        let Ok(rows) = probe.table() else {
            panic!("ps must run");
        };
        assert!(rows.iter().any(|row| row.pid == me));
        let Ok(map) = probe.footprints(&[me]) else {
            panic!("top must run");
        };
        assert!(map.get(&me).is_some_and(|&kib| kib > 0));
        assert!(probe.free_percent().is_some());
    }
}
