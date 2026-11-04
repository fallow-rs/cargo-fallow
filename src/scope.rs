//! The scope of a run: the findings that the report keeps.
//!
//! The analysis always reads the whole workspace. The scope only removes
//! findings from the report, before the gates count them. A full scope keeps
//! each finding. Two flags narrow the scope: `--changed-since` keeps the
//! findings in the changed files, and `--baseline` leaves out the findings of
//! an earlier run.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use clap::Args;
use serde_json::Value;

use crate::baseline::{self, Entry, Loaded, Recorder};
use crate::diff::{ChangedFiles, member_dirs, owner};
use crate::error::{CliError, ErrorCode};
use crate::finding::Finding;
use crate::gates::RustGateName;
use crate::render;
use crate::rules::{Level, Rules};
use crate::section::{Section, to_value};
use crate::suppress::Registry;
use crate::workspace::Workspace;

/// The flag that scopes a run to the changed files.
pub const CHANGED_SINCE_FLAG: &str = "--changed-since";

/// The scope flags of `check` and of each static command.
#[derive(Debug, Clone, Default, Args)]
pub struct ScopeArgs {
    /// Report only the findings in the files that changed since the fork point
    /// of this ref and `HEAD`. Uncommitted and untracked files count. The
    /// analysis still reads the whole workspace.
    #[arg(long, value_name = "REF")]
    pub changed_since: Option<String>,
    /// Leave out each finding whose id is in this baseline file, from
    /// `--save-baseline`.
    #[arg(long, value_name = "PATH", conflicts_with = "save_baseline")]
    pub baseline: Option<PathBuf>,
    /// Write the id, the check and the rule of each finding to this file, and
    /// exit 0. A later run with `--baseline` leaves out these findings.
    #[arg(long, value_name = "PATH", conflicts_with = "changed_since")]
    pub save_baseline: Option<PathBuf>,
    /// Exit 1 when the baseline has an entry with no finding in the run. The
    /// run needs `--baseline` or the `baseline` key of the configuration.
    #[arg(long)]
    pub fail_on_stale_baseline: bool,
    /// Add `_meta` to the JSON report: the meaning, an example and the fix of
    /// each rule with a finding. Only the JSON format has it. `cargo fallow
    /// explain <rule>` gives one rule.
    #[arg(long)]
    pub explain: bool,
    /// Report only the findings of this workspace member. The flag can repeat.
    /// The analysis still reads the whole workspace.
    #[arg(
        long = "package",
        short = 'p',
        value_name = "NAME",
        conflicts_with = "save_baseline"
    )]
    pub packages: Vec<String>,
}

/// The members of `--package`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packages {
    /// The names, sorted.
    pub names: Vec<String>,
    /// The manifests of the named members.
    manifests: BTreeSet<String>,
    /// The folder and the manifest of each member.
    dirs: Vec<(String, String)>,
}

impl Packages {
    /// The members with these names. A name that no member has is an error.
    pub fn new(workspace: &Workspace, names: &[String]) -> Result<Self, CliError> {
        let mut sorted: Vec<String> = names.to_vec();
        sorted.sort();
        sorted.dedup();
        let mut manifests = BTreeSet::new();
        for name in &sorted {
            let Some(member) = workspace.members.iter().find(|member| &member.name == name) else {
                let known: Vec<&str> = workspace
                    .members
                    .iter()
                    .map(|member| member.name.as_str())
                    .collect();
                return Err(CliError::new(
                    ErrorCode::InvalidArgument,
                    format!("`--package {name}` names no workspace member"),
                )
                .with_help(format!("The members are: {}.", known.join(", "))));
            };
            manifests.insert(member.manifest_path.clone());
        }
        let dirs = member_dirs(workspace)
            .into_iter()
            .map(|(dir, manifest)| (dir.to_owned(), manifest.to_owned()))
            .collect();
        Ok(Self {
            names: sorted,
            manifests,
            dirs,
        })
    }

    /// True when a named member owns `path`: the member with the deepest
    /// folder that holds the path.
    pub fn keeps(&self, path: &str) -> bool {
        let dirs: Vec<(&str, &str)> = self
            .dirs
            .iter()
            .map(|(dir, manifest)| (dir.as_str(), manifest.as_str()))
            .collect();
        self.manifests.contains(&owner(&dirs, path))
    }
}

