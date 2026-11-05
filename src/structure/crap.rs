//! The CRAP score of each function from an LCOV file: `--coverage <LCOV>`.
//!
//! `CRAP = CC^2 * (1 - cov)^3 + CC`, with the cyclomatic complexity `CC`.
//! `cov` is the share of the `DA` lines inside the span of the unit that have
//! a count above 0. The lines inside a nested unit (a closure or an inner
//! `fn`) belong to that unit, not to the parent. The first and the last line
//! of a nested unit can also hold code of the parent, so they count for both.
//!
//! A unit with no `DA` line has no score. A unit with a `cfg` attribute in its
//! body has no score either: the build can leave a counted branch out, and
//! then the data does not show it. A test unit has no score.
//! This parser reads only the `SF` and `DA` records. It is separate from the
//! coverage map of `mutants`, which keeps other data for each test.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use serde::Serialize;

use crate::error::{CliError, ErrorCode};
use crate::render;
use crate::structure::complexity::Function;
use crate::structure::source::{File, Loaded};
use crate::workspace::Workspace;

/// The rule of a function at or over the CRAP limit.
pub const RULE: &str = "complexity-crap";

/// The flags that read an LCOV file.
#[derive(Debug, Clone, Default, Args)]
pub struct CoverageArgs {
    /// An LCOV file, for example from `cargo llvm-cov --lcov`. Each function
    /// then gets a CRAP score from its cyclomatic complexity and its line
    /// coverage.
    #[arg(long = "coverage", value_name = "LCOV")]
    pub path: Option<PathBuf>,
    /// The workspace root of the machine that wrote the LCOV file. Each `SF`
    /// path in this folder maps to the same path in the workspace.
    #[arg(long, value_name = "PATH", requires = "path")]
    pub coverage_root: Option<PathBuf>,
}

/// The source of the coverage of a score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoverageSource {
    /// The `DA` lines of an LCOV file.
    Lcov,
}

/// The CRAP fields of a complexity finding in a run with `--coverage`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, schemars::JsonSchema)]
#[schemars(rename = "ComplexityCrap")]
pub struct CrapFields {
    /// The CRAP score: `CC^2 * (1 - cov)^3 + CC`, rounded to 1 decimal. It is
    /// `null` for a unit with no score: no `DA` line, or a `cfg` attribute in
    /// the body.
    pub crap: Option<f64>,
    /// The line coverage of the function in percent, from 0 to 100. It is
    /// `null` when `crap` is `null`.
    pub coverage_pct: Option<f64>,
    /// The source of the coverage.
    pub coverage_source: CoverageSource,
}

impl CrapFields {
    /// The fields of a unit with no score.
    pub const NONE: Self = Self {
        crap: None,
        coverage_pct: None,
        coverage_source: CoverageSource::Lcov,
    };
}

/// The line counts of an LCOV file, by the path from the workspace root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lcov {
    files: BTreeMap<String, BTreeMap<u32, u64>>,
}

/// The coverage data after the join with the source files.
#[derive(Debug, Clone, Default)]
pub struct Coverage {
    files: BTreeMap<String, BTreeMap<u32, u64>>,
    /// The files with stale data: the file is gone, or a `DA` line is after
    /// the end of the file. Sorted.
    pub stale_files: Vec<String>,
}

/// The score of one unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    /// The CRAP score, rounded to 1 decimal.
    pub crap: f64,
    /// The line coverage in percent, rounded to 1 decimal.
    pub coverage_pct: f64,
}

/// Reads the LCOV file of `args`, or `None` without `--coverage`.
pub fn read(args: &CoverageArgs, workspace: &Workspace) -> Result<Option<Lcov>, CliError> {
    let Some(path) = &args.path else {
        return Ok(None);
    };
    let text = fs::read_to_string(path).map_err(|err| {
        CliError::new(
            ErrorCode::InvalidArgument,
            format!("cannot read the coverage file `{}`: {err}", path.display()),
        )
        .with_help(
            "Give an LCOV file, for example from `cargo llvm-cov --lcov --output-path lcov.info`.",
        )
    })?;
    Ok(Some(parse(
        &text,
        workspace.root.as_std_path(),
        args.coverage_root.as_deref(),
    )))
}

