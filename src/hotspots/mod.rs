//! The file scores, the hotspots, the snapshots and the trend of `check`.
//!
//! The model is `fallow health` for JavaScript:
//!
//! - A file score gives the complexity density (the cyclomatic complexity per
//!   line) and a maintainability index of each Rust source file.
//! - A hotspot is a file that is complex and changes often. The score is the
//!   recency-weighted commit count times the complexity density, each
//!   normalized to the largest value of the ranked files, on a scale of 0 to
//!   100.
//! - A snapshot saves the health score, its penalties and the issue count. A
//!   trend compares the run with a snapshot.

pub mod churn;
pub mod snapshot;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::error::{CliError, ErrorCode};
use crate::render;
use crate::structure::config::matches_prefix;
use crate::structure::source::Loaded;

/// The default history window of the hotspots.
pub const DEFAULT_SINCE: &str = "6m";
/// The default least number of commits for a hotspot.
pub const DEFAULT_MIN_COMMITS: u32 = 3;
/// A file under this many lines gets a smaller density weight in the
/// maintainability index. The same value as fallow for JavaScript.
const DENSITY_MIN_LINES: f64 = 50.0;
/// The points that a density of 1 takes from the maintainability index.
const DENSITY_WEIGHT: f64 = 30.0;
/// The points that a file with only dead `pub` items loses.
const DEAD_CODE_WEIGHT: f64 = 20.0;

/// The flags of the file scores, the hotspots and the trend.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct HealthArgs {
    /// Add `file_scores`: the complexity density and the maintainability
    /// index of each source file. Needs the complexity check.
    #[arg(long)]
    pub file_scores: bool,
    /// Add `hotspots`: the files that are complex and change often, ranked
    /// by git churn times complexity density. Needs the complexity check and
    /// the git history.
    #[arg(long)]
    pub hotspots: bool,
    /// The git history window of `--hotspots`: a duration (`6m`, `90d`,
    /// `2w`, `1y`) or a date (`2025-06-01`). The default is `6m`.
    #[arg(long, value_name = "DURATION", requires = "hotspots")]
    pub since: Option<String>,
    /// The least number of commits in the window for a hotspot. The default
    /// is 3.
    #[arg(long, value_name = "N", requires = "hotspots")]
    pub min_commits: Option<u32>,
    /// Save a snapshot of the health score, its penalties and the issue
    /// count for `--trend`. Without a path, the file goes to
    /// `.fallow/snapshots/` in the workspace root. A relative path is
    /// relative to the current folder.
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = "")]
    pub save_snapshot: Option<String>,
    /// Add `health_trend`: compare the run with the newest snapshot in
    /// `.fallow/snapshots/`.
    #[arg(long)]
    pub trend: bool,
    /// Compare the run with this snapshot file. Implies `--trend`. A relative
    /// path is relative to the current folder.
    #[arg(long, value_name = "PATH")]
    pub trend_from: Option<PathBuf>,
}

impl HealthArgs {
    /// True when the run needs the functions of each file.
    pub const fn needs_functions(&self) -> bool {
        self.file_scores || self.hotspots
    }

    /// Stops a run that asks for file scores or hotspots without the
    /// complexity check, or with an invalid `--since`.
    pub fn validate(&self, complexity: bool) -> Result<(), CliError> {
        if self.needs_functions() && !complexity {
            return Err(CliError::new(
                ErrorCode::InvalidArgument,
                "`--file-scores` and `--hotspots` need the complexity check",
            )
            .with_help("Add `complexity` to `--only`, or remove it from `--skip`."));
        }
        if let Some(path) = &self.trend_from {
            snapshot::load(path)?;
        }
        churn::git_since(self.since.as_deref().unwrap_or(DEFAULT_SINCE)).map(|_| ())
    }
}

/// The score of one source file.
#[derive(Debug, Clone, PartialEq, Serialize, schemars::JsonSchema)]
pub struct FileScore {
    /// The file, relative to the workspace root.
    pub path: String,
    /// The maintainability index from 0 to 100. Higher is better:
    /// `100 - density * 30 * min(lines / 50, 1) - dead_code_ratio * 20`.
    pub maintainability_index: f64,
    /// The cyclomatic complexity of all functions per line.
    pub complexity_density: f64,
    /// The sum of the cyclomatic complexity of the functions.
    pub total_cyclomatic: u32,
    /// The sum of the cognitive complexity of the functions.
    pub total_cognitive: u32,
    /// The functions and closures.
    pub function_count: usize,
    /// The lines.
    pub lines: u32,
    /// The `dead_public` findings per `pub` item of the file, from 0 to 1.
    /// Only a run with the visibility check has it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dead_code_ratio: Option<f64>,
    /// The density before rounding, for the index and the hotspot score.
    #[serde(skip)]
    #[schemars(skip)]
    raw_density: f64,
}

