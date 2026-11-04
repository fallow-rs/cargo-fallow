//! Duplicated code: clone groups on normalized token streams.
//!
//! The clone model is the model of fallow for JavaScript:
//!
//! 1. Each file gives a normalized token stream (see [`tokens`]). The streams
//!    join into one sequence, with a unique separator after each file, so no
//!    repeat crosses a file end.
//! 2. A suffix array and an LCP array give each repeat of at least
//!    `min-tokens` tokens. All places of one repeat form one raw group.
//! 3. In one file, a place that overlaps the place before it is dropped. A
//!    group needs two places.
//! 4. From the longest group down, a group whose token ranges a kept group
//!    already covers is dropped.
//! 5. A group under `min-lines` lines is dropped. With `skip-local`, a group
//!    with all places in one folder is dropped. Places in one file whose lines
//!    overlap join.
//! 6. From the most tokens down, a group whose line ranges kept groups already
//!    cover is dropped.
//! 7. A `// fallow-ignore-next-line code-duplication` comment above a place
//!    removes the place. A group under `min-occurrences` places is dropped.
//!
//! Groups whose places are in the same set of files form a clone family.
//!
//! [`Limits`] keeps the work bounded on data files, such as a large table
//! without a generator header:
//!
//! - A file with more tokens than the budget is skipped, with the reason in
//!   `skipped_files`.
//! - In a run of a repeated pattern of up to [`MAX_PERIOD`] tokens, each token
//!   after the first [`Limits::run_cap`] tokens becomes unique. Without this
//!   step, a run of `n` tokens gives about `n` nested repeats with about `n`
//!   places each, so the time and the memory grow with the square of `n`.
//! - A repeat keeps at most [`Limits::max_places`] places.
//! - The search stops with an error at [`Limits::deadline`].

mod suffix;
pub mod tokens;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;
use xxhash_rust::xxh3::xxh3_64;

use crate::baseline::Entry;
use crate::error::{CliError, ErrorCode};
use crate::finding::{self as shared, Category, Draft, Impact, Location, Severity};
use crate::gates::RustGateName as Gate;
use crate::render::Progress;
use crate::scope::Scope;
use crate::section::{Section, to_value};
use crate::structure::DupesArgs;
pub use crate::structure::config::Mode;
use crate::structure::config::{self, Duplication, matches_prefix};
use crate::structure::report::{Action, Report, count_gate, gates_text, plural, shared_actions};
use crate::structure::sarif;
use crate::structure::source::{self, File, Loaded, Needs, Skipped};
use crate::workspace;

/// The rule of a clone group.
const DUPES_RULE: &str = "code-duplication";
/// The comment kind that suppresses a clone instance.
const SUPPRESS_KIND: &str = "code-duplication";
/// The suppression comment.
const SUPPRESS_COMMENT: &str = "// fallow-ignore-next-line code-duplication";
/// A family with this many duplicated lines or more gets one module step.
const FAMILY_MODULE_LINES: u32 = 50;
/// The longest repeated pattern, in tokens, that the run cap reads.
const MAX_PERIOD: usize = 4;

/// The bounds of the clone search.
#[derive(Debug, Clone)]
pub struct Limits {
    /// The most places of one repeat. A repeat with more places keeps the
    /// first places in suffix order.
    pub max_places: usize,
    /// The tokens of a repeated pattern that stay comparable.
    pub run_cap: usize,
    /// The most tokens of one file. A larger file is skipped.
    pub max_file_tokens: usize,
    /// The time after which the search stops.
    pub deadline: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_places: 128,
            run_cap: 256,
            max_file_tokens: 250_000,
            deadline: Duration::from_secs(300),
        }
    }
}

/// The search did not finish before [`Limits::deadline`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedOut;

/// The start of a search and its deadline.
struct Clock {
    started: Instant,
    deadline: Duration,
}

impl Clock {
    fn check(&self) -> Result<(), TimedOut> {
        if self.started.elapsed() >= self.deadline {
            Err(TimedOut)
        } else {
            Ok(())
        }
    }
}

