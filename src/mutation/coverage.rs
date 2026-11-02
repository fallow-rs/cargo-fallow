//! Builds the map from each test to the lines that it executes.
//!
//! One build has `-C instrument-coverage`. Each test then runs alone through
//! nextest, with its own profile folder. The run converts the profiles of a test
//! to line ranges at once and deletes them, because one profile can be 20 MB.
//!
//! A map must never say that a test reaches nothing when the data is only
//! missing. So a test with no usable data stays out of the map, and each mutant
//! runs a test that the map does not know.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::mutation::guard::Scope;
use crate::mutation::process::{Exit, new_command, run_to_files};
use crate::mutation::testrun::{
    BuildMeta, NEXTEST_TESTS_FAILED, RunSpec, TestName, TestTree, run_nextest,
};

/// The version of the map layout. A change of the layout changes this value.
pub const MAP_FORMAT: u32 = 2;
/// The compiler flag of the coverage build.
pub const INSTRUMENT: &str = "-C instrument-coverage";
/// The time limit of one call of an LLVM tool.
const TOOL_LIMIT: Duration = Duration::from_mins(10);
/// An instrumented test is slower than a plain one. This limit only stops a test
/// that hangs, so it is far above the baseline.
const COVERAGE_FACTOR: u32 = 20;
/// The shortest time limit of one coverage run.
const MIN_COVERAGE_LIMIT: Duration = Duration::from_mins(1);

/// A closed range of lines, from 1.
pub type LineRange = (u32, u32);

/// The LLVM tools of the active toolchain.
#[derive(Debug, Clone)]
pub struct Tools {
    profdata: PathBuf,
    cov: PathBuf,
}

/// The output of `rustc -vV` in `dir`, so that the toolchain file of the project
/// applies. The value is empty when the call fails.
pub fn rustc_version(dir: &Path) -> String {
    rustc(dir, &["-vV"]).unwrap_or_default()
}

fn rustc(dir: &Path, args: &[&str]) -> Option<String> {
    let program = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = new_command(program)
        .current_dir(dir)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Finds the LLVM tools in the sysroot of the active toolchain. The error names
/// the first tool that is missing.
pub fn find_tools(dir: &Path) -> Result<Tools, String> {
    let sysroot = rustc(dir, &["--print", "sysroot"])
        .ok_or_else(|| "`rustc --print sysroot` failed".to_owned())?;
    let version = rustc_version(dir);
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or_else(|| "`rustc -vV` does not name the host".to_owned())?;
    let bin = Path::new(sysroot.trim())
        .join("lib/rustlib")
        .join(host)
        .join("bin");
    let tool = |name: &str| {
        let path = bin.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        if path.is_file() {
            Ok(path)
        } else {
            Err(format!("cannot find `{name}` in `{}`", bin.display()))
        }
    };
    Ok(Tools {
        profdata: tool("llvm-profdata")?,
        cov: tool("llvm-cov")?,
    })
}

/// The lines of one file in the map.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileCoverage {
    /// The lines with coverage data in any test.
    known: Vec<LineRange>,
    /// The lines that each test executes, by the index of the test.
    hits: BTreeMap<u32, Vec<LineRange>>,
}

/// The tests that execute some lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineTests {
    /// No test has data on these lines, so the map says nothing about them.
    NoData,
    /// The map has data, and these tests execute a line.
    Tests(BTreeSet<u32>),
}

/// The map from each test to the lines that it executes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageMap {
    /// The layout version.
    pub format: u32,
    /// The tests with data, as the full nextest names.
    pub tests: Vec<String>,
    /// The tests with no usable data, as the full nextest names.
    pub unmapped: Vec<String>,
    /// The tests that fail when they run alone with no mutant, and pass in the
    /// baseline. Such a test needs another test, so its result says nothing about a
    /// mutant in a run of selected tests.
    pub fail_alone: Vec<String>,
    /// The data of each file, by the path from the workspace root.
    pub files: BTreeMap<String, FileCoverage>,
}

