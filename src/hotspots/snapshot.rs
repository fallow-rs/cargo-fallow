//! The health snapshots of `--save-snapshot` and the trend of `--trend`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::diff::git;
use crate::error::{CliError, ErrorCode};
use crate::health::Health;

/// The `kind` of a snapshot file.
pub const KIND: &str = "health-snapshot";
/// The version of the snapshot shape.
pub const SCHEMA_VERSION: u32 = 1;
/// The folder of the snapshots, relative to the workspace root.
pub const FOLDER: &str = ".fallow/snapshots";
/// A change under this size is `stable`.
const TOLERANCE: f64 = 0.05;

/// One snapshot file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "HealthSnapshot")]
pub struct Snapshot {
    /// The version of the snapshot shape.
    pub schema_version: u32,
    /// The version of cargo-fallow that wrote the snapshot.
    pub version: String,
    /// Always `health-snapshot`.
    pub kind: String,
    /// Always `rust`.
    pub language: String,
    /// The time of the run, in ISO 8601 form in UTC.
    pub timestamp: String,
    /// The commit of `HEAD`, when the workspace is in a git work tree.
    pub git_sha: Option<String>,
    /// The version of the formula of the health score.
    pub formula_version: Option<u32>,
    /// The health score. `None` for a run without a score.
    pub score: Option<f64>,
    /// The grade of the score.
    pub grade: Option<String>,
    /// The points of each penalty of the run, keyed by penalty name.
    pub penalties: BTreeMap<String, f64>,
    /// The number of findings.
    pub total_issues: usize,
    /// The checks of the run.
    #[serde(default)]
    pub checks: Vec<String>,
    /// True when the run kept only part of the findings: `--changed-since`,
    /// `--package` or `--baseline`.
    #[serde(default)]
    pub scoped: bool,
}

/// The facts of a run that a snapshot records.
pub struct Run<'a> {
    /// The health part of the report.
    pub health: Option<&'a Health>,
    /// The number of findings.
    pub total_issues: usize,
    /// The checks of the run.
    pub checks: Vec<String>,
    /// True when the run kept only part of the findings.
    pub scoped: bool,
}

/// The trend of a run against a snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, schemars::JsonSchema)]
pub struct HealthTrend {
    /// The snapshot of the comparison.
    pub compared_to: TrendPoint,
    /// The change of each value that both runs have.
    pub metrics: Vec<TrendMetric>,
    /// The snapshots in the folder, or 1 for `--trend-from`.
    pub snapshots_loaded: usize,
    /// `improving`, `declining` or `stable`: the direction of the score, or
    /// of the other values when a run has no score.
    #[schemars(extend("enum" = ["improving", "declining", "stable"]))]
    pub overall_direction: &'static str,
    /// Why some values are not comparable: another formula version, other
    /// checks or another scope. Absent when the runs match.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// The snapshot of a comparison.
#[derive(Debug, Clone, PartialEq, Serialize, schemars::JsonSchema)]
pub struct TrendPoint {
    /// The time of the snapshot.
    pub timestamp: String,
    /// The commit of the snapshot.
    pub git_sha: Option<String>,
    /// The score of the snapshot.
    pub score: Option<f64>,
    /// The grade of the snapshot.
    pub grade: Option<String>,
    /// The formula version of the snapshot score.
    pub formula_version: Option<u32>,
}

/// The change of one value.
#[derive(Debug, Clone, PartialEq, Serialize, schemars::JsonSchema)]
pub struct TrendMetric {
    /// `score`, `total_issues` or the name of a penalty.
    pub name: String,
    /// The value of the snapshot.
    pub previous: f64,
    /// The value of the run.
    pub current: f64,
    /// `current - previous`.
    pub delta: f64,
    /// `improving`, `declining` or `stable`. A higher score is better. A
    /// lower penalty or issue count is better.
    #[schemars(extend("enum" = ["improving", "declining", "stable"]))]
    pub direction: &'static str,
    /// `points` for the score and a penalty, `count` for the issues.
    #[schemars(extend("enum" = ["points", "count"]))]
    pub unit: &'static str,
}

/// The snapshot of a run.
pub fn of_run(root: &Path, run: Run<'_>, now: u64) -> Snapshot {
    let score = match run.health {
        Some(Health::Score(score)) => Some(score),
        _ => None,
    };
    let penalties = score
        .and_then(|score| serde_json::to_value(&score.penalties).ok())
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    Snapshot {
        schema_version: SCHEMA_VERSION,
        version: crate::report::TOOL_VERSION.to_owned(),
        kind: KIND.to_owned(),
        language: "rust".to_owned(),
        timestamp: iso_time(now),
        git_sha: git(root, &["rev-parse", "HEAD"])
            .ok()
            .map(|sha| String::from_utf8_lossy(&sha).trim().to_owned())
            .filter(|sha| !sha.is_empty()),
        formula_version: score.map(|score| score.formula_version),
        score: score.and_then(|score| score.score),
        grade: score.and_then(|score| score.grade).map(str::to_owned),
        penalties,
        total_issues: run.total_issues,
        checks: run.checks,
        scoped: run.scoped,
    }
}

/// The file name of a snapshot in [`FOLDER`]: the time, with `-` for `:`
/// so the name is valid on Windows.
fn file_name(now: u64, suffix: u32) -> String {
    let time = iso_time(now).replace(':', "-");
    if suffix == 0 {
        format!("{time}.json")
    } else {
        format!("{time}-{suffix}.json")
    }
}

/// True for a file name that [`file_name`] gives.
fn is_snapshot_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".json") else {
        return false;
    };
    let bytes = stem.as_bytes();
    bytes.len() >= 20
        && bytes[..20]
            .iter()
            .enumerate()
            .all(|(index, byte)| match index {
                4 | 7 | 13 | 16 => *byte == b'-',
                10 => *byte == b'T',
                19 => *byte == b'Z',
                _ => byte.is_ascii_digit(),
            })
        && (bytes.len() == 20
            || bytes[20] == b'-' && bytes[21..].iter().all(u8::is_ascii_digit) && bytes.len() > 21)
}