/// One place of a clone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct Instance {
    /// The file.
    pub file: String,
    /// The first line.
    pub start_line: u32,
    /// The last line.
    pub end_line: u32,
    /// The first column, starting at 1.
    pub start_col: u32,
    /// The column after the last character.
    pub end_col: u32,
}

/// One clone group.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct CloneGroup {
    /// A stable id: `dup:` and the hash of the normalized tokens.
    pub fingerprint: String,
    /// The places, sorted by file and line.
    pub instances: Vec<Instance>,
    /// The number of tokens of the clone.
    pub token_count: u32,
    /// The largest line count of a place.
    pub line_count: u32,
    /// The next steps.
    pub actions: Vec<Action>,
}

/// A set of clone groups in the same files.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct CloneFamily {
    /// The files.
    pub files: Vec<String>,
    /// The fingerprints of the groups.
    pub groups: Vec<String>,
    /// The sum of the line counts of the groups.
    pub total_duplicated_lines: u32,
    /// The sum of the token counts of the groups.
    pub total_duplicated_tokens: u32,
    /// The next steps.
    pub actions: Vec<Action>,
}

/// The counts of a run.
#[derive(Debug, Clone, Default, Serialize, schemars::JsonSchema)]
pub struct Stats {
    /// The files in the token streams.
    pub total_files: usize,
    /// The files with a clone.
    pub files_with_clones: usize,
    /// The lines of the files.
    pub total_lines: u64,
    /// The lines in a clone, counted once for each line.
    pub duplicated_lines: u64,
    /// The tokens of the files.
    pub total_tokens: u64,
    /// The tokens that a removal of the clones would save.
    pub duplicated_tokens: u64,
    /// The clone groups.
    pub clone_groups: usize,
    /// The clone families.
    pub clone_families: usize,
    /// The places of all clone groups.
    pub clone_instances: usize,
    /// `duplicated_lines` as a percentage of `total_lines`.
    pub duplication_percentage: f64,
    /// The groups under `min-occurrences` places.
    pub clone_groups_below_min_occurrences: usize,
}

/// The summary of a duplication run.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[schemars(rename = "DupesSummary")]
pub struct Summary {
    /// The normalization mode.
    #[schemars(extend("enum" = ["strict", "mild", "weak", "semantic"]))]
    pub mode: &'static str,
    /// The smallest clone, in tokens.
    pub min_tokens: u32,
    /// The smallest clone, in lines.
    pub min_lines: u32,
    /// The counts.
    pub stats: Stats,
    /// The files with a generator header or an `@generated` comment, which
    /// the check skips.
    pub generated_files: usize,
    /// The clone families.
    pub clone_families: Vec<CloneFamily>,
    /// The files that the check skips, with the reason: a file that did not
    /// parse, or a file over the token budget.
    pub skipped_files: Vec<Skipped>,
}

pub fn run(args: &DupesArgs) -> Result<Section, CliError> {
    let progress = Progress::new(args.common.quiet);
    progress.step("Reading cargo metadata");
    let workspace = workspace::load(args.common.manifest_path.as_deref())?;
    let mut settings: Duplication = config::section(&workspace, "dupes")?;
    if let Some(mode) = args.mode {
        settings.mode = mode;
    }
    if let Some(value) = args.min_tokens {
        settings.min_tokens = value;
    }
    if let Some(value) = args.min_lines {
        settings.min_lines = value;
    }
    if let Some(value) = args.threshold {
        settings.threshold = value;
    }
    progress.step("Reading the token streams");
    let loaded = source::load(
        &workspace,
        Needs {
            functions: false,
            tokens: Some((settings.mode, settings.ignore_imports)),
        },
    );
    // `--threshold` is the flag of the threshold gate. A threshold from the
    // config only reports the gate, the same as each other gate without its flag.
    let gates = Enforce {
        clones: args.fail_on_clones,
        threshold: args.threshold.is_some(),
    };
    // A flag that changes the clone search narrows the findings.
    let scope = Scope::load(&workspace, &args.common.scope)?
        .narrowed(args.mode.is_some() || args.min_tokens.is_some() || args.min_lines.is_some());
    scope.annotate(section(&loaded, &settings, gates, &scope, progress)?)
}

