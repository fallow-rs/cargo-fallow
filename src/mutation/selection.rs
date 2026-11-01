//! Selects the tests of one mutant from the coverage map.
//!
//! The rule is on the safe side: a test runs when the map says that it executes
//! a line of the mutant, when the map does not know the test, or when the map has
//! no data on the lines of the mutant. So missing data costs time, and it does not
//! change a verdict.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

use crate::mutation::coverage::{CoverageMap, LineRange, LineTests};
use crate::mutation::testrun::{TestName, escape_filter};

/// A longer filter does not fit in one argument on Linux, which allows 128 KiB.
const MAX_FILTER_BYTES: usize = 96 * 1024;
/// nextest accepts priorities from -100 to 100.
const PRIORITY_LEVELS: usize = 201;
/// The highest priority of nextest.
const TOP_PRIORITY: i32 = 100;

/// The tests of one mutant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
    /// The filter of the run, or `None` when the run needs no filter of tests.
    pub only: Option<String>,
    /// True when no test of this tier executes the mutant.
    pub empty: bool,
    /// The baseline time of the selected tests, in milliseconds.
    pub baseline_ms: u64,
    /// True when the map has no data on the lines of the mutant.
    pub no_line_data: bool,
}

/// The map with a lookup from a test name to its index.
pub struct Selector {
    map: CoverageMap,
    index: BTreeMap<String, u32>,
}

impl Selector {
    /// Wraps a map.
    pub fn new(map: CoverageMap) -> Self {
        let index = map
            .tests
            .iter()
            .enumerate()
            .map(|(at, name)| (name.clone(), u32::try_from(at).unwrap_or(u32::MAX)))
            .collect();
        Self { map, index }
    }

    /// The map.
    pub const fn map(&self) -> &CoverageMap {
        &self.map
    }

    /// Selects from `tests`, the tests of a tier with their baseline time.
    pub fn pick(&self, tests: &[(String, u64)], file: &str, lines: LineRange) -> Picked {
        let on_lines = self.map.tests_on(file, lines);
        let hits = match &on_lines {
            LineTests::NoData => None,
            LineTests::Tests(found) => Some(found),
        };
        let selected: Vec<&(String, u64)> = tests
            .iter()
            .filter(|(name, _)| match (hits, self.index.get(name)) {
                (Some(found), Some(index)) => found.contains(index),
                _ => true,
            })
            .collect();
        let baseline_ms = selected.iter().map(|(_, ms)| ms).sum();
        // With each test selected, the plain run is the same and has no long filter.
        let only = if selected.len() == tests.len() {
            None
        } else {
            only_filter(selected.iter().map(|(name, _)| name.as_str()))
        };
        Picked {
            only,
            empty: selected.is_empty(),
            baseline_ms,
            no_line_data: hits.is_none(),
        }
    }
}

/// The filter that selects `names`, grouped by binary. It is `None` when the text
/// is too long for one argument. Then the run has each test, which is safe.
fn only_filter<'a>(names: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut by_binary: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    for name in names {
        // A name with another shape cannot get a filter, so the run has each test.
        let parsed = TestName::parse(name)?;
        by_binary
            .entry(parsed.binary_filter())
            .or_default()
            .insert(parsed.test);
    }
    let mut out = String::new();
    for (binary, tests) in by_binary {
        if !out.is_empty() {
            out.push_str(" | ");
        }
        // A write to a `String` cannot fail.
        let _ = write!(out, "({binary} & (");
        let mut first = true;
        for test in tests {
            if !first {
                out.push_str(" | ");
            }
            first = false;
            let _ = write!(out, "test(={})", escape_filter(test));
        }
        out.push_str("))");
        if out.len() > MAX_FILTER_BYTES {
            return None;
        }
    }
    Some(out)
}

/// Writes a nextest tool config that runs a fast test first. `tests` has the
/// fastest test first. A suite with more tests than priority levels shares a level
/// between neighbours, so the order is only exact up to one level.
pub fn write_order(path: &Path, tests: &[(String, u64)]) -> io::Result<()> {
    fs::write(path, order_config(tests))
}