impl CoverageMap {
    /// The tests that execute a line in the closed range, from the first to the
    /// last line of the mutated text.
    pub fn tests_on(&self, file: &str, lines: LineRange) -> LineTests {
        let Some(found) = self.files.get(file) else {
            return LineTests::NoData;
        };
        if !overlaps(&found.known, lines) {
            return LineTests::NoData;
        }
        LineTests::Tests(
            found
                .hits
                .iter()
                .filter(|(_, ranges)| overlaps(ranges, lines))
                .map(|(test, _)| *test)
                .collect(),
        )
    }
}

/// True when a sorted list of closed ranges has a line in `lines`.
fn overlaps(ranges: &[LineRange], lines: LineRange) -> bool {
    let at = ranges.partition_point(|range| range.1 < lines.0);
    ranges.get(at).is_some_and(|range| range.0 <= lines.1)
}

/// Joins sorted lines into closed ranges.
fn to_ranges(lines: &BTreeSet<u32>) -> Vec<LineRange> {
    let mut ranges: Vec<LineRange> = Vec::new();
    for &line in lines {
        match ranges.last_mut() {
            Some(last) if last.1 + 1 == line => last.1 = line,
            _ => ranges.push((line, line)),
        }
    }
    ranges
}

/// The lines of one test, from one LCOV text.
#[derive(Debug, Default, PartialEq, Eq)]
struct TestLines {
    /// True when the test executes any line in the export.
    executed_any: bool,
    /// The lines with data in the files of the map.
    known: BTreeMap<String, BTreeSet<u32>>,
    /// The executed lines in the files of the map.
    hit: BTreeMap<String, BTreeSet<u32>>,
}

/// Reads an LCOV text. A path outside `root` or outside `files` only counts for
/// `executed_any`, and a line with another shape does not count.
fn parse_lcov(text: &str, root: &Path, files: &BTreeSet<String>) -> TestLines {
    let mut out = TestLines::default();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("SF:") {
            current = relative_key(root, path).filter(|key| files.contains(key));
        } else if let Some(data) = line.strip_prefix("DA:") {
            let mut fields = data.split(',');
            let Some(number) = fields.next().and_then(|field| field.parse::<u32>().ok()) else {
                continue;
            };
            let executed = fields.next().is_some_and(|count| count != "0");
            out.executed_any |= executed;
            if let Some(key) = &current {
                out.known.entry(key.clone()).or_default().insert(number);
                if executed {
                    out.hit.entry(key.clone()).or_default().insert(number);
                }
            }
        }
    }
    out
}

/// The map key of a path: relative to `root`, with `/` separators.
fn relative_key(root: &Path, path: &str) -> Option<String> {
    let relative = Path::new(path).strip_prefix(root).ok()?;
    Some(relative.to_string_lossy().replace('\\', "/"))
}

/// The inputs of one map.
pub struct MapInput<'a> {
    /// The instrumented tree.
    pub tree: TestTree<'a>,
    /// The instrumented build.
    pub meta: &'a BuildMeta,
    /// The LLVM tools.
    pub tools: &'a Tools,
    /// The tests that pass in the baseline, with their baseline time.
    pub tests: &'a [(String, u64)],
    /// The files with mutants, by the path from the workspace root.
    pub files: &'a BTreeSet<String>,
    /// The source paths that the export of LLVM reads: files or folders.
    pub sources: &'a [PathBuf],
    /// The workspace root of the instrumented tree.
    pub root: &'a Path,
    /// The test processes that run at the same time.
    pub jobs: usize,
}

/// The result of one map.
#[derive(Debug)]
pub struct Built {
    /// The map.
    pub map: CoverageMap,
    /// False when the memory guard killed a run. Then the map stays out of the
    /// cache, because the next run can have better data.
    pub complete: bool,
}