/// The gates of the check that fail the run.
#[derive(Debug, Clone, Copy)]
pub struct Enforce {
    /// The `duplication-findings` gate.
    pub clones: bool,
    /// The `duplication-threshold` gate.
    pub threshold: bool,
}

/// Finds the clone groups in `loaded`. The load must have the tokens of
/// `settings`. `enforced` names the gates that fail the run.
pub fn section(
    loaded: &Loaded,
    settings: &Duplication,
    enforced: Enforce,
    scope: &Scope,
    progress: Progress,
) -> Result<Section, CliError> {
    let files: Vec<&File> = loaded
        .files
        .iter()
        .filter(|file| !file.skip_metrics && !matches_prefix(&file.path, &settings.exclude))
        .collect();
    progress.step("Finding clones");
    let limits = Limits::default();
    let hides =
        |path: &str, line: u32| scope.hides("dupes", SUPPRESS_KIND, path, Some(line..=line));
    let found = detect(&files, settings, &limits, &hides).map_err(|TimedOut| {
        CliError::new(
            ErrorCode::TimedOut,
            format!(
                "the clone search did not finish in {} s",
                limits.deadline.as_secs()
            ),
        )
        .with_help("Add the large generated or data files to `exclude` in `[workspace.metadata.fallow.dupes]`.")
    })?;
    let mut skipped_files = loaded.skipped_files();
    skipped_files.extend(found.skipped.iter().cloned());
    skipped_files.sort();
    // A clone group counts when one of its copies is in a changed file.
    let found_groups = found.groups.len();
    let groups: Vec<_> = found
        .groups
        .into_iter()
        .filter(|group| {
            scope.keeps_any(
                group
                    .instances
                    .iter()
                    .map(|instance| instance.file.as_str()),
            )
        })
        // A clone group has one baseline entry: its fingerprint.
        .filter(|group| {
            scope.keeps_entry(Entry {
                check: "dupes".to_owned(),
                rule: DUPES_RULE.to_owned(),
                id: group.fingerprint.clone(),
            })
        })
        .collect();
    let mut stats = found.stats;
    let mut families = found.families;
    if groups.len() != found_groups {
        // The group counts follow the scope and the baseline. The line, token
        // and file counts stay those of the whole workspace.
        let kept: BTreeSet<&str> = groups
            .iter()
            .map(|group| group.fingerprint.as_str())
            .collect();
        families.retain(|family| {
            family
                .groups
                .iter()
                .any(|group| kept.contains(group.as_str()))
        });
        stats.clone_groups = groups.len();
        stats.clone_families = families.len();
        stats.clone_instances = groups.iter().map(|group| group.instances.len()).sum();
    }
    let summary = Summary {
        mode: settings.mode.as_str(),
        min_tokens: settings.min_tokens,
        min_lines: settings.min_lines,
        stats,
        generated_files: loaded.files.iter().filter(|file| file.skip_metrics).count(),
        clone_families: families,
        skipped_files,
    };
    let mut report = Report::new("dupes", summary, groups);
    report.gate_outcomes.insert(
        Gate::DuplicationFindings,
        count_gate(report.findings.len(), enforced.clones),
    );
    // The percentage counts the whole workspace, so a scoped run does not
    // judge it.
    if settings.threshold > 0.0 && scope.is_full() {
        let observed = report.summary.stats.duplication_percentage;
        let status = if observed > settings.threshold {
            fallow_report_protocol::GateStatus::Fail
        } else {
            fallow_report_protocol::GateStatus::Pass
        };
        report.gate_outcomes.insert(
            Gate::DuplicationThreshold,
            fallow_report_protocol::GateOutcome::measured(
                status,
                enforced.threshold,
                observed,
                settings.threshold,
            ),
        );
    }
    let findings = shared_findings(&report);
    Ok(Section {
        human: human(&report),
        json: to_value(&report)?,
        sarif: sarif_log(&findings, &report, enforced.clones),
        findings,
        gates: report.gate_outcomes,
    })
}

fn place(instance: &Instance) -> Location {
    Location {
        path: instance.file.clone(),
        line: Some(instance.start_line),
        column: Some(instance.start_col),
        end_line: Some(instance.end_line),
    }
}