/// One hotspot.
#[derive(Debug, Clone, PartialEq, Serialize, schemars::JsonSchema)]
pub struct Hotspot {
    /// The file, relative to the workspace root.
    pub path: String,
    /// The churn times the complexity, from 0 to 100. Higher is riskier.
    pub score: f64,
    /// The commits that changed the file in the window.
    pub commits: u32,
    /// The commits with a recency weight: a commit of 90 days ago counts half.
    pub weighted_commits: f64,
    /// The added lines in the window.
    pub lines_added: u32,
    /// The deleted lines in the window.
    pub lines_deleted: u32,
    /// The cyclomatic complexity per line.
    pub complexity_density: f64,
    /// `accelerating`, `cooling` or `stable`: the commits in the newer half of
    /// the `--since` window against the commits in the older half.
    #[schemars(extend("enum" = ["accelerating", "cooling", "stable"]))]
    pub trend: &'static str,
}

/// The settings and the counts of the hotspot ranking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct HotspotSummary {
    /// The history window.
    pub since: String,
    /// The least number of commits for a hotspot.
    pub min_commits: u32,
    /// The ranked files.
    pub files_analyzed: usize,
    /// The source files with a commit in the window, but fewer than
    /// `min_commits`.
    pub files_excluded: usize,
    /// True when the repository is a shallow clone. Then the run ranks no
    /// file.
    pub shallow_clone: bool,
}

/// The `pub` items and the `dead_public` findings of each file.
pub type DeadCounts = BTreeMap<String, (usize, usize)>;

/// The score of each file that the complexity check measures, sorted by the
/// maintainability index, the lowest first, then by path. The scores have no
/// `dead_code_ratio`: [`with_dead_counts`] adds it.
pub fn file_scores(loaded: &Loaded, exclude: &[String]) -> Vec<FileScore> {
    let mut scores: Vec<FileScore> = loaded
        .files
        .iter()
        .filter(|file| !file.skip_metrics && !matches_prefix(&file.path, exclude))
        .map(|file| {
            let total_cyclomatic = file
                .functions
                .iter()
                .map(|function| function.cyclomatic)
                .sum();
            let total_cognitive = file
                .functions
                .iter()
                .map(|function| function.cognitive)
                .sum();
            let lines = file.lines.max(1);
            let density = f64::from(total_cyclomatic) / f64::from(lines);
            FileScore {
                path: file.path.clone(),
                maintainability_index: maintainability(density, lines, None),
                complexity_density: round(density, 3),
                total_cyclomatic,
                total_cognitive,
                function_count: file.functions.len(),
                lines: file.lines,
                dead_code_ratio: None,
                raw_density: density,
            }
        })
        .collect();
    sort_scores(&mut scores);
    scores
}

/// The file scores with the `dead_public` ratio of each file, when the
/// visibility check ran.
pub fn with_dead_counts(mut scores: Vec<FileScore>, dead: Option<&DeadCounts>) -> Vec<FileScore> {
    let Some(dead) = dead else {
        return scores;
    };
    for score in &mut scores {
        let ratio = match dead.get(&score.path) {
            Some(&(public, dead)) if public > 0 => dead as f64 / public as f64,
            _ => 0.0,
        };
        score.dead_code_ratio = Some(round(ratio, 3));
        score.maintainability_index =
            maintainability(score.raw_density, score.lines.max(1), Some(ratio));
    }
    sort_scores(&mut scores);
    scores
}

/// The maintainability index of a file, from 0 to 100, with one decimal.
fn maintainability(density: f64, lines: u32, dead_code_ratio: Option<f64>) -> f64 {
    let dampening = (f64::from(lines) / DENSITY_MIN_LINES).min(1.0);
    let complexity = (density * DENSITY_WEIGHT).mul_add(-dampening, 100.0);
    let index = dead_code_ratio
        .unwrap_or(0.0)
        .mul_add(-DEAD_CODE_WEIGHT, complexity);
    round(index.clamp(0.0, 100.0), 1)
}

fn sort_scores(scores: &mut [FileScore]) {
    scores.sort_by(|a, b| {
        a.maintainability_index
            .total_cmp(&b.maintainability_index)
            .then_with(|| a.path.cmp(&b.path))
    });
}