/// Writes a snapshot of `--save-snapshot`: to the given path, or to a new
/// file in [`FOLDER`]. A second run in the same second gets a suffix, so it
/// does not replace the first file. Returns the path.
pub fn save(root: &Path, given: &str, now: u64, snapshot: &Snapshot) -> Result<PathBuf, CliError> {
    let failed = |path: &Path, err: &dyn std::fmt::Display| {
        CliError::new(
            ErrorCode::OutputFailed,
            format!("cannot write the snapshot `{}`: {err}", path.display()),
        )
    };
    let text =
        serde_json::to_string_pretty(snapshot).map_err(|err| failed(Path::new(given), &err))?;
    let text = format!("{text}\n");
    if !given.is_empty() {
        let path = PathBuf::from(given);
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|err| failed(&path, &err))?;
        }
        fs::write(&path, &text).map_err(|err| failed(&path, &err))?;
        return Ok(path);
    }
    let folder = root.join(FOLDER);
    fs::create_dir_all(&folder).map_err(|err| failed(&folder, &err))?;
    for suffix in 0..1000 {
        let path = folder.join(file_name(now, suffix));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write as _;
                file.write_all(text.as_bytes())
                    .map_err(|err| failed(&path, &err))?;
                return Ok(path);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(failed(&path, &err)),
        }
    }
    Err(failed(&folder, &"each file name of this second is in use"))
}

/// Reads one snapshot.
pub fn load(path: &Path) -> Result<Snapshot, CliError> {
    let failed = |err: &dyn std::fmt::Display| {
        CliError::new(
            ErrorCode::InvalidArgument,
            format!("cannot read the snapshot `{}`: {err}", path.display()),
        )
        .with_help("Give a file that `--save-snapshot` wrote.")
    };
    let text = fs::read_to_string(path).map_err(|err| failed(&err))?;
    let snapshot: Snapshot = serde_json::from_str(&text).map_err(|err| failed(&err))?;
    if snapshot.kind != KIND {
        return Err(failed(&format!("the `kind` is not `{KIND}`")));
    }
    Ok(snapshot)
}