/// The findings in the shared shape: one for each place of each clone group,
/// the same as the SARIF results. The id uses the group fingerprint, the file
/// and the first line of the place.
fn shared_findings(report: &Report<Summary, CloneGroup>) -> Vec<shared::Finding> {
    report
        .findings
        .iter()
        .flat_map(|group| {
            group.instances.iter().map(move |instance| {
                let line = instance.start_line.to_string();
                shared::Finding::new(Draft {
                    check: "dupes",
                    rule: DUPES_RULE,
                    severity: Severity::Warning,
                    message: format!(
                        "Clone {}: {} lines, {} tokens, {} instances.",
                        group.fingerprint,
                        group.line_count,
                        group.token_count,
                        group.instances.len()
                    ),
                    location: place(instance),
                    identity: &[&group.fingerprint, &instance.file, &line],
                    category: Category::Duplication,
                    impact: Impact::Minor,
                })
                .with_related(
                    group
                        .instances
                        .iter()
                        .filter(|other| *other != instance)
                        .map(place)
                        .collect(),
                )
                .with_actions(shared_actions(&group.actions))
            })
        })
        .collect()
}

/// The result of a clone search.
#[derive(Debug, Clone, Default)]
pub struct Found {
    /// The clone groups, sorted.
    pub groups: Vec<CloneGroup>,
    /// The clone families, sorted by files.
    pub families: Vec<CloneFamily>,
    /// The counts.
    pub stats: Stats,
    /// The files over the token budget.
    pub skipped: Vec<Skipped>,
}

/// A token range in one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Spot {
    file: usize,
    start: usize,
}

/// A clone group before the line step.
struct Raw {
    length: usize,
    places: Vec<Spot>,
}

/// Finds the clone groups in `files`, within `limits`.
/// `hides` is true for a copy that a suppression comment hides, by its file
/// and first line.
pub fn detect(
    files: &[&File],
    settings: &Duplication,
    limits: &Limits,
    hides: &dyn Fn(&str, u32) -> bool,
) -> Result<Found, TimedOut> {
    let clock = Clock {
        started: Instant::now(),
        deadline: limits.deadline,
    };
    let (kept_files, over): (Vec<&File>, Vec<&File>) = files
        .iter()
        .partition(|file| file.tokens.len() <= limits.max_file_tokens);
    let files = kept_files.as_slice();
    let skipped = over
        .iter()
        .map(|file| Skipped {
            path: file.path.clone(),
            reason: format!(
                "the file has {} tokens, over the limit of {} for the clone search",
                file.tokens.len(),
                limits.max_file_tokens
            ),
        })
        .collect();
    let (text, alphabet, starts) = sequence(files, limits.run_cap);
    clock.check()?;
    let raw = raw_groups(
        &text,
        alphabet,
        &starts,
        settings.min_tokens.max(1),
        limits,
        &clock,
    )?;
    let kept = drop_token_subsets(raw, files.len());
    clock.check()?;
    let mut groups: Vec<(CloneGroup, u64)> = kept
        .iter()
        .filter_map(|group| line_group(files, group, settings))
        .collect();
    clock.check()?;
    drop_line_subsets(&mut groups);
    let below = drop_suppressed(&mut groups, hides, settings.min_occurrences);
    set_fingerprints(&mut groups);
    let mut groups: Vec<CloneGroup> = groups.into_iter().map(|(group, _)| group).collect();
    groups.sort_by(|a, b| {
        let weight =
            |group: &CloneGroup| u64::from(group.token_count) * group.instances.len() as u64;
        weight(b)
            .cmp(&weight(a))
            .then_with(|| b.token_count.cmp(&a.token_count))
            .then_with(|| b.instances.len().cmp(&a.instances.len()))
            .then_with(|| first_key(a).cmp(&first_key(b)))
    });
    for group in &mut groups {
        group.actions = group_actions(group);
    }
    let families = families(&groups);
    let mut stats = stats(files, &text, &groups);
    stats.clone_families = families.len();
    stats.clone_groups_below_min_occurrences = below;
    Ok(Found {
        groups,
        families,
        stats,
        skipped,
    })
}