/// Adds the parts of the flags to the report: the file scores, the hotspots
/// and the trend, and writes the snapshot. The trend reads the old snapshot
/// before the run writes a new one.
pub fn attach(
    mut report: crate::check::report::CheckReport,
    root: &std::path::Path,
    args: &HealthArgs,
    scores: Option<Vec<FileScore>>,
) -> Result<crate::check::report::CheckReport, CliError> {
    if args.hotspots {
        let (ranked, summary) = rank(root, args, scores.as_deref().unwrap_or_default())?;
        report.hotspots = Some(ranked);
        report.hotspot_summary = Some(summary);
    }
    if args.file_scores {
        report.file_scores = scores;
    }
    let trend = args.trend || args.trend_from.is_some();
    if !trend && args.save_snapshot.is_none() {
        return Ok(report);
    }
    let now = now();
    let current = snapshot::of_run(
        root,
        snapshot::Run {
            health: report.health.as_ref(),
            total_issues: report.total_issues,
            checks: report
                .checks
                .iter()
                .map(|name| name.as_str().to_owned())
                .collect(),
            scoped: report.changed_since.is_some()
                || report.packages.is_some()
                || report.baseline.is_some(),
        },
        now,
    );
    if trend {
        let previous = match &args.trend_from {
            Some(path) => Some((snapshot::load(path)?, 1)),
            None => snapshot::newest(root),
        };
        match previous {
            Some((previous, count)) => {
                report.health_trend = Some(snapshot::trend(&previous, &current, count));
            }
            None => render::warn(&format!(
                "`--trend` found no snapshot in `{}`. Run with `--save-snapshot` first.",
                snapshot::FOLDER
            )),
        }
    }
    if let Some(given) = &args.save_snapshot {
        snapshot::save(root, given, now, &current)?;
    }
    Ok(report)
}

/// The hotspots of the scored files, sorted by score, the highest first, then
/// by path. A file needs `min_commits` commits in the window, and some
/// complexity: a file with no branch is not a hotspot. `window` is the start
/// and the end of the history window, in seconds since the Unix epoch.
pub fn hotspots(
    scores: &[FileScore],
    churn: &churn::Churn,
    min_commits: u32,
    window: (u64, u64),
) -> (Vec<Hotspot>, usize) {
    let mut excluded = 0;
    let eligible: Vec<(&FileScore, &churn::FileChurn)> = scores
        .iter()
        .filter(|score| score.raw_density > 0.0)
        .filter_map(|score| churn.files.get(&score.path).map(|churn| (score, churn)))
        .filter(|(_, churn)| {
            let keep = churn.commits >= min_commits;
            excluded += usize::from(!keep);
            keep
        })
        .collect();
    let max_weight = eligible
        .iter()
        .map(|(_, churn)| churn.weighted_commits)
        .fold(0.0, f64::max);
    let max_density = eligible
        .iter()
        .map(|(score, _)| score.raw_density)
        .fold(0.0, f64::max);
    let ratio = |value: f64, max: f64| if max > 0.0 { value / max } else { 0.0 };
    let mut ranked: Vec<Hotspot> = eligible
        .into_iter()
        .map(|(score, churn)| Hotspot {
            path: score.path.clone(),
            score: round(
                ratio(churn.weighted_commits, max_weight)
                    * ratio(score.raw_density, max_density)
                    * 100.0,
                1,
            ),
            commits: churn.commits,
            weighted_commits: round(churn.weighted_commits, 2),
            lines_added: churn.lines_added,
            lines_deleted: churn.lines_deleted,
            complexity_density: score.complexity_density,
            trend: churn::trend(&churn.timestamps, window.0, window.1),
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.path.cmp(&b.path))
    });
    (ranked, excluded)
}

/// The hotspots of a run with `--hotspots`, and their summary. A shallow
/// clone and a failed git read give a warning and no hotspot.
pub fn rank(
    root: &std::path::Path,
    args: &HealthArgs,
    scores: &[FileScore],
) -> Result<(Vec<Hotspot>, HotspotSummary), CliError> {
    let since = args
        .since
        .clone()
        .unwrap_or_else(|| DEFAULT_SINCE.to_owned());
    let min_commits = args.min_commits.unwrap_or(DEFAULT_MIN_COMMITS);
    let now = now();
    let start = churn::window_start(&since, now).unwrap_or(0);
    let churn = match churn::read(root, &churn::git_since(&since)?, now) {
        Ok(churn) => churn,
        Err(detail) => {
            render::warn(&format!(
                "cannot read the git history, so the run ranks no hotspot: {}",
                detail.trim()
            ));
            churn::Churn::default()
        }
    };
    if churn.shallow_clone {
        render::warn(
            "the repository is a shallow clone, so the run ranks no hotspot. Fetch the full \
             history, for example with `fetch-depth: 0`.",
        );
    }
    let (ranked, files_excluded) = hotspots(scores, &churn, min_commits, (start, now));
    let summary = HotspotSummary {
        since,
        min_commits,
        files_analyzed: ranked.len(),
        files_excluded,
        shallow_clone: churn.shallow_clone,
    };
    Ok((ranked, summary))
}

