//! The `api` command: the public API changes of the published libraries
//! between the merge base of a ref and HEAD.
//!
//! The command reads the full tree of the merge base, runs `cargo metadata
//! --no-deps` on it, and builds the module graph of both versions. A
//! published library is a library whose `publish` is not `false`. The
//! command compares the keys of [`surface`]: each public path of each item.
//!
//! - A key that only HEAD has is `added`. A key that only the base has is
//!   `removed`. A key whose headers differ is `changed`. A header holds the
//!   signature, the `cfg` and `derive` attributes, and no body.
//! - A library that only HEAD publishes is one `crate-added` finding. A
//!   library that only the base publishes is one `crate-removed` finding. A
//!   feature that only the base has is `feature-removed`.
//! - The `api-removals` gate counts the removals of a library when the
//!   breaking part of its version did not change: the major version from
//!   1.0, else the minor version.
//!
//! The command does not judge a type change behind a re-export, a trait
//! implementation, an auto trait, the meaning of a bound or
//! `#[non_exhaustive]`.

mod headers;
mod surface;
mod tree;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use clap::Args;
use serde::Serialize;

use crate::api::surface::{CrateApi, Entry, Surface};
use crate::error::CliError;
use crate::finding::{self as shared, Category, Draft, Impact, Location, Severity};
use crate::gates::RustGateName as Gate;
use crate::render::Progress;
use crate::scope::Scope;
use crate::section::{Section, to_value};
use crate::structure::CommonArgs;
use crate::structure::report::{Action, Report, count_gate, gates_text, plural, shared_actions};
use crate::structure::sarif;
use crate::workspace;

/// The rule of a removal: `removed`, `crate-removed` or `feature-removed`.
pub const REMOVAL_RULE: &str = "api-removal";
/// The rule of a `changed` finding.
pub const CHANGE_RULE: &str = "api-change";
/// The rule of an `added` or a `crate-added` finding.
pub const ADDITION_RULE: &str = "api-addition";

/// The flags of `api`.
#[derive(Debug, Clone, Args)]
pub struct ApiArgs {
    #[command(flatten)]
    pub(crate) common: CommonArgs,
    /// The git ref to compare with. The command compares HEAD with the merge
    /// base of HEAD and the ref, and reads the full tree of the merge base. A
    /// shallow clone needs the history of the merge base: in GitHub Actions,
    /// set `fetch-depth: 0` in actions/checkout.
    #[arg(long, value_name = "REF")]
    base: String,
    /// Exit 1 when a published library removed an item, a crate or a feature
    /// with no breaking version bump.
    #[arg(long)]
    fail_on_issues: bool,
}

/// The kind of one change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(rename = "ApiChange")]
pub enum Change {
    /// HEAD has a public path that the base does not have.
    Added,
    /// The base has a public path that HEAD does not have.
    Removed,
    /// The header of an item at the path changed.
    Changed,
    /// HEAD publishes a library that the base does not publish.
    CrateAdded,
    /// The base publishes a library that HEAD does not publish.
    CrateRemoved,
    /// The base has a feature that HEAD does not have.
    FeatureRemoved,
}

impl Change {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Changed => "changed",
            Self::CrateAdded => "crate-added",
            Self::CrateRemoved => "crate-removed",
            Self::FeatureRemoved => "feature-removed",
        }
    }

    const fn is_removal(self) -> bool {
        matches!(
            self,
            Self::Removed | Self::CrateRemoved | Self::FeatureRemoved
        )
    }

    const fn is_addition(self) -> bool {
        matches!(self, Self::Added | Self::CrateAdded)
    }

    const fn rule(self) -> &'static str {
        if self.is_removal() {
            REMOVAL_RULE
        } else if self.is_addition() {
            ADDITION_RULE
        } else {
            CHANGE_RULE
        }
    }
}