/// Each repeat of at least `min_tokens` tokens with two places that do not
/// overlap, the longest first.
fn raw_groups(
    text: &[u32],
    alphabet: usize,
    starts: &[usize],
    min_tokens: u32,
    limits: &Limits,
    clock: &Clock,
) -> Result<Vec<Raw>, TimedOut> {
    let order = suffix::suffix_array(text, alphabet);
    clock.check()?;
    let lcp = suffix::lcp_array(text, &order);
    let file_of = |position: usize| starts.partition_point(|&start| start <= position) - 1;
    let found = suffix::intervals(text, &order, &lcp, min_tokens);
    clock.check()?;
    let mut raw: Vec<Raw> = Vec::new();
    for interval in found {
        if raw.len().is_multiple_of(1024) {
            clock.check()?;
        }
        let length = interval.length as usize;
        let last = interval
            .last
            .min(interval.first + limits.max_places.max(2) - 1);
        let mut places: Vec<Spot> = order[interval.first..=last]
            .iter()
            .map(|&position| {
                let position = position as usize;
                let file = file_of(position);
                Spot {
                    file,
                    start: position - starts[file],
                }
            })
            .collect();
        places.sort_by_key(|place| (place.file, place.start));
        drop_overlaps(&mut places, length);
        if places.len() >= 2 {
            raw.push(Raw { length, places });
        }
    }
    raw.sort_by(|a, b| {
        b.length
            .cmp(&a.length)
            .then_with(|| a.places[0].file.cmp(&b.places[0].file))
            .then_with(|| a.places[0].start.cmp(&b.places[0].start))
    });
    Ok(raw)
}

/// Drops each group whose token ranges the kept groups already cover.
fn drop_token_subsets(raw: Vec<Raw>, files: usize) -> Vec<Raw> {
    let mut covered = Coverage::new(files);
    raw.into_iter()
        .filter(|group| {
            let ranges = group
                .places
                .iter()
                .map(|place| (place.file, place.start, place.start + group.length));
            if covered.contains_all(ranges.clone()) {
                return false;
            }
            for (file, start, end) in ranges {
                covered.add(file, start, end);
            }
            true
        })
        .collect()
}

/// Drops each group whose line ranges the kept groups already cover, from the
/// most tokens down.
fn drop_line_subsets(groups: &mut Vec<(CloneGroup, u64)>) {
    groups.sort_by(|(a, _), (b, _)| {
        b.token_count
            .cmp(&a.token_count)
            .then_with(|| b.instances.len().cmp(&a.instances.len()))
            .then_with(|| first_key(a).cmp(&first_key(b)))
    });
    let mut by_lines = LineCoverage::default();
    groups.retain(|(group, _)| {
        if by_lines.contains_all(&group.instances) {
            return false;
        }
        by_lines.add(&group.instances);
        true
    });
}

/// Removes the suppressed places, then the groups under two places or under
/// `min_occurrences`. Returns the number of groups under `min_occurrences`.
fn drop_suppressed(
    groups: &mut Vec<(CloneGroup, u64)>,
    hides: &dyn Fn(&str, u32) -> bool,
    min_occurrences: u32,
) -> usize {
    let mut below = 0;
    groups.retain_mut(|(group, _)| {
        group
            .instances
            .retain(|instance| !hides(&instance.file, instance.start_line));
        if group.instances.len() < 2 {
            return false;
        }
        if group.instances.len() < min_occurrences.max(2) as usize {
            below += 1;
            return false;
        }
        true
    });
    below
}

/// The joined sequence of ranks, the alphabet size and the start of each file.
/// In a run of a repeated pattern, each token after the first `run_cap`
/// tokens gets a unique rank.
fn sequence(files: &[&File], run_cap: usize) -> (Vec<u32>, usize, Vec<usize>) {
    let mut ranks: BTreeMap<u64, u32> = BTreeMap::new();
    for file in files {
        for token in &file.tokens {
            ranks.entry(token.hash).or_insert(0);
        }
    }
    for (rank, value) in ranks.values_mut().enumerate() {
        *value = u32::try_from(rank).unwrap_or(u32::MAX);
    }
    let distinct = ranks.len();
    let total: usize = files.iter().map(|file| file.tokens.len() + 1).sum();
    let mut text = Vec::with_capacity(total);
    let mut starts = Vec::with_capacity(files.len());
    let mut next_unique = distinct + files.len();
    for (index, file) in files.iter().enumerate() {
        let start = text.len();
        starts.push(start);
        text.extend(file.tokens.iter().map(|token| ranks[&token.hash]));
        for at in long_runs(&text[start..], run_cap) {
            text[start + at] = u32::try_from(next_unique).unwrap_or(u32::MAX);
            next_unique += 1;
        }
        // A unique separator, so no repeat crosses the end of a file.
        text.push(u32::try_from(distinct + index).unwrap_or(u32::MAX));
    }
    (text, next_unique, starts)
}