/// The newest snapshot in [`FOLDER`] that reads, and the number of snapshot
/// files, or `None` when the folder has none. Only a file name that
/// `--save-snapshot` gives counts, and those names sort by time. A file that
/// does not read gives a warning, and the next older file counts.
pub fn newest(root: &Path) -> Option<(Snapshot, usize)> {
    let entries = fs::read_dir(root.join(FOLDER)).ok()?;
    // The time part of the name sorts as text, and the suffix as a number.
    let mut files: Vec<(String, u32, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if !is_snapshot_name(&name) {
                return None;
            }
            let suffix = name[20..name.len() - ".json".len()]
                .trim_start_matches('-')
                .parse()
                .unwrap_or(0);
            Some((name[..20].to_owned(), suffix, entry.path()))
        })
        .collect();
    files.sort();
    let count = files.len();
    for (_, _, path) in files.iter().rev() {
        match load(path) {
            Ok(snapshot) => return Some((snapshot, count)),
            Err(err) => crate::render::warn(&format!("{}. The trend skips the file.", err.message)),
        }
    }
    None
}

/// The trend of the run `current` against `previous`.
pub fn trend(previous: &Snapshot, current: &Snapshot, snapshots_loaded: usize) -> HealthTrend {
    let mut metrics = Vec::new();
    let mut warnings = Vec::new();
    // Another formula gives other points for the same code.
    let same_formula = previous.formula_version == current.formula_version
        || previous.formula_version.is_none()
        || current.formula_version.is_none();
    if same_formula {
        if let (Some(before), Some(now)) = (previous.score, current.score) {
            metrics.push(metric("score", before, now, true, "points"));
        }
        for (name, now) in &current.penalties {
            if let Some(before) = previous.penalties.get(name) {
                metrics.push(metric(name, *before, *now, false, "points"));
            }
        }
    } else {
        warnings.push(format!(
            "the snapshot has formula version {}, and the run has {}, so the trend does not compare the score and the penalties",
            previous.formula_version.unwrap_or_default(),
            current.formula_version.unwrap_or_default()
        ));
    }
    if !previous.checks.is_empty() && previous.checks != current.checks {
        warnings.push(format!(
            "the snapshot ran the checks {}, and the run ran {}, so the issue counts differ in scope",
            previous.checks.join(", "),
            current.checks.join(", ")
        ));
    }
    if previous.scoped != current.scoped {
        warnings.push(
            "one of the runs kept only part of the findings, so the issue counts differ in scope"
                .to_owned(),
        );
    }
    metrics.push(metric(
        "total_issues",
        previous.total_issues as f64,
        current.total_issues as f64,
        false,
        "count",
    ));
    let overall_direction = match metrics.first() {
        Some(first) if first.name == "score" => first.direction,
        _ => combined(&metrics),
    };
    HealthTrend {
        compared_to: TrendPoint {
            timestamp: previous.timestamp.clone(),
            git_sha: previous.git_sha.clone(),
            score: previous.score,
            grade: previous.grade.clone(),
            formula_version: previous.formula_version,
        },
        metrics,
        snapshots_loaded,
        overall_direction,
        warnings,
    }
}

fn metric(
    name: &str,
    previous: f64,
    current: f64,
    higher_is_better: bool,
    unit: &'static str,
) -> TrendMetric {
    // Adding zero turns a negative zero into zero.
    let delta = ((current - previous) * 10.0).round() / 10.0 + 0.0;
    let direction = if delta.abs() < TOLERANCE {
        "stable"
    } else if (delta > 0.0) == higher_is_better {
        "improving"
    } else {
        "declining"
    };
    TrendMetric {
        name: name.to_owned(),
        previous,
        current,
        delta,
        direction,
        unit,
    }
}

/// The direction of a set of values: `improving` when some improve and none
/// declines, `declining` when some decline and none improves.
fn combined(metrics: &[TrendMetric]) -> &'static str {
    let any = |direction: &str| metrics.iter().any(|metric| metric.direction == direction);
    match (any("improving"), any("declining")) {
        (true, false) => "improving",
        (false, true) => "declining",
        _ => "stable",
    }
}