/// One API change.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[schemars(rename = "ApiFinding")]
pub struct Finding {
    /// The package name.
    #[serde(rename = "crate")]
    pub krate: String,
    /// The public path, or the feature name for `feature-removed`, or the
    /// package name for a crate change.
    pub path: String,
    /// The kind of the item, or `null` for a crate or a feature change.
    pub item_kind: Option<&'static str>,
    /// The change.
    pub change: Change,
    /// The header at the base, or `null`. More than one header, for example
    /// one for each `cfg` variant, are joined with ` | `.
    pub before: Option<String>,
    /// The header at HEAD, or `null`.
    pub after: Option<String>,
    /// The file at HEAD. For a removal, the file of the nearest item around
    /// the path at HEAD, else the package manifest.
    pub file: String,
    /// The line in `file`.
    pub line: u32,
    /// The package version at the base, or `null`.
    pub version_base: Option<String>,
    /// The package version at HEAD, or `null`.
    pub version_head: Option<String>,
    /// True when the finding counts for the `api-removals` gate.
    pub breaking: bool,
    /// The next steps.
    pub actions: Vec<Action>,
}

/// The summary of an `api` run.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[schemars(rename = "ApiSummary")]
pub struct Summary {
    /// The ref of `--base`.
    pub base: String,
    /// The merge base of HEAD and the ref.
    pub merge_base: String,
    /// The published libraries at the base or at HEAD.
    pub crates: usize,
    /// The `added` and `crate-added` findings.
    pub added: usize,
    /// The `removed`, `crate-removed` and `feature-removed` findings.
    pub removed: usize,
    /// The `changed` findings.
    pub changed: usize,
}

/// Runs `api`.
pub fn run(args: &ApiArgs) -> Result<Section, CliError> {
    let progress = Progress::new(args.common.quiet);
    progress.step("Reading cargo metadata");
    let cargo_flags = args.common.cargo.flags();
    refuse_baseline(&args.common.scope)?;
    let head = workspace::load(args.common.manifest_path.as_deref(), &cargo_flags)?;
    let root = head.root.as_std_path();
    let merge_base = crate::structure::git::merge_base(root, &args.base)?;
    let scope = Scope::load(&head, &args.common.scope)?;
    progress.step("Reading the tree of the merge base");
    let tree = tree::checkout(root, &merge_base)?;
    let base = workspace::load(Some(&tree.root.join("Cargo.toml")), &cargo_flags)?;
    progress.step("Reading the API of the published libraries");
    let before = surface::surface(&base);
    let after = surface::surface(&head);
    // `-p` keeps the findings of the packages that it names: a finding
    // belongs to its package, also when the item comes from another crate.
    let manifests: BTreeMap<&String, &String> = before
        .crates
        .iter()
        .chain(after.crates.iter())
        .map(|(name, api)| (name, &api.manifest))
        .collect();
    let kept: BTreeSet<&String> = manifests
        .iter()
        .filter(|(_, manifest)| scope.keeps(manifest))
        .map(|(name, _)| *name)
        .collect();
    let rules = scope.rules();
    let mut findings = compare(&before, &after);
    findings
        .retain(|finding| kept.contains(&finding.krate) && !rules.is_off(&[finding.change.rule()]));
    let summary = Summary {
        base: args.base.clone(),
        merge_base,
        crates: kept.len(),
        added: findings.iter().filter(|f| f.change.is_addition()).count(),
        removed: findings.iter().filter(|f| f.change.is_removal()).count(),
        changed: findings
            .iter()
            .filter(|f| f.change == Change::Changed)
            .count(),
    };
    scope.annotate(section(summary, findings, args.fail_on_issues, &scope)?)
}

/// Stops a run with a flag that judges one tree: `--baseline`,
/// `--save-baseline` or `--changed-since`. An API change is a change between
/// two trees, and `--base` names the other tree.
fn refuse_baseline(scope: &crate::scope::ScopeArgs) -> Result<(), CliError> {
    if scope.baseline.is_none() && scope.save_baseline.is_none() && scope.changed_since.is_none() {
        return Ok(());
    }
    Err(CliError::new(
        crate::error::ErrorCode::InvalidArgument,
        "`api` does not take `--baseline`, `--save-baseline` or `--changed-since`",
    )
    .with_help("`--base` names the tree to compare with. To accept a removal, bump the version or keep the item."))
}