fn order_config(tests: &[(String, u64)]) -> String {
    let per_level = tests.len().div_ceil(PRIORITY_LEVELS).max(1);
    let mut out = String::new();
    for (level, chunk) in tests.chunks(per_level).enumerate() {
        let terms: Vec<String> = chunk
            .iter()
            .filter_map(|(name, _)| TestName::parse(name))
            .map(|name| format!("({})", name.filter()))
            .collect();
        let priority = TOP_PRIORITY - i32::try_from(level).unwrap_or(0);
        if terms.is_empty() {
            continue;
        }
        // A JSON string is also a valid TOML string.
        let filter = serde_json::Value::String(terms.join(" | ")).to_string();
        let _ = writeln!(
            out,
            "[[profile.default.overrides]]\nfilter = {filter}\npriority = {priority}\n"
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::mutation::coverage::parse_test_map;

    fn selector() -> Selector {
        Selector::new(parse_test_map(
            Path::new("/w"),
            "a.rs",
            &[
                ("p::b$one", "SF:/w/a.rs\nDA:1,1\nDA:2,0\nDA:3,0\n"),
                ("p::b$two", "SF:/w/a.rs\nDA:1,0\nDA:2,1\nDA:3,0\n"),
            ],
        ))
    }

    fn tests(names: &[(&str, u64)]) -> Vec<(String, u64)> {
        names
            .iter()
            .map(|(name, ms)| ((*name).to_owned(), *ms))
            .collect()
    }

    #[test]
    fn only_the_tests_on_the_line_run() {
        let all = tests(&[("p::b$one", 5), ("p::b$two", 7)]);
        let picked = selector().pick(&all, "a.rs", (2, 2));
        assert_eq!(
            picked.only.as_deref(),
            Some("(package(=p) & binary(=b) & (test(=two)))")
        );
        assert_eq!(picked.baseline_ms, 7);
        assert!(!picked.empty);
        assert!(!picked.no_line_data);
    }

    #[test]
    fn a_line_that_no_test_executes_selects_nothing() {
        let all = tests(&[("p::b$one", 5), ("p::b$two", 7)]);
        let picked = selector().pick(&all, "a.rs", (3, 3));
        assert!(picked.empty);
        assert_eq!(picked.baseline_ms, 0);
    }

    #[test]
    fn a_test_outside_the_map_always_runs() {
        let all = tests(&[("p::b$one", 5), ("p::b$new", 3)]);
        let picked = selector().pick(&all, "a.rs", (3, 3));
        assert!(!picked.empty);
        assert_eq!(
            picked.only.as_deref(),
            Some("(package(=p) & binary(=b) & (test(=new)))")
        );
    }

    #[test]
    fn a_line_with_no_data_runs_each_test() {
        let all = tests(&[("p::b$one", 5), ("p::b$two", 7)]);
        let picked = selector().pick(&all, "a.rs", (40, 41));
        assert_eq!(picked.only, None);
        assert_eq!(picked.baseline_ms, 12);
        assert!(picked.no_line_data);
    }

    #[test]
    fn an_excluded_test_stays_out() {
        let all = tests(&[("p::b$two", 7)]);
        assert!(selector().pick(&all, "a.rs", (1, 1)).empty);
    }

    #[test]
    fn the_filter_groups_by_binary_and_escapes() {
        let names = ["b::c$t::a", "a::d$x", "b::c$t::b"];
        assert_eq!(
            only_filter(names.into_iter()).as_deref(),
            Some(
                "(package(=a) & binary(=d) & (test(=x))) | (package(=b) & binary(=c) & (test(=t::a) | test(=t::b)))"
            )
        );
    }

    #[test]
    fn a_name_that_nextest_can_refuse_runs_each_test() {
        let names = ["p::b$plain", "p::b$a (b)"];
        assert_eq!(only_filter(names.into_iter()), None);
    }

    #[test]
    fn a_filter_over_the_argument_size_runs_each_test() {
        let names: Vec<String> = (0..20_000)
            .map(|n| format!("p::b$test_number_{n}"))
            .collect();
        assert_eq!(only_filter(names.iter().map(String::as_str)), None);
    }

    #[test]
    fn the_order_gives_the_first_test_the_top_priority() {
        let text = order_config(&tests(&[("p::b$fast", 1), ("p::b$slow", 9)]));
        let fast = text.find("test(=fast)").unwrap_or(usize::MAX);
        let top = text.find("priority = 100").unwrap_or(0);
        let slow = text.find("test(=slow)").unwrap_or(0);
        let next = text.find("priority = 99").unwrap_or(0);
        assert!(fast < top && top < slow && slow < next, "{text}");
    }

    #[test]
    fn a_large_suite_stays_in_the_priority_range() {
        let many: Vec<(String, u64)> = (0..1000).map(|n| (format!("p::b$t{n}"), n)).collect();
        let text = order_config(&many);
        assert_eq!(text.matches("priority = ").count(), 200);
        assert!(!text.contains("priority = -101"), "{text}");
    }

    #[test]
    fn the_order_leaves_out_a_name_that_nextest_can_refuse() {
        let text = order_config(&tests(&[("p::b$a\"b", 1), ("p::b$plain", 2)]));
        assert!(!text.contains("a\\\"b"), "{text}");
        assert!(text.contains("test(=plain)"), "{text}");
    }
}