/// The line of the human output that names the packages.
pub fn packages_line(packages: &Packages) -> String {
    format!(
        "Scope: {} {}\n",
        if packages.names.len() == 1 {
            "package"
        } else {
            "packages"
        },
        packages.names.join(", ")
    )
}

/// The key of the baseline file in `[workspace.metadata.fallow]`.
const BASELINE_KEY: &str = "baseline";

/// The baseline file of the run: `--baseline`, else the `baseline` key of
/// `[workspace.metadata.fallow]`, relative to the workspace root. A run with
/// `--save-baseline` reads no baseline from the configuration.
fn baseline_path(workspace: &Workspace, args: &ScopeArgs) -> Result<Option<PathBuf>, CliError> {
    if args.baseline.is_some() || args.save_baseline.is_some() {
        return Ok(args.baseline.clone());
    }
    let Some(value) = workspace
        .metadata
        .get("fallow")
        .and_then(|root| root.get(BASELINE_KEY))
    else {
        return Ok(None);
    };
    let Some(path) = value.as_str() else {
        return Err(CliError::new(
            crate::error::ErrorCode::InvalidConfig,
            "`baseline` in `[workspace.metadata.fallow]` is not a path",
        )
        .with_help("Set `baseline = \"fallow-baseline.json\"`, a path from the workspace root."));
    };
    check_baseline_key(path)?;
    let full = workspace.root.as_std_path().join(path);
    if !full.is_file() {
        return Err(CliError::new(
            crate::error::ErrorCode::InvalidConfig,
            format!("the baseline `{path}` of `[workspace.metadata.fallow]` does not exist"),
        )
        .with_help(format!(
            "Run `cargo fallow check --save-baseline {path}`, or remove the `baseline` key."
        )));
    }
    Ok(Some(full))
}