fn section(
    summary: Summary,
    findings: Vec<Finding>,
    enforced: bool,
    scope: &Scope,
) -> Result<Section, CliError> {
    let mut report = Report::new("api", summary, findings);
    let counted = report
        .findings
        .iter()
        .filter(|finding| finding.breaking && scope.rules().counts(&[REMOVAL_RULE], true))
        .count();
    report
        .gate_outcomes
        .insert(Gate::ApiRemovals, count_gate(counted, enforced));
    let findings = shared_findings(&report);
    Ok(Section {
        human: human(&report),
        json: to_value(&report)?,
        sarif: sarif_log(&findings, &report),
        findings,
        gates: report.gate_outcomes,
    })
}

/// The findings between two surfaces, sorted by crate, change and path.
fn compare(before: &Surface, after: &Surface) -> Vec<Finding> {
    let mut found = Vec::new();
    let names: BTreeSet<&String> = before.crates.keys().chain(after.crates.keys()).collect();
    for name in names {
        let versions = Versions {
            base: before.versions.get(name).cloned(),
            head: after.versions.get(name).cloned(),
        };
        match (before.crates.get(name), after.crates.get(name)) {
            (Some(old), Some(new)) => compare_crate(name, old, new, &versions, &mut found),
            (None, Some(new)) => {
                found.push(crate_finding(name, Change::CrateAdded, new, &versions));
            }
            (Some(old), None) => {
                found.push(crate_finding(name, Change::CrateRemoved, old, &versions));
            }
            (None, None) => {}
        }
    }
    found.sort_by(|a, b| (&a.krate, a.change, &a.path).cmp(&(&b.krate, b.change, &b.path)));
    found
}

/// The versions of one package.
struct Versions {
    base: Option<String>,
    head: Option<String>,
}

impl Versions {
    /// True when the breaking part of the version did not change: the major
    /// version from 1.0, else the minor version. A package that HEAD does not
    /// have cannot have a bump.
    fn same_breaking_part(&self) -> bool {
        match (&self.base, &self.head) {
            (Some(base), Some(head)) => breaking_part(base) == breaking_part(head),
            _ => true,
        }
    }
}

