//! The git churn of each file: commits, lines and a recency weight, from
//! `git log --numstat`.

use std::collections::BTreeMap;
use std::path::Path;

use crate::diff::{git, is_shallow};
use crate::error::{CliError, ErrorCode};

/// A commit of this age in days has half the weight of a commit of today.
/// The same half-life as fallow for JavaScript.
const HALF_LIFE_DAYS: f64 = 90.0;
/// The seconds in a day.
const DAY_SECONDS: f64 = 86_400.0;
/// The ratio of recent to older commits over which the churn accelerates.
const ACCELERATING_RATIO: f64 = 1.5;
/// The ratio of recent to older commits under which the churn cools.
const COOLING_RATIO: f64 = 0.67;

/// The churn of one file in the window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileChurn {
    /// The commits that changed the file.
    pub commits: u32,
    /// The sum of the recency weight of each commit.
    pub weighted_commits: f64,
    /// The added lines.
    pub lines_added: u32,
    /// The deleted lines.
    pub lines_deleted: u32,
    /// The commit times, in seconds since the Unix epoch.
    pub timestamps: Vec<u64>,
}

/// The churn of the workspace.
#[derive(Debug, Clone, Default)]
pub struct Churn {
    /// The churn of each file, keyed by the path relative to the workspace
    /// root.
    pub files: BTreeMap<String, FileChurn>,
    /// True when the repository is a shallow clone, so the history is cut.
    pub shallow_clone: bool,
}

/// The `--since` value as a git date: `6m`, `90d`, `2w`, `1y` or an ISO date
/// `2025-06-01`.
pub fn git_since(value: &str) -> Result<String, CliError> {
    let invalid = || {
        CliError::new(
            ErrorCode::InvalidArgument,
            format!("`--since {value}` is not a duration or a date"),
        )
        .with_help("Give a number with `d`, `w`, `m` or `y`, for example `6m` or `90d`, or a date such as `2025-06-01`.")
    };
    if is_iso_date(value) {
        return Ok(value.to_owned());
    }
    let unit = value.chars().last().ok_or_else(invalid)?;
    let count: u32 = value[..value.len() - unit.len_utf8()]
        .parse()
        .map_err(|_| invalid())?;
    let name = match unit {
        'd' => "days",
        'w' => "weeks",
        'm' => "months",
        'y' => "years",
        _ => return Err(invalid()),
    };
    if count == 0 {
        return Err(invalid());
    }
    Ok(format!("{count}.{name}.ago"))
}

fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        })
}

/// Reads the churn of the files under `root` since `since`, a git date. `now`
/// is the time of the run, in seconds since the Unix epoch. A shallow clone
/// gives no churn: its history is cut, so the counts are wrong.
pub fn read(root: &Path, since: &str, now: u64) -> Result<Churn, String> {
    if is_shallow(root) {
        return Ok(Churn {
            files: BTreeMap::new(),
            shallow_clone: true,
        });
    }
    let since = format!("--since={since}");
    let output = git(
        root,
        &[
            "-c",
            "core.quotePath=false",
            "log",
            &since,
            "--no-merges",
            "--no-renames",
            "--relative",
            "--numstat",
            "-z",
            "--format=%x1e%ct",
            "--",
            ".",
        ],
    )?;
    Ok(Churn {
        files: parse_log(&String::from_utf8_lossy(&output), now),
        shallow_clone: false,
    })
}

/// Reads `git log --numstat -z --format=%x1e%ct` output. Each field ends
/// with a NUL byte, so a path keeps each character, also a tab or a quote.
pub fn parse_log(text: &str, now: u64) -> BTreeMap<String, FileChurn> {
    let mut files: BTreeMap<String, FileChurn> = BTreeMap::new();
    let mut time: Option<u64> = None;
    for field in text.split('\0') {
        let field = field.trim_start_matches('\n');
        if let Some(header) = field.strip_prefix('\u{1e}') {
            time = header.trim().parse::<u64>().ok();
            continue;
        }
        let Some(time) = time else {
            continue;
        };
        let mut parts = field.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let age_days = now.saturating_sub(time) as f64 / DAY_SECONDS;
        let entry = files.entry(path.to_owned()).or_default();
        entry.commits += 1;
        entry.weighted_commits += 0.5_f64.powf(age_days / HALF_LIFE_DAYS);
        // A binary file has `-` for the counts.
        entry.lines_added += added.parse::<u32>().unwrap_or(0);
        entry.lines_deleted += deleted.parse::<u32>().unwrap_or(0);
        entry.timestamps.push(time);
    }
    files
}