/// The positions in `ranks` after the first `run_cap` tokens of a run of a
/// repeated pattern of up to [`MAX_PERIOD`] tokens.
fn long_runs(ranks: &[u32], run_cap: usize) -> Vec<usize> {
    let mut marked = vec![false; ranks.len()];
    for period in 1..=MAX_PERIOD {
        let mut run = period;
        for at in period..ranks.len() {
            if ranks[at] == ranks[at - period] {
                run += 1;
                if run > run_cap {
                    marked[at] = true;
                }
            } else {
                run = period;
            }
        }
    }
    marked
        .iter()
        .enumerate()
        .filter_map(|(at, &mark)| mark.then_some(at))
        .collect()
}

/// Drops each place that starts inside the place before it in the same file.
fn drop_overlaps(places: &mut Vec<Spot>, length: usize) {
    let mut kept: Vec<Spot> = Vec::with_capacity(places.len());
    for place in places.drain(..) {
        if let Some(last) = kept.last()
            && last.file == place.file
            && place.start < last.start + length
        {
            continue;
        }
        kept.push(place);
    }
    *places = kept;
}

/// The token ranges that kept groups cover, merged for each file.
struct Coverage {
    files: Vec<Vec<(usize, usize)>>,
}

impl Coverage {
    fn new(files: usize) -> Self {
        Self {
            files: vec![Vec::new(); files],
        }
    }

    fn contains_all(&self, mut ranges: impl Iterator<Item = (usize, usize, usize)>) -> bool {
        ranges.all(|(file, start, end)| contains(&self.files[file], start, end))
    }

    fn add(&mut self, file: usize, start: usize, end: usize) {
        insert_merged(&mut self.files[file], start, end, false);
    }
}

/// True when one merged range holds `start..end`.
fn contains(ranges: &[(usize, usize)], start: usize, end: usize) -> bool {
    let index = ranges.partition_point(|&(first, _)| first <= start);
    index > 0 && ranges[index - 1].1 >= end
}

/// Adds `start..end` to sorted, merged ranges. With `touch`, ranges that only
/// touch also merge.
fn insert_merged(ranges: &mut Vec<(usize, usize)>, start: usize, end: usize, touch: bool) {
    let gap = usize::from(touch);
    let first = ranges.partition_point(|&(_, last)| last + gap < start);
    let mut merged = (start, end);
    let mut last = first;
    while last < ranges.len() && ranges[last].0 <= merged.1 + gap {
        merged.0 = merged.0.min(ranges[last].0);
        merged.1 = merged.1.max(ranges[last].1);
        last += 1;
    }
    ranges.splice(first..last, [merged]);
}

/// The line ranges that kept groups cover, merged for each file.
#[derive(Default)]
struct LineCoverage {
    files: BTreeMap<String, Vec<(usize, usize)>>,
}

impl LineCoverage {
    fn contains_all(&self, instances: &[Instance]) -> bool {
        instances.iter().all(|instance| {
            self.files.get(&instance.file).is_some_and(|ranges| {
                contains(
                    ranges,
                    instance.start_line as usize,
                    instance.end_line as usize,
                )
            })
        })
    }

    fn add(&mut self, instances: &[Instance]) {
        for instance in instances {
            let ranges = self.files.entry(instance.file.clone()).or_default();
            insert_merged(
                ranges,
                instance.start_line as usize,
                instance.end_line as usize,
                false,
            );
        }
    }
}