/// Reads an LCOV text. `root` is the workspace root. With `remap`, an `SF`
/// path in `remap` maps to the same path in `root`. A path outside the root
/// does not count. The counts of two records of one file add up.
pub fn parse(text: &str, root: &Path, remap: Option<&Path>) -> Lcov {
    let canonical = root.canonicalize().ok();
    let mut lcov = Lcov::default();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(path) = line.strip_prefix("SF:") {
            current = workspace_key(path, root, canonical.as_deref(), remap);
        } else if line == "end_of_record" {
            current = None;
        } else if let Some(data) = line.strip_prefix("DA:")
            && let Some(key) = &current
        {
            let mut fields = data.split(',');
            let (Some(number), Some(count)) = (
                fields.next().and_then(|field| field.parse::<u32>().ok()),
                fields.next().and_then(|field| field.parse::<u64>().ok()),
            ) else {
                continue;
            };
            let entry = lcov
                .files
                .entry(key.clone())
                .or_default()
                .entry(number)
                .or_default();
            *entry = entry.saturating_add(count);
        }
    }
    lcov
}

/// The path of an `SF` record from the workspace root, with `/` separators.
fn workspace_key(
    path: &str,
    root: &Path,
    canonical: Option<&Path>,
    remap: Option<&Path>,
) -> Option<String> {
    if let Some(remap) = remap {
        let path = path.replace('\\', "/");
        let prefix = remap.to_string_lossy().replace('\\', "/");
        let rest = path
            .strip_prefix(prefix.trim_end_matches('/'))?
            .strip_prefix('/')?;
        return crate::modules::normalize(Path::new(rest));
    }
    let given = Path::new(path);
    if given.is_relative() {
        return crate::modules::normalize(&PathBuf::from(path.replace('\\', "/")));
    }
    let relative = given
        .strip_prefix(root)
        .ok()
        .or_else(|| canonical.and_then(|canonical| given.strip_prefix(canonical).ok()))
        .map(Path::to_path_buf)
        .or_else(|| {
            let real = given.canonicalize().ok()?;
            real.strip_prefix(canonical?).ok().map(Path::to_path_buf)
        })?;
    crate::modules::normalize(&relative)
}

/// Joins the LCOV data with the source files. Each stale file gives a
/// warning. With no `SF` path in a source file of the workspace, the data
/// cannot belong to the workspace, so the run stops.
pub fn join(lcov: &Lcov, loaded: &Loaded, root: &Path) -> Result<Coverage, CliError> {
    let lines: BTreeMap<&str, u32> = loaded
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.lines))
        .collect();
    let mut stale = BTreeMap::new();
    let mut matched = false;
    for (path, counts) in &lcov.files {
        match lines.get(path.as_str()) {
            Some(&last) => {
                matched = true;
                if counts.keys().next_back().is_some_and(|&line| line > last) {
                    stale.insert(
                        path.clone(),
                        "has a line after the end of the file, so the data is older than the file",
                    );
                }
            }
            None if !root.join(path).exists() => {
                stale.insert(path.clone(), "names a file that is gone");
            }
            None => {}
        }
    }
    if !matched {
        return Err(CliError::new(
            ErrorCode::InvalidArgument,
            "no `SF` path of the coverage file is a source file of the workspace",
        )
        .with_help("Run the coverage in this workspace, or give the workspace root of the machine that wrote the file with `--coverage-root <PATH>`."));
    }
    // The warnings come after the match check. With no match, each path is
    // wrong, so a stale warning would mislead.
    for (path, reason) in &stale {
        render::warn(&format!("the coverage data of `{path}` {reason}"));
    }
    Ok(Coverage {
        files: lcov.files.clone(),
        stale_files: stale.into_keys().collect(),
    })
}