/// Checks that the `baseline` key is a path in the workspace root: relative,
/// with no `..` part.
pub fn check_baseline_key(path: &str) -> Result<(), CliError> {
    let relative = std::path::Path::new(path);
    let inside = relative.is_relative()
        && relative.components().all(|part| {
            matches!(
                part,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        });
    if inside {
        return Ok(());
    }
    Err(CliError::new(
        crate::error::ErrorCode::InvalidConfig,
        format!("the baseline `{path}` of `[workspace.metadata.fallow]` leaves the workspace root"),
    )
    .with_help("Use a path from the workspace root, with no `..` part."))
}

/// The line of the human output that names the changed files.
pub fn changed_line(changed: &ChangedFiles) -> String {
    let count = changed.files.len();
    let base: String = changed.base.chars().take(12).collect();
    format!(
        "Scope: {count} {} changed since `{}` (fork point {base})\n",
        if count == 1 { "file" } else { "files" },
        changed.reference,
    )
}

/// The line of the human output that names the baseline.
pub fn baseline_line(summary: &baseline::Summary) -> String {
    format!(
        "Baseline: `{}`, {} {} left out, {} stale\n",
        summary.path,
        summary.matched,
        if summary.matched == 1 {
            "finding"
        } else {
            "findings"
        },
        summary.stale.len(),
    )
}

/// The findings that a run keeps.
#[derive(Debug, Default)]
pub struct Scope {
    /// The changed files of `--changed-since`, or `None` for each file.
    pub changed: Option<ChangedFiles>,
    /// The members of `--package`, or `None` for each member.
    pub packages: Option<Packages>,
    /// The baseline of `--baseline`.
    baseline: Option<Loaded>,
    /// The entries for `--save-baseline`.
    recorder: Option<Recorder>,
    /// True when a stale baseline entry fails the run.
    fail_on_stale: bool,
    /// True when a flag of the command narrows the findings, for example
    /// `visibility --kind`. A baseline entry then can be stale with no change
    /// of the code, so the run does not judge stale entries.
    narrowed: bool,
    /// True when the JSON report gets `_meta` with the rules of its findings.
    explain: bool,
    /// The suppression comments of the workspace.
    suppressions: Registry,
    /// The files whose suppression comments can be stale: the `.rs` files of
    /// the target folders and the manifests.
    marked_files: Vec<String>,
    /// The folder of each member, by package name.
    member_folders: BTreeMap<String, String>,
    /// The levels of `[workspace.metadata.fallow.rules]`.
    rules: Rules,
}

impl Scope {
    /// The scope of `fix`: the members of `--package`, the rules table and
    /// the suppression comments. A baseline does not hide an edit, because
    /// `fix` must change the old findings too.
    pub fn for_fix(workspace: &Workspace, packages: &[String]) -> Result<Self, CliError> {
        let args = ScopeArgs {
            packages: packages.to_vec(),
            ..ScopeArgs::default()
        };
        Self::read(workspace, &args, false)
    }

    /// The scope of the flags.
    pub fn load(workspace: &Workspace, args: &ScopeArgs) -> Result<Self, CliError> {
        Self::read(workspace, args, true)
    }

    /// The scope of the flags. `baselines` is false when the run reads no
    /// baseline.
    fn read(workspace: &Workspace, args: &ScopeArgs, baselines: bool) -> Result<Self, CliError> {
        Ok(Self {
            changed: args
                .changed_since
                .as_deref()
                .map(|reference| ChangedFiles::load(workspace, CHANGED_SINCE_FLAG, reference))
                .transpose()?,
            packages: if args.packages.is_empty() {
                None
            } else {
                Some(Packages::new(workspace, &args.packages)?)
            },
            baseline: {
                let path = if baselines {
                    baseline_path(workspace, args)?
                } else {
                    None
                };
                if args.fail_on_stale_baseline && path.is_none() {
                    return Err(CliError::new(
                        crate::error::ErrorCode::InvalidArgument,
                        "`--fail-on-stale-baseline` needs a baseline",
                    )
                    .with_help(
                        "Add `--baseline <PATH>`, or set `baseline` in `[workspace.metadata.fallow]`.",
                    ));
                }
                path.as_deref().map(Loaded::read).transpose()?
            },
            recorder: args.save_baseline.clone().map(Recorder::new),
            fail_on_stale: args.fail_on_stale_baseline,
            narrowed: false,
            explain: args.explain,
            rules: Rules::load(workspace, &boundary_rules(workspace))?,
            suppressions: Registry::new(workspace.root.as_std_path(), boundary_rules(workspace)),
            marked_files: marked_files(workspace),
            member_folders: workspace
                .members
                .iter()
                .map(|member| {
                    let folder = member
                        .manifest_path
                        .rsplit_once('/')
                        .map_or("", |(folder, _)| folder);
                    (member.name.clone(), folder.to_owned())
                })
                .collect(),
        })
    }

    /// True when a suppression comment hides the finding of `check` and
    /// `rule` at `lines` of `path`. `lines` is `None` for a finding of the
    /// whole file.
    pub fn hides(
        &self,
        check: &str,
        rule: &str,
        path: &str,
        lines: Option<&std::ops::RangeInclusive<u32>>,
    ) -> bool {
        self.suppressions.hides(check, rule, path, lines)
    }

    /// True when a suppression comment hides a finding in the shared shape.
    pub fn hides_finding(&self, finding: &Finding) -> bool {
        let lines = finding
            .location
            .line
            .map(|line| line..=finding.location.end_line.unwrap_or(line).max(line));
        self.hides(
            finding.check,
            &finding.rule,
            &finding.location.path,
            lines.as_ref(),
        )
    }

    /// Adds the suppression results of the check of `section`: the number of
    /// hidden findings in `suppressed`, and a `stale-suppression` warning for
    /// each comment of the check that hid no finding. A scoped run judges only
    /// the comments in the files of its scope.
    pub fn attach_suppressions(&self, section: &mut Section) {
        let Some(check) = section.json["kind"]
            .as_str()
            .and_then(crate::suppress::check_name)
        else {
            return;
        };
        insert(
            &mut section.json,
            "suppressed",
            Value::from(self.suppressions.hidden(check)),
        );
        if !self.judges_stale_suppressions(check, section) {
            return;
        }
        let unjudged = unjudged_folders(&section.json, &self.member_folders);
        let files = self
            .marked_files
            .iter()
            .map(String::as_str)
            .filter(|path| self.keeps(path))
            .filter(|path| !crate::structure::config::matches_prefix(path, &unjudged));
        let stale = self.suppressions.stale(check, files);
        section.add_stale_suppressions(check, &stale);
    }

    /// True when a comment of `check` that hid nothing is stale in this run:
    /// no flag narrows the findings, and the check ran each of its rules.
    /// `size` gives its findings only with a growth comparison.
    fn judges_stale_suppressions(&self, check: &str, section: &Section) -> bool {
        if self.narrowed {
            return false;
        }
        check != "size"
            || section
                .gates
                .get(&RustGateName::FileGrowth)
                .is_some_and(|gate| gate.status != fallow_report_protocol::GateStatus::Skipped)
    }

    /// The scope of a run in which a flag narrows the findings.
    #[must_use]
    pub const fn narrowed(mut self, narrowed: bool) -> Self {
        self.narrowed = narrowed;
        self
    }

    /// True when the run can judge stale baseline entries: it has no file
    /// filter and no flag that narrows the findings.
    const fn judges_stale(&self) -> bool {
        self.is_full() && !self.narrowed
    }

    /// The `_meta` member of a JSON report with these findings, when the run
    /// has `--explain`.
    pub fn meta(&self, findings: &[Finding]) -> Result<Option<Value>, CliError> {
        if !self.explain {
            return Ok(None);
        }
        crate::explain::meta(
            findings
                .iter()
                .map(|finding| (finding.check, finding.rule.as_str())),
        )
        .map(Some)
    }

    /// True when the run removes the findings of a baseline.
    pub const fn has_baseline(&self) -> bool {
        self.baseline.is_some()
    }

    /// True when no file filter narrows the scope. A baseline does not
    /// count: it removes known findings, and the rest of the report still
    /// covers the whole workspace.
    pub const fn is_full(&self) -> bool {
        self.changed.is_none() && self.packages.is_none()
    }

    /// The name of the file filter of the run: `changed` for
    /// `--changed-since`, `package` for `--package`, or `None`.
    pub const fn filter(&self) -> Option<&'static str> {
        if self.changed.is_some() {
            Some("changed")
        } else if self.packages.is_some() {
            Some("package")
        } else {
            None
        }
    }

    /// True when the scope keeps a finding at `path`.
    pub fn keeps(&self, path: &str) -> bool {
        self.changed
            .as_ref()
            .is_none_or(|changed| changed.keeps(path))
            && self
                .packages
                .as_ref()
                .is_none_or(|packages| packages.keeps(path))
    }

    /// True when the scope keeps a finding with one of these places: one
    /// place passes each filter of the scope.
    pub fn keeps_any<'a>(&self, mut paths: impl Iterator<Item = &'a str>) -> bool {
        self.is_full() || paths.any(|path| self.keeps(path))
    }

    /// True when the baseline does not hold the entry. The entry then goes to
    /// the baseline file of `--save-baseline`.
    pub fn keeps_entry(&self, entry: Entry) -> bool {
        // A rule that is off gives no finding, so a baseline does not hold it.
        if self.rules.is_off(&[&entry.rule]) {
            return false;
        }
        if self
            .baseline
            .as_ref()
            .is_some_and(|baseline| baseline.absorbs(&entry.id))
        {
            return false;
        }
        if let Some(recorder) = &self.recorder {
            recorder.record(entry);
        }
        true
    }

    /// True when the baseline does not hold the entry that `entry` builds.
    /// A run without a baseline and without `--save-baseline` builds no
    /// entry.
    pub fn keeps_entry_of(&self, entry: impl FnOnce() -> Entry) -> bool {
        if self.baseline.is_none() && self.recorder.is_none() && self.rules.is_empty() {
            return true;
        }
        self.keeps_entry(entry())
    }

    /// True when the baseline does not hold the finding.
    pub fn keeps_finding(&self, finding: &Finding) -> bool {
        if self.baseline.is_none() && self.recorder.is_none() && self.rules.is_empty() {
            return true;
        }
        self.keeps_entry(Entry::of(finding))
    }

    /// Keeps the items whose finding in the shared shape the baseline does not
    /// hold. `shared` has one finding for each item, in the same order.
    pub fn retain_pairs<T>(&self, items: &mut Vec<T>, shared: &mut Vec<Finding>) {
        debug_assert_eq!(
            items.len(),
            shared.len(),
            "one shared finding for each item"
        );
        // A suppressed finding goes before the baseline sees it, so a saved
        // baseline does not hold it.
        let keep: Vec<bool> = shared
            .iter()
            .map(|finding| !self.hides_finding(finding) && self.keeps_finding(finding))
            .collect();
        let mut flags = keep.iter();
        items.retain(|_| flags.next().copied().unwrap_or(true));
        let mut flags = keep.iter();
        shared.retain(|_| flags.next().copied().unwrap_or(true));
    }

    /// Keeps the items for which `keep` is true. `shared` has one finding for
    /// each item, in the same order, and loses the same entries.
    pub fn retain_where<T>(
        items: &mut Vec<T>,
        shared: &mut Vec<Finding>,
        keep: impl Fn(&T) -> bool,
    ) {
        let flags: Vec<bool> = items.iter().map(&keep).collect();
        let mut kept = flags.iter();
        items.retain(|_| kept.next().copied().unwrap_or(true));
        let mut kept = flags.iter();
        shared.retain(|_| kept.next().copied().unwrap_or(true));
    }

    /// The levels of `[workspace.metadata.fallow.rules]`.
    pub const fn rules(&self) -> &Rules {
        &self.rules
    }

    /// Gives each finding of a configured rule its level: the severity of the
    /// finding in the shared shape, and the level of its SARIF result.
    pub fn apply_rules(&self, section: &mut Section) {
        if self.rules.is_empty() {
            return;
        }
        self.rules.apply(&mut section.findings);
        let Some(runs) = section.sarif["runs"].as_array_mut() else {
            return;
        };
        for result in runs
            .iter_mut()
            .filter_map(|run| run["results"].as_array_mut())
            .flatten()
        {
            let level = result["ruleId"]
                .as_str()
                .and_then(|rule| self.rules.level(&[rule]));
            let text = match level {
                Some(Level::Error) => "error",
                Some(Level::Warn) => "warning",
                _ => continue,
            };
            result["level"] = Value::from(text);
        }
    }

    /// The `baseline` member of a report on `checks`, or `None` without
    /// `--baseline`.
    pub fn baseline_summary(&self, checks: &[&str]) -> Option<baseline::Summary> {
        self.baseline.as_ref().map(|baseline| baseline::Summary {
            path: baseline.path.clone(),
            matched: baseline.matched(),
            stale: if self.judges_stale() {
                baseline.stale(checks)
            } else {
                Vec::new()
            },
        })
    }

    /// The `stale-baseline` gate of a report with this baseline member.
    pub fn stale_gate(&self, summary: &baseline::Summary) -> fallow_report_protocol::GateOutcome {
        baseline::stale_gate(summary.stale.len(), self.judges_stale(), self.fail_on_stale)
    }

    /// True when the run writes a baseline file. Such a run exits 0.
    pub const fn saves(&self) -> bool {
        self.recorder.is_some()
    }

    /// Writes the baseline file of `--save-baseline`. `kind` is the command.
    pub fn save(&self, kind: &str) -> Result<(), CliError> {
        self.recorder
            .as_ref()
            .map_or(Ok(()), |recorder| recorder.write(kind))
    }

    /// The section of one command with the scope in its JSON and human
    /// output. The `kind` of the JSON report names the check.
    pub fn annotate(&self, mut section: Section) -> Result<Section, CliError> {
        self.apply_rules(&mut section);
        if let Some(meta) = self.meta(&section.findings)? {
            insert(&mut section.json, "_meta", meta);
        }
        self.attach_suppressions(&mut section);
        let check = section.json["kind"].as_str().unwrap_or_default().to_owned();
        let check = check.as_str();
        let mut lines = String::new();
        if let Some(changed) = &self.changed {
            insert(&mut section.json, "changed_since", to_value(changed)?);
            lines.push_str(&changed_line(changed));
        }
        if let Some(packages) = &self.packages {
            insert(&mut section.json, "packages", to_value(&packages.names)?);
            lines.push_str(&packages_line(packages));
        }
        if let Some(summary) = self.baseline_summary(&[check]) {
            let gate = self.stale_gate(&summary);
            if let Some(map) = section
                .json
                .get_mut("gate_outcomes")
                .and_then(Value::as_object_mut)
            {
                map.insert(
                    RustGateName::StaleBaseline.as_str().to_owned(),
                    to_value(&gate)?,
                );
            }
            lines.push_str(&baseline_line(&summary));
            if gate.fails_run() {
                lines.push_str("The `stale-baseline` gate fails: remove the stale entries with `--save-baseline`.\n");
            }
            section.human = format!(
                "{}{}\n",
                section.human.trim_end(),
                render::gate_block_line(RustGateName::StaleBaseline, &gate)
            );
            if let Some(note) = render::gate_notification(RustGateName::StaleBaseline, &gate) {
                add_notification(&mut section.sarif, note);
            }
            section.gates.insert(RustGateName::StaleBaseline, gate);
            insert(&mut section.json, "baseline", to_value(&summary)?);
        }
        section.human.insert_str(0, &lines);
        if self.saves() {
            self.save(check)?;
            for gate in section.gates.values_mut() {
                gate.enforced = false;
            }
            unenforce(&mut section.json);
        }
        Ok(section)
    }
}