/// What one test gave.
enum OneTest {
    Mapped(TestLines),
    Unmapped,
    Killed,
    /// The test ran and failed. Another test at the same time can cause that.
    Failed,
}

/// Runs each test alone and builds the map.
pub fn build_map(input: &MapInput<'_>) -> io::Result<Built> {
    let objects = input.meta.read_objects()?;
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, io::Result<OneTest>)>> = Mutex::new(Vec::new());
    thread::scope(|scope| {
        for _ in 0..input.jobs {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    let Some((name, baseline_ms)) = input.tests.get(index) else {
                        return;
                    };
                    let one = run_one(input, &objects, index, name, *baseline_ms);
                    results
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push((index, one));
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_else(PoisonError::into_inner);
    results.sort_by_key(|(index, _)| *index);
    let mut collector = Collector::default();
    for (index, one) in results {
        let (name, baseline_ms) = &input.tests[index];
        let mut one = one?;
        // No other test runs now, so a second failure is the result of the test.
        if matches!(one, OneTest::Failed) && input.jobs > 1 {
            one = run_one(input, &objects, index, name, *baseline_ms)?;
        }
        collector.add(name, one);
    }
    Ok(collector.finish())
}

/// Adds the results of each test, in the order of the tests.
#[derive(Default)]
struct Collector {
    map: CoverageMap,
    known: BTreeMap<String, BTreeSet<u32>>,
    killed: bool,
}

impl Collector {
    fn add(&mut self, name: &str, one: OneTest) {
        match one {
            OneTest::Killed => {
                self.killed = true;
                self.map.unmapped.push(name.to_owned());
            }
            OneTest::Unmapped => self.map.unmapped.push(name.to_owned()),
            OneTest::Failed => self.map.fail_alone.push(name.to_owned()),
            // A test always executes its own lines. No executed line means that
            // the profile of the test process is missing, and that only a listing
            // process wrote a profile.
            OneTest::Mapped(lines) if !lines.executed_any => {
                self.map.unmapped.push(name.to_owned());
            }
            OneTest::Mapped(lines) => {
                let index = u32::try_from(self.map.tests.len()).unwrap_or(u32::MAX);
                self.map.tests.push(name.to_owned());
                for (file, known) in lines.known {
                    self.known.entry(file).or_default().extend(known);
                }
                for (file, hit) in lines.hit {
                    let entry = self.map.files.entry(file).or_default();
                    entry.hits.insert(index, to_ranges(&hit));
                }
            }
        }
    }

    fn finish(mut self) -> Built {
        for (file, known) in &self.known {
            let entry = self.map.files.entry(file.clone()).or_default();
            entry.known = to_ranges(known);
        }
        self.map.format = MAP_FORMAT;
        Built {
            map: self.map,
            complete: !self.killed,
        }
    }
}

fn run_one(
    input: &MapInput<'_>,
    objects: &Objects,
    index: usize,
    name: &str,
    baseline_ms: u64,
) -> io::Result<OneTest> {
    let Some(parsed) = TestName::parse(name) else {
        return Ok(OneTest::Unmapped);
    };
    let folder = input.tree.logs.join(format!("coverage-{index:06}"));
    fs::create_dir_all(&folder)?;
    let limit = (Duration::from_millis(baseline_ms) * COVERAGE_FACTOR).max(MIN_COVERAGE_LIMIT);
    let spec = RunSpec {
        label: format!("coverage-{index:06}"),
        limit,
        only: Some(parsed.filter()),
        profile_file: Some(folder.join("%p-%m.profraw")),
        remove_logs: true,
        ..RunSpec::new(1)
    };
    let (exit, suite) = run_nextest(input.tree, input.meta, &spec)?;
    let one = match exit {
        Exit::Killed(_) => OneTest::Killed,
        Exit::Code(Some(0)) if suite.pass >= 1 => convert(input, objects, &parsed, &folder)?,
        Exit::Code(Some(NEXTEST_TESTS_FAILED)) if suite.fail >= 1 => OneTest::Failed,
        _ => OneTest::Unmapped,
    };
    // A failed removal only leaves files in a work folder.
    let _ = fs::remove_dir_all(&folder);
    Ok(one)
}

/// Joins the profiles of one test, deletes them, and reads the lines of the test.
fn convert(
    input: &MapInput<'_>,
    objects: &Objects,
    test: &TestName<'_>,
    folder: &Path,
) -> io::Result<OneTest> {
    let profiles: Vec<PathBuf> = fs::read_dir(folder)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension() == Some(OsStr::new("profraw")))
        .collect();
    let suite = format!("{}::{}", test.package, test.binary);
    let Some((first, others)) = objects
        .tests
        .get(&suite)
        .and_then(|paths| paths.split_first())
    else {
        return Ok(OneTest::Unmapped);
    };
    if profiles.is_empty() {
        return Ok(OneTest::Unmapped);
    }
    let joined = folder.join("joined.profdata");
    let mut command = new_command(&input.tools.profdata);
    command
        .args(["merge", "-sparse", "-o"])
        .arg(&joined)
        .args(&profiles);
    if !run_tool(command, input, folder, "profdata")? {
        return Ok(OneTest::Unmapped);
    }
    for profile in &profiles {
        // A failed removal only leaves a file until the folder goes.
        let _ = fs::remove_file(profile);
    }
    let mut command = new_command(&input.tools.cov);
    command
        .args([
            "export",
            "--format=lcov",
            "--skip-branches",
            "--skip-functions",
        ])
        .arg(format!("--instr-profile={}", joined.display()))
        .arg(first);
    for object in others.iter().chain(&objects.non_test) {
        command.arg("--object").arg(object);
    }
    command.args(input.sources);
    if !run_tool(command, input, folder, "export")? {
        return Ok(OneTest::Unmapped);
    }
    let text = fs::read_to_string(folder.join("export.out"))?;
    Ok(OneTest::Mapped(parse_lcov(&text, input.root, input.files)))
}

fn run_tool(
    command: Command,
    input: &MapInput<'_>,
    folder: &Path,
    label: &str,
) -> io::Result<bool> {
    let out = folder.join(format!("{label}.out"));
    let err = folder.join(format!("{label}.err"));
    let guard = input.tree.guard;
    let exit = run_to_files(command, &out, &err, TOOL_LIMIT, guard, Scope::Build)?;
    Ok(exit == Exit::Code(Some(0)))
}

/// The binaries that can hold the coverage data of a test.
#[derive(Debug, Default, Clone)]
pub struct Objects {
    /// The test binaries of each suite name, `package::binary`.
    pub tests: BTreeMap<String, Vec<PathBuf>>,
    /// The other binaries of the package. A test can start these binaries.
    pub non_test: Vec<PathBuf>,
}

/// Builds a map from LCOV texts, for the tests of other modules.
#[cfg(test)]
pub fn parse_test_map(root: &Path, file: &str, tests: &[(&str, &str)]) -> CoverageMap {
    let files = [file.to_owned()].into();
    let mut collector = Collector::default();
    for (name, text) in tests {
        collector.add(name, OneTest::Mapped(parse_lcov(text, root, &files)));
    }
    collector.finish().map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    const LCOV: &str = "SF:/w/src/lib.rs\nFNF:1\nDA:1,3\nDA:2,0\nDA:4,18446744073709551615\nDA:x,1\nend_of_record\nSF:/w/tests/t.rs\nDA:9,1\nend_of_record\nSF:/other/lib.rs\nDA:7,1\nend_of_record\n";

    #[test]
    fn lcov_keeps_only_the_files_of_the_map() {
        let parsed = parse_lcov(LCOV, Path::new("/w"), &files(&["src/lib.rs"]));
        assert!(parsed.executed_any);
        assert_eq!(parsed.known["src/lib.rs"], [1, 2, 4].into());
        assert_eq!(parsed.hit["src/lib.rs"], [1, 4].into());
        assert_eq!(parsed.known.len(), 1);
    }

    #[test]
    fn lines_in_another_file_count_as_executed() {
        let parsed = parse_lcov("SF:/w/t.rs\nDA:1,1\n", Path::new("/w"), &files(&["a.rs"]));
        assert!(parsed.executed_any);
        assert!(parsed.known.is_empty());
    }

    #[test]
    fn a_profile_with_no_executed_line_executes_nothing() {
        let parsed = parse_lcov("SF:/w/a.rs\nDA:1,0\n", Path::new("/w"), &files(&["a.rs"]));
        assert!(!parsed.executed_any);
    }

    #[test]
    fn lines_join_into_ranges() {
        assert_eq!(
            to_ranges(&[1, 2, 3, 7, 9, 10].into()),
            [(1, 3), (7, 7), (9, 10)]
        );
        assert!(to_ranges(&BTreeSet::new()).is_empty());
    }

    #[test]
    fn overlap_checks_each_edge() {
        let ranges = [(2, 4), (8, 8)];
        for (lines, wanted) in [
            ((1, 1), false),
            ((1, 2), true),
            ((4, 4), true),
            ((5, 7), false),
            ((5, 8), true),
            ((9, 20), false),
            ((1, 20), true),
        ] {
            assert_eq!(overlaps(&ranges, lines), wanted, "{lines:?}");
        }
    }

    fn sample() -> CoverageMap {
        let mut collector = Collector::default();
        let lines =
            |text: &str| OneTest::Mapped(parse_lcov(text, Path::new("/w"), &files(&["a.rs"])));
        collector.add("b$one", lines("SF:/w/a.rs\nDA:1,1\nDA:2,0\nDA:3,0\n"));
        collector.add("b$two", lines("SF:/w/a.rs\nDA:1,0\nDA:2,1\nDA:3,0\n"));
        collector.add("b$none", lines("SF:/w/a.rs\nDA:1,0\n"));
        collector.add("b$gone", OneTest::Unmapped);
        collector.finish().map
    }

    #[test]
    fn a_line_gives_the_tests_that_execute_it() {
        let map = sample();
        assert_eq!(map.tests, ["b$one", "b$two"]);
        assert_eq!(map.tests_on("a.rs", (1, 1)), LineTests::Tests([0].into()));
        assert_eq!(map.tests_on("a.rs", (2, 2)), LineTests::Tests([1].into()));
        assert_eq!(
            map.tests_on("a.rs", (1, 2)),
            LineTests::Tests([0, 1].into())
        );
        assert_eq!(
            map.tests_on("a.rs", (3, 3)),
            LineTests::Tests(BTreeSet::new())
        );
    }

    #[test]
    fn a_line_with_no_data_says_nothing() {
        let map = sample();
        assert_eq!(map.tests_on("a.rs", (4, 9)), LineTests::NoData);
        assert_eq!(map.tests_on("other.rs", (1, 1)), LineTests::NoData);
    }

    #[test]
    fn a_test_with_no_data_is_not_in_the_map() {
        let map = sample();
        assert_eq!(map.unmapped, ["b$none", "b$gone"]);
    }

    #[test]
    fn a_guard_kill_keeps_the_map_out_of_the_cache() {
        let mut collector = Collector::default();
        collector.add("b$one", OneTest::Killed);
        let built = collector.finish();
        assert!(!built.complete);
        assert_eq!(built.map.unmapped, ["b$one"]);
    }

    #[test]
    fn a_map_survives_json() {
        let map = sample();
        let text = serde_json::to_string(&map).unwrap_or_default();
        let back: Result<CoverageMap, _> = serde_json::from_str(&text);
        assert_eq!(back.ok(), Some(map));
    }
}