impl Coverage {
    /// The score of each function of `file`, in the same order. A test unit,
    /// a unit with a `cfg` attribute in its body and a unit with no `DA` line
    /// get `None`.
    pub fn scores(&self, file: &File) -> Vec<Option<Score>> {
        let counts = self.files.get(&file.path);
        file.functions
            .iter()
            .enumerate()
            .map(|(index, function)| {
                if function.test || file.test_only {
                    return None;
                }
                if function.cfg_code {
                    return None;
                }
                let (total, hit) = own_lines(&file.functions, index)
                    .filter_map(|line| counts?.get(&line))
                    .fold((0_u32, 0_u32), |(total, hit), &count| {
                        (total + 1, hit + u32::from(count > 0))
                    });
                (total > 0).then(|| score(function.cyclomatic, hit, total))
            })
            .collect()
    }
}

/// True when the unit is not a test unit, so it can have a score.
pub const fn scored(function: &Function, file: &File) -> bool {
    !function.test && !file.test_only
}

/// The CRAP score of a unit with `hit` of `total` lines covered.
pub fn score(cyclomatic: u32, hit: u32, total: u32) -> Score {
    let cc = f64::from(cyclomatic);
    let coverage = f64::from(hit) / f64::from(total);
    let crap = (cc * cc).mul_add((1.0 - coverage).powi(3), cc);
    Score {
        crap: round(crap),
        coverage_pct: round(coverage * 100.0),
    }
}

fn round(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// The lines of the unit at `index`, without the inner lines of the units
/// that it holds.
fn own_lines(functions: &[Function], index: usize) -> impl Iterator<Item = u32> + '_ {
    let unit = &functions[index];
    let nested: Vec<(u32, u32)> = functions
        .iter()
        .enumerate()
        .filter(|&(other, inner)| other != index && holds(unit, inner))
        .map(|(_, inner)| (inner.start_line + 1, inner.end_line.saturating_sub(1)))
        .collect();
    (unit.start_line..=unit.end_line).filter(move |line| {
        !nested
            .iter()
            .any(|(first, last)| (*first..=*last).contains(line))
    })
}