/// The part of a version that a breaking change must bump, as Cargo reads
/// it: the major version from 1.0, the minor version from 0.1, else the patch
/// version.
fn breaking_part(version: &str) -> (u64, u64, u64) {
    let mut parts = version
        .split(['.', '-', '+'])
        .map(|part| part.parse::<u64>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    let patch = parts.next().unwrap_or(0);
    match (major, minor) {
        (0, 0) => (0, 0, patch),
        (0, _) => (0, minor, 0),
        _ => (major, 0, 0),
    }
}

fn compare_crate(
    name: &str,
    old: &CrateApi,
    new: &CrateApi,
    versions: &Versions,
    found: &mut Vec<Finding>,
) {
    let breaking = versions.same_breaking_part();
    let keys: BTreeSet<&String> = old.entries.keys().chain(new.entries.keys()).collect();
    for key in keys {
        let finding = match (old.entries.get(key), new.entries.get(key)) {
            // The members of a type alias are the members of its target,
            // which the command does not follow. A member below an alias can
            // still exist, so its removal gives no finding.
            (Some(_), None) if below_alias(key, new) => None,
            (Some(before), None) => {
                let (file, line) = removal_place(key, new);
                Some(Finding {
                    item_kind: before.first().map(|entry| entry.kind),
                    before: Some(headers_of(before)),
                    after: None,
                    file,
                    line,
                    breaking,
                    ..base_finding(name, key, Change::Removed, versions)
                })
            }
            (None, Some(after)) => after.first().map(|entry| Finding {
                item_kind: Some(entry.kind),
                before: None,
                after: Some(headers_of(after)),
                file: entry.file.clone(),
                line: entry.line,
                ..base_finding(name, key, Change::Added, versions)
            }),
            (Some(before), Some(after)) if differs(before, after) => {
                after.first().map(|entry| Finding {
                    item_kind: Some(entry.kind),
                    before: Some(headers_of(before)),
                    after: Some(headers_of(after)),
                    file: entry.file.clone(),
                    line: entry.line,
                    ..base_finding(name, key, Change::Changed, versions)
                })
            }
            _ => None,
        };
        found.extend(finding);
    }
    for feature in old.features.difference(&new.features) {
        found.push(Finding {
            file: new.manifest.clone(),
            breaking,
            ..base_finding(name, feature, Change::FeatureRemoved, versions)
        });
    }
}

/// True when a key around `key` is a type alias at HEAD.
fn below_alias(key: &str, new: &CrateApi) -> bool {
    let mut parent = key;
    while let Some((head, _)) = parent.rsplit_once("::") {
        parent = head;
        let alias = new
            .entries
            .get(parent)
            .is_some_and(|entries| entries.iter().any(|entry| entry.kind == "type_alias"));
        if alias {
            return true;
        }
    }
    false
}

/// True when the kinds or the headers at a key differ, as multisets.
fn differs(before: &[Entry], after: &[Entry]) -> bool {
    shape(before) != shape(after)
}

fn shape(entries: &[Entry]) -> Vec<(&str, &str)> {
    let mut shape: Vec<(&str, &str)> = entries
        .iter()
        .map(|entry| (entry.kind, entry.header.as_str()))
        .collect();
    shape.sort_unstable();
    shape
}

fn headers_of(entries: &[Entry]) -> String {
    let mut headers: Vec<&str> = entries.iter().map(|entry| entry.header.as_str()).collect();
    headers.sort_unstable();
    headers.dedup();
    headers.join(" | ")
}

/// The place of a removed path at HEAD: the nearest item around the path, the
/// root file for a path at the crate root, else the manifest.
fn removal_place(key: &str, new: &CrateApi) -> (String, u32) {
    let mut parent = key;
    while let Some((head, _)) = parent.rsplit_once("::") {
        parent = head;
        if !parent.contains("::") {
            return if new.root_file.is_empty() {
                (new.manifest.clone(), 1)
            } else {
                (new.root_file.clone(), 1)
            };
        }
        if let Some(entry) = new.entries.get(parent).and_then(|entries| entries.first()) {
            return (entry.file.clone(), entry.line);
        }
    }
    (new.manifest.clone(), 1)
}

fn base_finding(name: &str, path: &str, change: Change, versions: &Versions) -> Finding {
    Finding {
        krate: name.to_owned(),
        path: path.to_owned(),
        item_kind: None,
        change,
        before: None,
        after: None,
        file: String::new(),
        line: 1,
        version_base: versions.base.clone(),
        version_head: versions.head.clone(),
        breaking: false,
        actions: actions(change, path),
    }
}

fn crate_finding(name: &str, change: Change, api: &CrateApi, versions: &Versions) -> Finding {
    Finding {
        file: api.manifest.clone(),
        breaking: change == Change::CrateRemoved && versions.same_breaking_part(),
        ..base_finding(name, name, change, versions)
    }
}

fn actions(change: Change, path: &str) -> Vec<Action> {
    let step = match change {
        Change::Removed | Change::CrateRemoved | Change::FeatureRemoved => Action::manual(
            "bump-breaking-version",
            format!("Keep `{path}`, or bump the breaking part of the version of the package"),
        ),
        Change::Changed => Action::manual(
            "review-api-change",
            format!("Check that the change of `{path}` keeps the callers valid"),
        ),
        Change::Added | Change::CrateAdded => Action::manual(
            "review-api-addition",
            format!("Check that `{path}` is meant to be public API"),
        ),
    };
    vec![step]
}

/// The findings in the shared shape. An addition has none: SARIF, the
/// annotations and CodeClimate show only removals and changes.
fn shared_findings(report: &Report<Summary, Finding>) -> Vec<shared::Finding> {
    report
        .findings
        .iter()
        .filter(|finding| !finding.change.is_addition())
        .map(|finding| {
            let message = match (&finding.before, &finding.after) {
                (Some(before), Some(after)) => format!(
                    "`{}` of `{}` changed from `{before}` to `{after}`.",
                    finding.path, finding.krate
                ),
                _ => match finding.change {
                    Change::CrateRemoved => {
                        format!("`{}` is no longer a published library.", finding.krate)
                    }
                    Change::FeatureRemoved => {
                        format!(
                            "The feature `{}` of `{}` is gone.",
                            finding.path, finding.krate
                        )
                    }
                    _ => format!(
                        "The public path `{}` of `{}` is gone.",
                        finding.path, finding.krate
                    ),
                },
            };
            shared::Finding::new(Draft {
                check: "api",
                rule: finding.change.rule(),
                severity: if finding.breaking {
                    Severity::Error
                } else {
                    Severity::Warning
                },
                message,
                location: Location::at(finding.file.clone(), finding.line, 1),
                identity: &[&finding.krate, finding.change.as_str(), &finding.path],
                category: Category::BugRisk,
                impact: if finding.breaking {
                    Impact::Major
                } else {
                    Impact::Minor
                },
            })
            .with_actions(shared_actions(&finding.actions))
        })
        .collect()
}

fn human(report: &Report<Summary, Finding>) -> String {
    let summary = &report.summary;
    let short: String = summary.merge_base.chars().take(12).collect();
    let mut text = format!(
        "cargo fallow api: {} {} against {} (merge base {short}): {} added, {} removed, {} changed\n",
        summary.crates,
        plural(summary.crates, "published library", "published libraries"),
        summary.base,
        summary.added,
        summary.removed,
        summary.changed,
    );
    if report.findings.is_empty() {
        text.push_str("No API change\n");
    }
    let mut by_crate: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
    for finding in report.findings.iter().filter(|f| !f.change.is_addition()) {
        by_crate.entry(&finding.krate).or_default().push(finding);
    }
    for (name, findings) in by_crate {
        let _ = writeln!(text, "  {name}:");
        for finding in findings {
            let detail = match (&finding.before, &finding.after) {
                (Some(before), Some(after)) => format!(": {before} -> {after}"),
                (Some(before), None) => format!(": {before}"),
                _ => String::new(),
            };
            let _ = writeln!(
                text,
                "    {} {}{detail} ({}:{})",
                finding.change.as_str(),
                finding.path,
                finding.file,
                finding.line
            );
        }
    }
    text.push_str(&gates_text(&report.gate_outcomes));
    text
}

fn sarif_log(findings: &[shared::Finding], report: &Report<Summary, Finding>) -> serde_json::Value {
    let rules = BTreeMap::from([
        (
            REMOVAL_RULE.to_owned(),
            "A published library removed a public path, the crate or a feature.".to_owned(),
        ),
        (
            CHANGE_RULE.to_owned(),
            "The header of a public item of a published library changed.".to_owned(),
        ),
    ]);
    sarif::log(&rules, findings, &report.gate_outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: &'static str, header: &str) -> Vec<Entry> {
        vec![Entry {
            kind,
            header: header.to_owned(),
            file: "src/lib.rs".to_owned(),
            line: 1,
        }]
    }

    fn surface(entries: &[(&str, Vec<Entry>)]) -> Surface {
        let api = CrateApi {
            manifest: "Cargo.toml".to_owned(),
            root_file: "src/lib.rs".to_owned(),
            entries: entries
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect(),
            ..CrateApi::default()
        };
        Surface {
            crates: BTreeMap::from([("c".to_owned(), api)]),
            versions: BTreeMap::from([("c".to_owned(), "1.0.0".to_owned())]),
        }
    }

    #[test]
    fn a_member_below_a_new_type_alias_is_not_removed() {
        let before = surface(&[
            ("c::T", entry("struct", "pub struct T")),
            ("c::T::new", entry("method", "pub fn new ( ) -> Self")),
        ]);
        let after = surface(&[("c::T", entry("type_alias", "pub type T = other :: T"))]);
        let changes: Vec<(Change, String)> = compare(&before, &after)
            .into_iter()
            .map(|finding| (finding.change, finding.path))
            .collect();
        assert_eq!(changes, [(Change::Changed, "c::T".to_owned())]);
    }

    #[test]
    fn the_breaking_part_is_the_major_from_1_and_else_the_minor() {
        assert_eq!(breaking_part("1.4.2"), breaking_part("1.0.0"));
        assert_ne!(breaking_part("2.0.0"), breaking_part("1.9.9"));
        assert_eq!(breaking_part("0.3.1"), breaking_part("0.3.9-rc.1"));
        assert_ne!(breaking_part("0.4.0"), breaking_part("0.3.9"));
        assert_ne!(breaking_part("0.0.2"), breaking_part("0.0.1"));
    }
}