/// The time in ISO 8601 form in UTC, for example `2026-11-04T06:30:59Z`.
pub fn iso_time(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let rest = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// The calendar date of a day count since 1970-01-01, from the algorithm of
/// Howard Hinnant.
const fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the month is 1 to 12 and the day is 1 to 31"
    )]
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_time_gives_the_calendar_date() {
        assert_eq!(iso_time(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_time(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso_time(1_793_773_859), "2026-11-04T06:30:59Z");
    }

    fn snapshot(score: Option<f64>, penalties: &[(&str, f64)], issues: usize) -> Snapshot {
        Snapshot {
            schema_version: SCHEMA_VERSION,
            version: "0.0.0".to_owned(),
            kind: KIND.to_owned(),
            language: "rust".to_owned(),
            timestamp: iso_time(0),
            git_sha: None,
            formula_version: Some(2),
            score,
            grade: None,
            penalties: penalties
                .iter()
                .map(|(name, value)| ((*name).to_owned(), *value))
                .collect(),
            total_issues: issues,
            checks: Vec::new(),
            scoped: false,
        }
    }

    #[test]
    fn a_trend_compares_each_shared_value() {
        let before = snapshot(Some(80.0), &[("complexity", 4.0), ("unit_size", 2.0)], 10);
        let after = snapshot(Some(78.5), &[("complexity", 4.0), ("duplication", 1.0)], 7);
        let trend = trend(&before, &after, 3);
        let rows: Vec<(&str, f64, &str)> = trend
            .metrics
            .iter()
            .map(|metric| (metric.name.as_str(), metric.delta, metric.direction))
            .collect();
        assert_eq!(
            rows,
            [
                ("score", -1.5, "declining"),
                ("complexity", 0.0, "stable"),
                ("total_issues", -3.0, "improving"),
            ]
        );
        assert_eq!(trend.overall_direction, "declining");
        assert_eq!(trend.snapshots_loaded, 3);
    }

    #[test]
    fn another_formula_version_compares_only_the_issue_count() {
        let mut before = snapshot(Some(80.0), &[("complexity", 4.0)], 10);
        before.formula_version = Some(1);
        let after = snapshot(Some(70.0), &[("complexity", 9.0)], 10);
        let trend = trend(&before, &after, 1);
        let names: Vec<&str> = trend
            .metrics
            .iter()
            .map(|metric| metric.name.as_str())
            .collect();
        assert_eq!(names, ["total_issues"]);
        assert_eq!(trend.warnings.len(), 1, "{:?}", trend.warnings);
    }

    #[test]
    fn other_checks_or_another_scope_give_a_warning() {
        let mut before = snapshot(None, &[], 10);
        before.checks = vec!["complexity".to_owned()];
        let mut after = snapshot(None, &[], 4);
        after.checks = vec!["complexity".to_owned(), "size".to_owned()];
        after.scoped = true;
        assert_eq!(trend(&before, &after, 1).warnings.len(), 2);
    }

    #[test]
    fn a_small_negative_change_is_zero() {
        let row = metric("unit_size", 2.04, 2.0, false, "points");
        assert_eq!(row.delta.to_string(), "0");
        assert_eq!(row.direction, "stable");
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "a test fails loudly when its setup does not work"
    )]
    fn only_saved_names_are_snapshots_and_a_second_save_gets_a_suffix() {
        assert!(is_snapshot_name("2026-11-04T06-30-59Z.json"));
        assert!(is_snapshot_name("2026-11-04T06-30-59Z-2.json"));
        assert!(!is_snapshot_name("baseline.json"));
        assert!(!is_snapshot_name("2026-11-04T06-30-59Z-.json"));
        let dir = tempfile::tempdir().expect("a temporary directory");
        let first = save(dir.path(), "", 1_793_773_859, &snapshot(None, &[], 1)).expect("save");
        let second = save(dir.path(), "", 1_793_773_859, &snapshot(None, &[], 2)).expect("save");
        assert_ne!(first, second);
        std::fs::write(dir.path().join(FOLDER).join("zz-notes.json"), "{").expect("write");
        let (newest, count) = newest(dir.path()).expect("a snapshot");
        assert_eq!((newest.total_issues, count), (2, 2));
    }

    #[test]
    fn a_trend_without_a_score_follows_the_other_values() {
        let before = snapshot(None, &[("unit_size", 2.0)], 10);
        let after = snapshot(None, &[("unit_size", 1.0)], 10);
        assert_eq!(trend(&before, &after, 1).overall_direction, "improving");
    }
}