/// True when `outer` holds `inner`. Two units with the same lines are in name
/// order: the unit whose name comes later is the inner one.
fn holds(outer: &Function, inner: &Function) -> bool {
    let inside = inner.start_line >= outer.start_line && inner.end_line <= outer.end_line;
    let same = inner.start_line == outer.start_line && inner.end_line == outer.end_line;
    inside && (!same || (inner.line, inner.column) > (outer.line, outer.column))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ra_ap_syntax::AstNode as _;

    use crate::parse::{Edition, with_parsed};
    use crate::structure::complexity::functions;
    use crate::structure::source::LineIndex;

    fn file(source: &str) -> File {
        let functions = with_parsed(source, Edition::Edition2024, |parsed, text| {
            functions(parsed.syntax(), &LineIndex::new(text))
        })
        .unwrap_or_default();
        File {
            path: "src/lib.rs".to_owned(),
            lines: crate::structure::source::count_lines(source),
            skip_metrics: false,
            test_only: false,
            functions,
            tokens: Vec::new(),
        }
    }

    fn coverage(lines: &[(u32, u64)]) -> Coverage {
        Coverage {
            files: BTreeMap::from([("src/lib.rs".to_owned(), lines.iter().copied().collect())]),
            stale_files: Vec::new(),
        }
    }

    #[test]
    fn the_formula_follows_the_definition() {
        assert_eq!(score(5, 0, 4).crap, 30.0);
        assert_eq!(score(6, 0, 8).crap, 42.0);
        assert_eq!(score(6, 8, 8).crap, 6.0);
        // 36 * 0.875^3 + 6 = 30.117
        let partial = score(6, 1, 8);
        assert_eq!((partial.crap, partial.coverage_pct), (30.1, 12.5));
    }

    #[test]
    fn a_closure_owns_its_inner_lines_and_shares_its_edges() {
        let source = "fn outer() {\n    let f = |x: u8| {\n        x + 1\n    };\n    f(1);\n}\n";
        let file = file(source);
        let names: Vec<&str> = file.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["outer", "f"]);
        let outer: Vec<u32> = own_lines(&file.functions, 0).collect();
        assert_eq!(outer, [1, 2, 4, 5, 6]);
        let inner: Vec<u32> = own_lines(&file.functions, 1).collect();
        assert_eq!(inner, [2, 3, 4]);
        // Only line 3 is covered: the closure has coverage, `outer` has none.
        let scores = coverage(&[(1, 0), (2, 0), (3, 4), (4, 0), (5, 0), (6, 0)]).scores(&file);
        assert_eq!(scores[0].map(|s| s.coverage_pct), Some(0.0));
        assert_eq!(scores[1].map(|s| s.coverage_pct), Some(33.3));
    }

    #[test]
    fn a_one_line_closure_does_not_take_the_line_of_its_parent() {
        let file = file("fn outer(v: &[u8]) -> usize { v.iter().filter(|x| **x > 0).count() }\n");
        let scores = coverage(&[(1, 2)]).scores(&file);
        assert!(
            scores
                .iter()
                .all(|s| s.map(|s| s.coverage_pct) == Some(100.0)),
            "{scores:?}"
        );
    }

    #[test]
    fn a_generic_function_and_a_unit_with_no_data() {
        let source = "fn generic<T: Into<u8>>(t: T) -> u8 {\n    t.into()\n}\nfn other() {\n}\n";
        let scores = coverage(&[(1, 1), (2, 0), (3, 1)]).scores(&file(source));
        assert_eq!(scores[0].map(|s| s.coverage_pct), Some(66.7));
        assert_eq!(scores[1], None);
    }

    #[test]
    fn a_unit_with_cfg_code_has_no_score() {
        let source = "fn f(a: u8) -> u8 {\n    #[cfg(feature = \"x\")]\n    if a > 1 { return 2; }\n    a\n}\nfn g() {\n    #[cfg(test)]\n    let _x = 1;\n}\n";
        let file = file(source);
        let flags: Vec<bool> = file.functions.iter().map(|f| f.cfg_code).collect();
        assert_eq!(flags, [true, false]);
        let scores = coverage(&[(1, 0), (4, 0), (6, 0), (8, 0)]).scores(&file);
        assert_eq!(scores[0], None);
        assert!(scores[1].is_some());
    }

    #[test]
    fn a_test_unit_has_no_score() {
        let source = "#[cfg(test)]\nmod tests {\n    fn helper() {}\n    #[test]\n    fn check() {}\n}\n#[test]\nfn alone() {}\nfn real() {}\n";
        let file = file(source);
        let tests: Vec<bool> = file.functions.iter().map(|f| f.test).collect();
        assert_eq!(tests, [true, true, true, false]);
        let scores = coverage(&[(3, 1), (5, 1), (8, 1), (9, 1)]).scores(&file);
        assert_eq!(scores.iter().filter(|s| s.is_some()).count(), 1);
    }

    #[test]
    fn the_parser_maps_each_path_to_the_workspace_and_adds_the_records() {
        let text = "TN:\nSF:/w/src/lib.rs\nFN:1,f\nDA:1,3\nDA:2,0\nDA:x,1\nend_of_record\nSF:/w/src/lib.rs\nDA:2,1\nend_of_record\nSF:/other/x.rs\nDA:1,1\nend_of_record\nSF:src/rel.rs\r\nDA:4,1\r\nend_of_record\n";
        let lcov = parse(text, Path::new("/w"), None);
        let keys: Vec<&str> = lcov.files.keys().map(String::as_str).collect();
        assert_eq!(keys, ["src/lib.rs", "src/rel.rs"]);
        assert_eq!(lcov.files["src/lib.rs"], BTreeMap::from([(1, 3), (2, 1)]));
        let remapped = parse(
            "SF:C:\\ci\\work\\src\\lib.rs\nDA:1,1\n",
            Path::new("/w"),
            Some(Path::new("C:\\ci\\work")),
        );
        assert_eq!(remapped.files.keys().collect::<Vec<_>>(), ["src/lib.rs"]);
        let outside = parse(
            "SF:/ci/workspace/src/lib.rs\nDA:1,1\n",
            Path::new("/w"),
            Some(Path::new("/ci/work")),
        );
        assert!(outside.files.is_empty());
    }
}