/// The start of the window of a `--since` value, in seconds since the Unix
/// epoch, or `None` for a value that is not valid. A month counts 30 days and
/// a year 365 days, the same as git.
pub fn window_start(value: &str, now: u64) -> Option<u64> {
    if is_iso_date(value) {
        let year: i64 = value[..4].parse().ok()?;
        let month: i64 = value[5..7].parse().ok()?;
        let day: i64 = value[8..].parse().ok()?;
        let seconds = days_from_civil(year, month, day).checked_mul(86_400)?;
        return u64::try_from(seconds).ok();
    }
    let unit = value.chars().last()?;
    let count: u64 = value[..value.len() - unit.len_utf8()].parse().ok()?;
    let days = match unit {
        'd' => 1,
        'w' => 7,
        'm' => 30,
        'y' => 365,
        _ => return None,
    };
    Some(now.saturating_sub(count.saturating_mul(days).saturating_mul(86_400)))
}

/// The day count since 1970-01-01 of a calendar date, from the algorithm of
/// Howard Hinnant.
const fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The direction of the churn: the commits in the newer half of the window
/// from `start` to `end` against the commits in the older half.
pub fn trend(timestamps: &[u64], start: u64, end: u64) -> &'static str {
    if end <= start || timestamps.is_empty() {
        return "stable";
    }
    let middle = start + (end - start) / 2;
    let recent = timestamps.iter().filter(|time| **time > middle).count() as f64;
    let older = timestamps.iter().filter(|time| **time <= middle).count() as f64;
    if older == 0.0 {
        return if recent > 1.0 {
            "accelerating"
        } else {
            "stable"
        };
    }
    let ratio = recent / older;
    if ratio > ACCELERATING_RATIO {
        "accelerating"
    } else if ratio < COOLING_RATIO {
        "cooling"
    } else {
        "stable"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_values_become_git_dates() {
        assert_eq!(git_since("6m").ok().as_deref(), Some("6.months.ago"));
        assert_eq!(git_since("90d").ok().as_deref(), Some("90.days.ago"));
        assert_eq!(git_since("2w").ok().as_deref(), Some("2.weeks.ago"));
        assert_eq!(git_since("1y").ok().as_deref(), Some("1.years.ago"));
        assert_eq!(git_since("2025-06-01").ok().as_deref(), Some("2025-06-01"));
        for bad in ["", "m", "0d", "6x", "-1d", "2025-6-1", "6 m"] {
            assert!(git_since(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_log_gives_commits_lines_and_weights() {
        let now = 1_000 * 86_400;
        let half_life_ago = now - 90 * 86_400;
        let log = format!(
            "\u{1e}{now}\0\n3\t1\ta.rs\0-\t-\timage.png\0\u{1e}{half_life_ago}\0\n2\t0\ta.rs\01\t1\tdir/b\tc \"d\".rs\0"
        );
        let files = parse_log(&log, now);
        let a = &files["a.rs"];
        assert_eq!((a.commits, a.lines_added, a.lines_deleted), (2, 5, 1));
        assert!((a.weighted_commits - 1.5).abs() < 1e-9, "{a:?}");
        assert_eq!(files["image.png"].lines_added, 0);
        assert_eq!(files["dir/b\tc \"d\".rs"].commits, 1, "{files:?}");
    }

    #[test]
    fn the_window_start_follows_the_since_value() {
        let now = 100 * 86_400;
        assert_eq!(window_start("10d", now), Some(90 * 86_400));
        assert_eq!(window_start("1w", now), Some(93 * 86_400));
        assert_eq!(window_start("2025-06-01", now), Some(1_748_736_000));
        assert_eq!(window_start("1970-01-01", now), Some(0));
        assert_eq!(window_start("6x", now), None);
    }

    #[test]
    fn the_trend_compares_the_halves_of_the_window() {
        assert_eq!(trend(&[], 0, 100), "stable");
        assert_eq!(trend(&[60, 90, 95], 0, 100), "accelerating");
        assert_eq!(trend(&[5, 10, 20, 100], 0, 100), "cooling");
        assert_eq!(trend(&[10, 90], 0, 100), "stable");
        // The span of the commits alone would call this cooling.
        assert_eq!(trend(&[1, 2, 99, 100], 0, 100), "stable");
        assert_eq!(trend(&[99], 0, 100), "stable");
    }
}