/// The group with lines and columns, or `None` when a line rule drops it. The
/// second value is the hash of the normalized tokens.
fn line_group(files: &[&File], group: &Raw, settings: &Duplication) -> Option<(CloneGroup, u64)> {
    let mut instances: Vec<Instance> = group
        .places
        .iter()
        .map(|place| {
            let tokens = &files[place.file].tokens;
            let first = tokens[place.start];
            let last = tokens[place.start + group.length - 1];
            Instance {
                file: files[place.file].path.clone(),
                start_line: first.line,
                end_line: last.end_line,
                start_col: first.column,
                end_col: last.end_column,
            }
        })
        .collect();
    let line_count = instances
        .iter()
        .map(|instance| instance.end_line - instance.start_line + 1)
        .max()
        .unwrap_or(0);
    if line_count < settings.min_lines {
        return None;
    }
    if settings.skip_local {
        let folders: BTreeSet<&str> = instances
            .iter()
            .map(|instance| parent(&instance.file))
            .collect();
        if folders.len() == 1 {
            return None;
        }
    }
    instances.sort_by(|a, b| (&a.file, a.start_line).cmp(&(&b.file, b.start_line)));
    let mut joined: Vec<Instance> = Vec::with_capacity(instances.len());
    for instance in instances {
        if let Some(last) = joined.last_mut()
            && last.file == instance.file
            && instance.start_line <= last.end_line
        {
            if instance.end_line > last.end_line {
                last.end_line = instance.end_line;
                last.end_col = instance.end_col;
            }
            continue;
        }
        joined.push(instance);
    }
    if joined.len() < 2 {
        return None;
    }
    // The hash reads the hash of each normalized token, not its rank. A rank
    // depends on each token of the workspace, so a change in another file
    // would change the fingerprint.
    let first = group.places[0];
    let mut bytes = Vec::with_capacity(group.length * 8);
    for token in &files[first.file].tokens[first.start..first.start + group.length] {
        bytes.extend_from_slice(&token.hash.to_le_bytes());
    }
    Some((
        CloneGroup {
            fingerprint: String::new(),
            instances: joined,
            token_count: u32::try_from(group.length).unwrap_or(u32::MAX),
            line_count,
            actions: Vec::new(),
        },
        xxh3_64(&bytes),
    ))
}

fn parent(path: &str) -> &str {
    Path::new(path)
        .parent()
        .and_then(Path::to_str)
        .unwrap_or_default()
}

fn first_key(group: &CloneGroup) -> (&str, u32) {
    group.instances.first().map_or(("", 0), |instance| {
        (instance.file.as_str(), instance.start_line)
    })
}

/// Sets `dup:` and 8 hex digits, or 16 digits for each group whose short
/// fingerprint another group also has.
fn set_fingerprints(groups: &mut [(CloneGroup, u64)]) {
    let mut seen: BTreeMap<u64, usize> = BTreeMap::new();
    for (_, hash) in groups.iter() {
        *seen.entry(hash >> 32).or_default() += 1;
    }
    for (group, hash) in groups.iter_mut() {
        group.fingerprint = if seen[&(*hash >> 32)] > 1 {
            format!("dup:{hash:016x}")
        } else {
            format!("dup:{:08x}", *hash >> 32)
        };
    }
}

fn group_actions(group: &CloneGroup) -> Vec<Action> {
    let count = group.instances.len();
    vec![
        Action::manual(
            "extract-shared",
            format!(
                "Extract duplicated code ({} lines, {count} {}) into a shared function",
                group.line_count,
                plural(count, "instance", "instances")
            ),
        ),
        Action::suppress(SUPPRESS_COMMENT, "above-duplicated-code"),
    ]
}