/// The folders of the packages that the check did not judge: `skipped` of
/// `deps`, `summary.skipped_packages` of `files`. A comment there is not stale.
fn unjudged_folders(json: &Value, folders: &BTreeMap<String, String>) -> Vec<String> {
    let lists = [&json["skipped"], &json["summary"]["skipped_packages"]];
    lists
        .into_iter()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|entry| entry["package"].as_str())
        .filter_map(|package| folders.get(package))
        // The root package has the folder `""`, which holds each path.
        .cloned()
        .collect()
}

/// The names of the boundary rules in the configuration. A suppression
/// comment can name one.
fn boundary_rules(workspace: &Workspace) -> Vec<String> {
    let rules = workspace
        .metadata
        .get("fallow")
        .and_then(|root| root.get("boundaries"));
    let Some(rules) = rules else {
        return Vec::new();
    };
    ["crates", "sources", "modules"]
        .iter()
        .filter_map(|key| rules.get(key).and_then(Value::as_array))
        .flatten()
        .filter_map(|rule| rule.get("rule").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

/// The files whose suppression comments a run judges: the `.rs` files in the
/// target folders of each member, and each manifest.
fn marked_files(workspace: &Workspace) -> Vec<String> {
    let mut files: Vec<String> =
        crate::files::target_files(workspace.root.as_std_path(), workspace)
            .into_keys()
            .collect();
    files.push("Cargo.toml".to_owned());
    files.extend(
        workspace
            .members
            .iter()
            .map(|member| member.manifest_path.clone()),
    );
    files.sort();
    files.dedup();
    files
}

/// Sets `enforced` to false in each gate of a JSON report. A run that saves a
/// baseline exits 0.
pub fn unenforce(json: &mut Value) {
    if let Some(map) = json.get_mut("gate_outcomes").and_then(Value::as_object_mut) {
        for gate in map.values_mut() {
            if let Some(gate) = gate.as_object_mut() {
                gate.insert("enforced".to_owned(), Value::Bool(false));
            }
        }
    }
}

/// Adds a gate notification to the first run of a SARIF log.
fn add_notification(sarif: &mut Value, note: Value) {
    let Some(invocation) = sarif
        .pointer_mut("/runs/0/invocations/0")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    match invocation.get_mut("toolExecutionNotifications") {
        Some(Value::Array(notes)) => notes.push(note),
        _ => {
            invocation.insert(
                "toolExecutionNotifications".to_owned(),
                Value::Array(vec![note]),
            );
        }
    }
}

/// Adds a member to a JSON object. A value that is not an object stays.
fn insert(json: &mut Value, key: &str, value: Value) {
    if let Value::Object(map) = json {
        map.insert(key.to_owned(), value);
    }
}