/// The number of rows of each list in the human text.
const HUMAN_ROWS: usize = 10;

/// The text of the file scores, the hotspots and the trend for a person.
pub fn human(report: &crate::check::report::CheckReport) -> String {
    use std::fmt::Write as _;
    let mut text = String::new();
    if let (Some(hotspots), Some(summary)) = (&report.hotspots, &report.hotspot_summary) {
        let _ = writeln!(
            text,
            "Hotspots (since {}, {} or more commits): {} {}",
            summary.since,
            summary.min_commits,
            hotspots.len(),
            if hotspots.len() == 1 { "file" } else { "files" }
        );
        for hotspot in hotspots.iter().take(HUMAN_ROWS) {
            let _ = writeln!(
                text,
                "  {:>5.1}  {}  {} commits, density {:.2}, {}",
                hotspot.score,
                hotspot.path,
                hotspot.commits,
                hotspot.complexity_density,
                hotspot.trend
            );
        }
    }
    if let Some(scores) = &report.file_scores {
        let _ = writeln!(text, "File scores, the least maintainable first:");
        for score in scores.iter().take(HUMAN_ROWS) {
            let _ = writeln!(
                text,
                "  {:>5.1}  {}  density {:.2}, {} functions",
                score.maintainability_index,
                score.path,
                score.complexity_density,
                score.function_count
            );
        }
    }
    if let Some(trend) = &report.health_trend {
        let _ = writeln!(
            text,
            "Trend against the snapshot of {}: {}",
            trend.compared_to.timestamp, trend.overall_direction
        );
        for metric in &trend.metrics {
            let _ = writeln!(
                text,
                "  {} {} -> {} ({:+}, {})",
                metric.name, metric.previous, metric.current, metric.delta, metric.direction
            );
        }
    }
    text
}

/// The time now, in seconds since the Unix epoch.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn round(value: f64, digits: i32) -> f64 {
    let scale = 10_f64.powi(digits);
    (value * scale).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(path: &str, density: f64) -> FileScore {
        FileScore {
            path: path.to_owned(),
            maintainability_index: 50.0,
            complexity_density: density,
            total_cyclomatic: 1,
            total_cognitive: 1,
            function_count: 1,
            lines: 10,
            dead_code_ratio: None,
            raw_density: density,
        }
    }

    fn churn_of(entries: &[(&str, u32, f64)]) -> churn::Churn {
        churn::Churn {
            files: entries
                .iter()
                .map(|(path, commits, weight)| {
                    (
                        (*path).to_owned(),
                        churn::FileChurn {
                            commits: *commits,
                            weighted_commits: *weight,
                            ..churn::FileChurn::default()
                        },
                    )
                })
                .collect(),
            shallow_clone: false,
        }
    }

    #[test]
    fn the_score_multiplies_the_normalized_churn_and_density() {
        let scores = [score("a.rs", 0.5), score("b.rs", 0.25), score("c.rs", 1.0)];
        let churn = churn_of(&[("a.rs", 4, 4.0), ("b.rs", 4, 2.0), ("c.rs", 1, 1.0)]);
        let (ranked, excluded) = hotspots(&scores, &churn, 3, (0, 1));
        assert_eq!(excluded, 1);
        let rows: Vec<(&str, f64)> = ranked
            .iter()
            .map(|hotspot| (hotspot.path.as_str(), hotspot.score))
            .collect();
        assert_eq!(rows, [("a.rs", 100.0), ("b.rs", 25.0)]);
    }

    #[test]
    fn a_file_with_no_complexity_is_not_a_hotspot() {
        let churn = churn_of(&[("a.rs", 4, 4.0), ("flat.rs", 9, 9.0)]);
        let (ranked, _) = hotspots(
            &[score("a.rs", 0.5), score("flat.rs", 0.0)],
            &churn,
            1,
            (0, 1),
        );
        let paths: Vec<&str> = ranked.iter().map(|hotspot| hotspot.path.as_str()).collect();
        assert_eq!(paths, ["a.rs"]);
        assert_eq!(ranked[0].score, 100.0);
    }

    #[test]
    fn no_churn_gives_no_hotspot() {
        let (ranked, excluded) =
            hotspots(&[score("a.rs", 1.0)], &churn::Churn::default(), 1, (0, 1));
        assert!(ranked.is_empty());
        assert_eq!(excluded, 0);
    }
}