fn families(groups: &[CloneGroup]) -> Vec<CloneFamily> {
    let mut by_files: BTreeMap<Vec<String>, Vec<&CloneGroup>> = BTreeMap::new();
    for group in groups {
        let files: BTreeSet<&String> = group.instances.iter().map(|i| &i.file).collect();
        by_files
            .entry(files.into_iter().cloned().collect())
            .or_default()
            .push(group);
    }
    by_files
        .into_iter()
        .map(|(files, members)| {
            let lines: u32 = members.iter().map(|group| group.line_count).sum();
            let tokens: u32 = members.iter().map(|group| group.token_count).sum();
            let count = members.len();
            let mut actions = vec![Action::manual(
                "extract-shared",
                format!(
                    "Extract {count} duplicated code {} ({lines} lines) into a shared {}",
                    plural(count, "block", "blocks"),
                    if lines >= FAMILY_MODULE_LINES {
                        "module"
                    } else {
                        "function"
                    },
                ),
            )];
            actions.push(Action::suppress(SUPPRESS_COMMENT, "above-duplicated-code"));
            CloneFamily {
                files,
                groups: members
                    .iter()
                    .map(|group| group.fingerprint.clone())
                    .collect(),
                total_duplicated_lines: lines,
                total_duplicated_tokens: tokens,
                actions,
            }
        })
        .collect()
}

fn stats(files: &[&File], text: &[u32], groups: &[CloneGroup]) -> Stats {
    let total_lines: u64 = files.iter().map(|file| u64::from(file.lines)).sum();
    let total_tokens = (text.len() - files.len()) as u64;
    let mut lines: BTreeMap<&str, Vec<(usize, usize)>> = BTreeMap::new();
    for instance in groups.iter().flat_map(|group| &group.instances) {
        let ranges = lines.entry(instance.file.as_str()).or_default();
        insert_merged(
            ranges,
            instance.start_line as usize,
            instance.end_line as usize,
            true,
        );
    }
    let duplicated_lines: u64 = lines
        .values()
        .flatten()
        .map(|(start, end)| (end - start + 1) as u64)
        .sum();
    let duplicated_tokens: u64 = groups
        .iter()
        .map(|group| u64::from(group.token_count) * (group.instances.len() as u64 - 1))
        .sum();
    #[expect(
        clippy::cast_precision_loss,
        reason = "a line count never approaches the f64 integer limit"
    )]
    let percentage = if total_lines == 0 {
        0.0
    } else {
        duplicated_lines as f64 / total_lines as f64 * 100.0
    };
    Stats {
        total_files: files.len(),
        files_with_clones: lines.len(),
        total_lines,
        duplicated_lines,
        total_tokens,
        duplicated_tokens: duplicated_tokens.min(total_tokens),
        clone_groups: groups.len(),
        clone_families: 0,
        clone_instances: groups.iter().map(|group| group.instances.len()).sum(),
        duplication_percentage: (percentage * 100.0).round() / 100.0,
        clone_groups_below_min_occurrences: 0,
    }
}

fn human(report: &Report<Summary, CloneGroup>) -> String {
    let stats = &report.summary.stats;
    let mut text = format!(
        "cargo fallow dupes: {} {}, {} clone {}, {} of {} lines duplicated ({:.2}%)\n",
        stats.total_files,
        plural(stats.total_files, "file", "files"),
        stats.clone_groups,
        plural(stats.clone_groups, "group", "groups"),
        stats.duplicated_lines,
        stats.total_lines,
        stats.duplication_percentage,
    );
    if report.findings.is_empty() {
        text.push_str("No clones found\n");
    }
    for group in &report.findings {
        let _ = writeln!(
            text,
            "  {} {} tokens, {} lines, {} {}",
            group.fingerprint,
            group.token_count,
            group.line_count,
            group.instances.len(),
            plural(group.instances.len(), "instance", "instances"),
        );
        for instance in &group.instances {
            let _ = writeln!(
                text,
                "    {}:{}-{}",
                instance.file, instance.start_line, instance.end_line
            );
        }
    }
    for file in &report.summary.skipped_files {
        let _ = writeln!(text, "  skipped {}: {}", file.path, file.reason);
    }
    text.push_str(&gates_text(&report.gate_outcomes));
    text
}

fn sarif_log(
    findings: &[shared::Finding],
    report: &Report<Summary, CloneGroup>,
    enforced: bool,
) -> serde_json::Value {
    let rules = BTreeMap::from([(
        "code-duplication".to_owned(),
        "A block of code has a copy in the workspace.".to_owned(),
    )]);
    sarif::log(
        &rules,
        findings,
        sarif::level(enforced),
        &report.gate_outcomes,
    )
}

#[cfg(test)]
mod tests;
