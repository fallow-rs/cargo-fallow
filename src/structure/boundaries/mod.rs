//! Crate, source and module boundary rules.
//!
//! The rules are in `[workspace.metadata.fallow.boundaries]`:
//!
//! - A crate rule names workspace packages that must not depend on other
//!   workspace packages. The edges come from `cargo metadata`: each section of
//!   `Cargo.toml` counts, also `[dev-dependencies]`, unless `kinds` limits it.
//! - A source rule names code patterns that must not appear in some folders,
//!   for example process-tree calls outside the package that owns them. A
//!   pattern matches a token sequence, so whitespace and comments do not count,
//!   and a pattern in a string or a comment does not match. With
//!   `match = "text"`, a pattern matches the plain text instead. `scope` and
//!   `allow` are in [`sources`].
//! - A module rule names modules of one package that must not use other
//!   modules of that package. It reads each `use` item and each path in the
//!   code that starts with `crate`, `self` or `super`.
//! - An item rule and a glob re-export rule work on the module graph. See
//!   [`items`].

mod cfg;
mod items;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use ra_ap_parser::LexedStr;
use ra_ap_syntax::ast::{self, AstNode, HasName};
use ra_ap_syntax::{SyntaxKind, SyntaxNode};
use serde::Serialize;

use crate::error::{CliError, ErrorCode};
use crate::finding::{self as shared, Category, Draft, Impact, Location, Severity};
use crate::gates::RustGateName as Gate;
use crate::gates::parse_edition;
use crate::modules::{ModuleDecl, TargetRoot, Walk, walk};
use crate::parse::{Edition, with_parsed};
use crate::render::{Progress, warn};
use crate::rules::Level;
use crate::scope::Scope;
use crate::section::{Section, to_value};
use crate::structure::BoundariesArgs;
mod sources;

use crate::structure::config::{
    self, CrateRule, DependencyKind, ModuleRule, SourceRule, SourceScope, matches_prefix,
};
use crate::structure::report::{Action, Report, count_gate, gates_text, plural, shared_actions};
use crate::structure::sarif;
use crate::structure::source::is_space_or_comment;
use crate::workspace::{self, LinkKind, Member, Workspace};

/// The `deny` value that names each workspace package.
const ANY: &str = "*";

/// Replaces each `{name}` in a message template with its value.
pub(super) fn fill(template: &str, values: &[(&str, &str)]) -> String {
    values
        .iter()
        .fold(template.to_owned(), |text, (name, value)| {
            text.replace(&format!("{{{name}}}"), value)
        })
}

/// The kind of a boundary finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(rename = "BoundariesKind")]
pub enum Kind {
    /// A workspace package depends on a denied workspace package.
    CrateDependency,
    /// A file contains a denied pattern.
    SourcePattern,
    /// A module uses a denied module.
    ModulePath,
    /// Code uses a denied workspace item, with the path resolved.
    ItemPath,
    /// A `pub use <path>::*` re-exports a denied crate or module.
    GlobReexport,
    /// Code that a `cfg` build compiles uses an item that the build does not
    /// compile.
    CfgPath,
    /// An `allow` entry of a source rule does not fit its file: the file has
    /// fewer matches than `max`, or it lacks a `require` pattern.
    AllowEntry,
}

impl Kind {
    /// Each kind.
    #[cfg(test)]
    pub const ALL: [Self; 7] = [
        Self::CrateDependency,
        Self::SourcePattern,
        Self::ModulePath,
        Self::ItemPath,
        Self::GlobReexport,
        Self::CfgPath,
        Self::AllowEntry,
    ];

    /// The kind in the report.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CrateDependency => "crate-dependency",
            Self::SourcePattern => "source-pattern",
            Self::ModulePath => "module-path",
            Self::ItemPath => "item-path",
            Self::GlobReexport => "glob-reexport",
            Self::CfgPath => "cfg-path",
            Self::AllowEntry => "allow-entry",
        }
    }
}

/// Where the path of an item rule finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(rename = "BoundariesOrigin")]
pub enum Origin {
    /// In code.
    Code,
    /// In the tokens of a macro. The line is the line of the macro.
    Macro,
}

/// One violation of a rule.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[schemars(rename = "BoundariesFinding")]
pub struct Finding {
    /// The rule name.
    pub rule: String,
    /// The kind of the rule.
    pub kind: Kind,
    /// The package or the module that the rule checks.
    pub from: String,
    /// The denied package, pattern owner or module path.
    pub to: String,
    /// The file of the violation.
    pub path: String,
    /// The line. It starts at 1.
    pub line: u32,
    /// The column. It starts at 1.
    pub col: u32,
    /// The denied pattern, for a source rule.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// The `Cargo.toml` sections of the dependency, for a crate rule.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dependency_kinds: Vec<&'static str>,
    /// Where the path of an item rule is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// The message.
    pub message: String,
    /// The next steps.
    pub actions: Vec<Action>,
}

/// The summary of a boundaries run.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[schemars(rename = "BoundariesSummary")]
pub struct Summary {
    /// The crate rules.
    pub crate_rules: usize,
    /// The source rules.
    pub source_rules: usize,
    /// The module rules.
    pub module_rules: usize,
    /// The item rules.
    pub item_rules: usize,
    /// The glob re-export rules.
    pub reexport_rules: usize,
    /// The `cfg` rules.
    pub cfg_rules: usize,
    /// The uses that the item rules cannot resolve and that name a denied
    /// item: a path that does not resolve to its end, a method call or a
    /// field access. They give no finding.
    pub unresolved: usize,
    /// The dependency edges between workspace packages.
    pub workspace_edges: usize,
    /// The files that the source rules read.
    pub source_files: usize,
}

pub fn run(args: &BoundariesArgs) -> Result<Section, CliError> {
    let progress = Progress::new(args.common.quiet);
    progress.step("Reading cargo metadata");
    let workspace = workspace::load(
        args.common.manifest_path.as_deref(),
        &args.common.cargo.flags(),
    )?;
    let scope = Scope::load(&workspace, &args.common.scope)?;
    scope.annotate(section(
        &workspace,
        args.fail_on_violations,
        &scope,
        progress,
    )?)
}

/// Checks the rules of the workspace. `enforced` is true when a violation
/// fails the run. The report keeps only the findings in `scope`.
pub fn section(
    workspace: &Workspace,
    enforced: bool,
    scope: &Scope,
    progress: Progress,
) -> Result<Section, CliError> {
    let settings: config::Boundaries = config::section(workspace, "boundaries")?;
    let resolver = Names::new(workspace, &settings.groups)?;
    progress.step("Checking the boundary rules");
    let edges = edges(workspace);
    let mut findings = Vec::new();
    for rule in &settings.crates {
        findings.extend(crate_rule(workspace, &edges, rule, &resolver)?);
    }
    let mut source_files = 0;
    let mut tests = None;
    for rule in &settings.sources {
        let (found, read) = source_rule(workspace, rule, &mut tests)?;
        findings.extend(found);
        source_files += read;
    }
    for rule in &settings.modules {
        findings.extend(module_rule(workspace, rule)?);
    }
    let resolved = items::check(workspace, &settings.items, &settings.reexports, &resolver)?;
    findings.extend(resolved.findings);
    findings.extend(cfg::findings(workspace, &settings.cfg)?);
    findings.retain(|finding| scope.keeps(&finding.path));
    findings.sort_by(|a, b| {
        (&a.rule, &a.path, a.line, a.col, &a.to, &a.pattern)
            .cmp(&(&b.rule, &b.path, b.line, b.col, &b.to, &b.pattern))
    });
    let summary = Summary {
        crate_rules: settings.crates.len(),
        source_rules: settings.sources.len(),
        module_rules: settings.modules.len(),
        item_rules: settings.items.len(),
        reexport_rules: settings.reexports.len(),
        cfg_rules: settings.cfg.len(),
        unresolved: resolved.unresolved,
        workspace_edges: edges.len(),
        source_files,
    };
    let mut report = Report::new("boundaries", summary, findings);
    let mut findings = shared_findings(&report);
    let rules = scope.rules();
    // A rule name, a kind or `boundaries` can turn a boundary finding off.
    // The filter goes before the baseline, so a saved baseline does not hold
    // the finding.
    Scope::retain_where(&mut report.findings, &mut findings, |finding| {
        !rules.is_off(&level_names(finding))
    });
    report.retain_baseline(scope, &mut findings);
    for (finding, shared) in report.findings.iter().zip(findings.iter_mut()) {
        shared.severity = rules.severity(&level_names(finding), shared.severity);
    }
    let counted = report
        .findings
        .iter()
        .filter(|finding| rules.counts(&level_names(finding), true))
        .count();
    report
        .gate_outcomes
        .insert(Gate::BoundaryViolations, count_gate(counted, enforced));
    let mut sarif = sarif_log(&report, &findings);
    // The structure log has one result for each finding, in the same order.
    if let Some(results) = sarif["runs"][0]["results"].as_array_mut() {
        for (finding, result) in report.findings.iter().zip(results) {
            match rules.level(&level_names(finding)) {
                Some(Level::Error) => result["level"] = "error".into(),
                Some(Level::Warn) => result["level"] = "warning".into(),
                _ => {}
            }
        }
    }
    Ok(Section {
        human: human(&report),
        json: to_value(&report)?,
        sarif,
        findings,
        gates: report.gate_outcomes,
    })
}

/// The names of a finding in `[workspace.metadata.fallow.rules]`: the rule
/// name of its table, its kind, and `boundaries` for each boundary rule.
fn level_names(finding: &Finding) -> [&str; 3] {
    [&finding.rule, finding.kind.as_str(), "boundaries"]
}

/// The findings in the shared shape. The id uses the rule, the file, the
/// denied target and the place among the same violations in the file, not the
/// line.
fn shared_findings(report: &Report<Summary, Finding>) -> Vec<shared::Finding> {
    let keys: Vec<(_, u32)> = report
        .findings
        .iter()
        .map(|finding| {
            // The kind keeps the count of an `allow-entry` finding apart, so
            // it does not move the index of a source finding of its file.
            let key = (
                finding.rule.as_str(),
                finding.kind,
                finding.path.as_str(),
                finding.to.as_str(),
                finding.pattern.as_deref().unwrap_or_default(),
            );
            (key, finding.line)
        })
        .collect();
    report
        .findings
        .iter()
        .zip(shared::occurrences(&keys))
        .map(|(finding, index)| {
            let index = index.to_string();
            let pattern = finding.pattern.as_deref().unwrap_or_default();
            let entry = Kind::AllowEntry.as_str();
            let identity: Vec<&str> = if finding.kind == Kind::AllowEntry {
                vec![&finding.path, &finding.to, pattern, &index, entry]
            } else {
                vec![&finding.path, &finding.to, pattern, &index]
            };
            shared::Finding::new(Draft {
                check: "boundaries",
                rule: &finding.rule,
                severity: Severity::Error,
                message: finding.message.clone(),
                location: Location::at(finding.path.clone(), finding.line, finding.col),
                identity: &identity,
                category: Category::BugRisk,
                impact: Impact::Major,
            })
            .with_actions(shared_actions(&finding.actions))
        })
        .collect()
}

/// The names of packages and groups.
pub(super) struct Names<'a> {
    members: BTreeSet<&'a str>,
    groups: &'a BTreeMap<String, Vec<String>>,
}

impl<'a> Names<'a> {
    fn new(
        workspace: &'a Workspace,
        groups: &'a BTreeMap<String, Vec<String>>,
    ) -> Result<Self, CliError> {
        let members: BTreeSet<&str> = workspace
            .members
            .iter()
            .map(|member| member.name.as_str())
            .collect();
        for (group, names) in groups {
            for name in names {
                if name.starts_with('@') || name == ANY {
                    return Err(invalid(&format!(
                        "group `{group}` names `{name}`; a group lists package names only"
                    )));
                }
            }
        }
        Ok(Self { members, groups })
    }

    /// The package names that `values` names. An unknown package name gives a
    /// warning, because a rule can name a package that a later change adds.
    pub(super) fn expand(
        &self,
        rule: &str,
        values: &[String],
    ) -> Result<BTreeSet<String>, CliError> {
        let mut names = BTreeSet::new();
        for value in values {
            if value == ANY {
                names.extend(self.members.iter().map(|&name| name.to_owned()));
            } else if let Some(group) = value.strip_prefix('@') {
                let Some(list) = self.groups.get(group) else {
                    return Err(invalid(&format!(
                        "rule `{rule}` names the unknown group `@{group}`"
                    )));
                };
                names.extend(list.iter().cloned());
            } else {
                if !self.members.contains(value.as_str()) {
                    warn(&format!(
                        "rule `{rule}` names `{value}`, which is not a workspace package"
                    ));
                }
                names.insert(value.clone());
            }
        }
        Ok(names)
    }
}

pub(super) fn invalid(message: &str) -> CliError {
    CliError::new(
        ErrorCode::InvalidConfig,
        format!("`[workspace.metadata.fallow.boundaries]` is not valid: {message}"),
    )
    .with_help("Correct the table in the root Cargo.toml.")
}

/// One dependency of a workspace package on a workspace package.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Edge {
    from: String,
    to: String,
    /// The name in `Cargo.toml`: the rename, or the package name.
    key: String,
    kind: DependencyKind,
}

fn edges(workspace: &Workspace) -> Vec<Edge> {
    let members: BTreeSet<&str> = workspace
        .members
        .iter()
        .map(|member| member.name.as_str())
        .collect();
    let mut found: Vec<Edge> = workspace
        .members
        .iter()
        .flat_map(|member| {
            member
                .links
                .iter()
                .filter(|link| members.contains(link.package.as_str()))
                .map(|link| Edge {
                    from: member.name.clone(),
                    to: link.package.clone(),
                    key: link.rename.clone().unwrap_or_else(|| link.package.clone()),
                    kind: match link.kind {
                        LinkKind::Normal => DependencyKind::Normal,
                        LinkKind::Dev => DependencyKind::Dev,
                        LinkKind::Build => DependencyKind::Build,
                    },
                })
        })
        .collect();
    found.sort();
    found.dedup();
    found
}

const fn kind_name(kind: DependencyKind) -> &'static str {
    match kind {
        DependencyKind::Normal => "normal",
        DependencyKind::Dev => "dev",
        DependencyKind::Build => "build",
    }
}

fn crate_rule(
    workspace: &Workspace,
    edges: &[Edge],
    rule: &CrateRule,
    names: &Names<'_>,
) -> Result<Vec<Finding>, CliError> {
    let from = names.expand(&rule.rule, &rule.from)?;
    let deny = names.expand(&rule.rule, &rule.deny)?;
    let allow = names.expand(&rule.rule, &rule.allow)?;
    let mut grouped: BTreeMap<(&str, &str), Vec<&Edge>> = BTreeMap::new();
    for edge in edges {
        let kind_ok = rule
            .kinds
            .as_ref()
            .is_none_or(|kinds| kinds.contains(&edge.kind));
        if kind_ok
            && from.contains(&edge.from)
            && deny.contains(&edge.to)
            && !allow.contains(&edge.to)
        {
            grouped
                .entry((&edge.from, &edge.to))
                .or_default()
                .push(edge);
        }
    }
    Ok(grouped
        .into_iter()
        .map(|((from, to), found)| {
            let member = workspace.members.iter().find(|member| member.name == from);
            let manifest = member.map_or_else(String::new, |member| member.manifest_path.clone());
            let line = member.map_or(1, |member| dependency_line(workspace, member, &found[0].key));
            let message = rule.message.as_deref().map_or_else(
                || format!("{from} must not depend on workspace crate {to}"),
                |template| fill(template, &[("from", from), ("to", to)]),
            );
            Finding {
                rule: rule.rule.clone(),
                kind: Kind::CrateDependency,
                from: from.to_owned(),
                to: to.to_owned(),
                path: manifest,
                line,
                col: 1,
                pattern: None,
                dependency_kinds: found.iter().map(|edge| kind_name(edge.kind)).collect(),
                origin: None,
                message,
                actions: vec![Action::manual(
                    "remove-dependency",
                    format!(
                        "Remove the dependency of {from} on {to}, or move the shared code to a crate that the rule allows"
                    ),
                )],
            }
        })
        .collect())
}

/// The line of the dependency `key` in the manifest of `member`, or 1.
fn dependency_line(workspace: &Workspace, member: &Member, key: &str) -> u32 {
    let path = workspace::absolute_path(&workspace.root, &member.manifest_path);
    let Ok(text) = fs::read_to_string(path) else {
        return 1;
    };
    text.lines()
        .position(|line| declares_dependency(line, key))
        .and_then(|index| u32::try_from(index + 1).ok())
        .unwrap_or(1)
}

/// True when a manifest line declares `key`: `key = ...`, `key.workspace = ...`
/// or a `[...dependencies.key]` table.
fn declares_dependency(line: &str, key: &str) -> bool {
    let line = line.trim();
    if let Some(table) = line
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        return table.rsplit_once('.').is_some_and(|(section, name)| {
            section.ends_with("dependencies") && name.trim_matches('"') == key
        });
    }
    let name = line
        .split(['=', '.'])
        .next()
        .unwrap_or_default()
        .trim()
        .trim_matches('"');
    name == key && line.len() > name.len()
}

/// Runs one source rule. `tests` holds the test files of the workspace after
/// the first rule with `scope = "production"`.
fn source_rule(
    workspace: &Workspace,
    rule: &SourceRule,
    tests: &mut Option<BTreeSet<String>>,
) -> Result<(Vec<Finding>, usize), CliError> {
    let patterns: Vec<(String, Vec<String>)> = rule
        .deny
        .iter()
        .map(|pattern| (pattern.clone(), lex(pattern)))
        .collect();
    if let Some((pattern, _)) = patterns.iter().find(|(_, tokens)| tokens.is_empty()) {
        return Err(invalid(&format!(
            "rule `{}` has the empty pattern `{pattern}`",
            rule.rule
        )));
    }
    let root = workspace.root.as_std_path();
    let mut files = BTreeSet::new();
    for path in &rule.paths {
        let trimmed = path.trim_end_matches('/');
        let absolute = root.join(trimmed);
        if !absolute.exists() {
            return Err(invalid(&format!(
                "rule `{}` names `{path}`, which does not exist",
                rule.rule
            )));
        }
        rust_files(root, &absolute, &mut files);
    }
    files.retain(|path: &String| !matches_prefix(path, &rule.exclude));
    if rule.scope == SourceScope::Production {
        let tests = tests.get_or_insert_with(|| sources::test_files(workspace));
        files.retain(|path| !tests.contains(path));
    }
    let allowed = sources::allowed(rule, &files)?;
    let mut found = Vec::new();
    let mut judged = 0;
    for path in &files {
        let Some(code) = sources::read(root, path, rule) else {
            continue;
        };
        judged += 1;
        let allow = allowed.get(path.as_str()).copied();
        found.extend(sources::judge(
            workspace, rule, path, &code, &patterns, allow,
        ));
    }
    Ok((found, judged))
}

fn source_finding(
    workspace: &Workspace,
    rule: &SourceRule,
    path: &str,
    pattern: &str,
    line: u32,
    col: u32,
) -> Finding {
    let owner = rule.owner.clone().unwrap_or_default();
    let message = rule.message.as_deref().map_or_else(
        || {
            if owner.is_empty() {
                format!("{path} contains the denied pattern {pattern}")
            } else {
                format!("{path} contains {pattern}, which belongs to {owner}")
            }
        },
        |template| {
            fill(
                template,
                &[("path", path), ("pattern", pattern), ("owner", &owner)],
            )
        },
    );
    let move_to = if owner.is_empty() {
        "the package that owns it".to_owned()
    } else {
        owner.clone()
    };
    Finding {
        rule: rule.rule.clone(),
        kind: Kind::SourcePattern,
        from: package_of(workspace, path),
        to: owner,
        path: path.to_owned(),
        line,
        col,
        pattern: Some(pattern.to_owned()),
        dependency_kinds: Vec::new(),
        origin: None,
        message,
        actions: vec![Action::manual(
            "move-code",
            format!("Move the code with `{pattern}` to {move_to}, and call it from here"),
        )],
    }
}

/// The workspace package whose folder holds `path`, the deepest one first.
fn package_of(workspace: &Workspace, path: &str) -> String {
    workspace
        .members
        .iter()
        .filter_map(|member| {
            let dir = Path::new(&member.manifest_path)
                .parent()?
                .to_str()?
                .to_owned();
            (dir.is_empty() || matches_prefix(path, std::slice::from_ref(&dir)))
                .then_some((dir.len(), &member.name))
        })
        .max()
        .map(|(_, name)| name.clone())
        .unwrap_or_default()
}

/// Adds each `.rs` file at or below `path`, relative to `root`, with `/`.
fn rust_files(root: &Path, path: &Path, found: &mut BTreeSet<String>) {
    if path.is_file() {
        if path.extension().is_some_and(|extension| extension == "rs")
            && let Some(relative) = relative(root, path)
        {
            found.insert(relative);
        }
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        // A link can point back to a parent folder, so the walk skips links.
        if !kind.is_symlink() {
            rust_files(root, &entry.path(), found);
        }
    }
}

fn relative(root: &Path, path: &Path) -> Option<String> {
    let rest = path.strip_prefix(root).ok()?;
    let parts: Option<Vec<&str>> = rest
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect();
    Some(parts?.join("/"))
}

/// The tokens of a pattern, without whitespace and comments.
fn lex(text: &str) -> Vec<String> {
    let lexed = LexedStr::new(Edition::CURRENT, text);
    (0..lexed.len())
        .filter(|&index| {
            !is_space_or_comment(lexed.kind(index)) && lexed.kind(index) != SyntaxKind::EOF
        })
        .map(|index| lexed.text(index).to_owned())
        .collect()
}

/// The tokens of a file with their offsets.
struct Lexed {
    tokens: Vec<(String, usize)>,
}

impl Lexed {
    fn new(text: &str) -> Self {
        let lexed = LexedStr::new(Edition::CURRENT, text);
        let tokens = (0..lexed.len())
            .filter(|&index| {
                !is_space_or_comment(lexed.kind(index)) && lexed.kind(index) != SyntaxKind::EOF
            })
            .map(|index| (lexed.text(index).to_owned(), lexed.text_start(index)))
            .collect();
        Self { tokens }
    }

    /// The offset of the first place of `wanted`.
    #[cfg(test)]
    fn find(&self, wanted: &[String]) -> Option<usize> {
        self.find_all(wanted).into_iter().next()
    }

    /// The offset of each place of `wanted`, in order.
    fn find_all(&self, wanted: &[String]) -> Vec<usize> {
        self.tokens
            .windows(wanted.len())
            .filter(|window| {
                window
                    .iter()
                    .zip(wanted)
                    .all(|((text, _), want)| text == want)
            })
            .map(|window| window[0].1)
            .collect()
    }
}

fn line_col(text: &str, offset: usize) -> (u32, u32) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let start = before.rfind('\n').map_or(0, |at| at + 1);
    let col = before[start..].chars().count() + 1;
    (
        u32::try_from(line).unwrap_or(u32::MAX),
        u32::try_from(col).unwrap_or(u32::MAX),
    )
}

/// A path in the code of a module.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UsedPath {
    /// The module of the code, as `crate::a::b`.
    module: Vec<String>,
    /// The path, made absolute.
    path: Vec<String>,
    line: u32,
    col: u32,
}

fn module_rule(workspace: &Workspace, rule: &ModuleRule) -> Result<Vec<Finding>, CliError> {
    let Some(member) = workspace
        .members
        .iter()
        .find(|member| member.name == rule.package)
    else {
        warn(&format!(
            "rule `{}` names `{}`, which is not a workspace package",
            rule.rule, rule.package
        ));
        return Ok(Vec::new());
    };
    let paths = RulePaths {
        from: module_paths(&rule.rule, &rule.from)?,
        deny: module_paths(&rule.rule, &rule.deny)?,
        allow: module_paths(&rule.rule, &rule.allow)?,
    };
    let root = workspace.root.as_std_path();
    let mut found: BTreeMap<(String, u32, u32, String), Finding> = BTreeMap::new();
    for target in &member.targets {
        let walked = used_paths(root, target);
        for skipped in &walked.skipped {
            warn(&format!(
                "the rule `{}` does not judge `{}`: {}",
                rule.rule, skipped.path, skipped.reason
            ));
        }
        for (path, used) in walked.files {
            for item in used {
                let Some(denied) = paths.denied(&item) else {
                    continue;
                };
                let finding = module_finding(rule, &path, &item, denied);
                let key = (path.clone(), item.line, item.col, finding.to.clone());
                found.entry(key).or_insert(finding);
            }
        }
    }
    Ok(found.into_values().collect())
}

/// The module paths of one module rule.
struct RulePaths {
    from: Vec<Vec<String>>,
    deny: Vec<Vec<String>>,
    allow: Vec<Vec<String>>,
}

impl RulePaths {
    /// The denied prefix that a used path breaks, if the rule checks the
    /// module of the path and does not allow the path.
    fn denied(&self, item: &UsedPath) -> Option<&[String]> {
        if !self
            .from
            .iter()
            .any(|prefix| starts_with(&item.module, prefix))
        {
            return None;
        }
        let denied = self
            .deny
            .iter()
            .find(|prefix| starts_with(&item.path, prefix))?;
        let allowed = self
            .allow
            .iter()
            .any(|prefix| starts_with(&item.path, prefix))
            || starts_with(&item.path, &item.module);
        (!allowed).then_some(denied.as_slice())
    }
}

/// The paths that the code of one target uses, for each file.
fn used_paths(root: &Path, target: &workspace::Target) -> Walk<(String, Vec<UsedPath>)> {
    let roots = [TargetRoot {
        path: target.src_path.clone(),
        edition: parse_edition(&target.edition),
        test_only: false,
    }];
    walk(root, &roots, |visit| {
        let text = fs::read_to_string(root.join(&visit.path))
            .map_err(|err| format!("cannot read the file: {err}"))?;
        let mut base = vec!["crate".to_owned()];
        base.extend(visit.module_path.iter().cloned());
        with_parsed(&text, visit.edition, |file, normalized| {
            let mut scan = PathScan {
                text: normalized,
                used: Vec::new(),
                decls: Vec::new(),
            };
            scan.items(file.syntax(), &base, &mut Vec::new());
            ((visit.path.clone(), scan.used), scan.decls)
        })
        .map_err(crate::structure::source::parse_reason)
    })
}

/// The finding for a used path that breaks the denied prefix `denied`.
fn module_finding(rule: &ModuleRule, path: &str, item: &UsedPath, denied: &[String]) -> Finding {
    let module = item.module.join("::");
    let to = item.path.join("::");
    let message = rule.message.as_deref().map_or_else(
        || format!("{module} must not use {to}"),
        |template| fill(template, &[("from", &module), ("to", &to)]),
    );
    let advice = format!(
        "Remove the use of {} from {module}, or move the shared code to a module that the rule allows",
        denied.join("::")
    );
    Finding {
        rule: rule.rule.clone(),
        kind: Kind::ModulePath,
        from: module,
        to,
        path: path.to_owned(),
        line: item.line,
        col: item.col,
        pattern: None,
        dependency_kinds: Vec::new(),
        origin: None,
        message,
        actions: vec![Action::manual("move-code", advice)],
    }
}

fn module_paths(rule: &str, values: &[String]) -> Result<Vec<Vec<String>>, CliError> {
    values
        .iter()
        .map(|value| {
            let segments: Vec<String> = value
                .split("::")
                .map(|part| part.trim().to_owned())
                .collect();
            if segments.first().map(String::as_str) != Some("crate")
                || segments.iter().any(String::is_empty)
            {
                return Err(invalid(&format!(
                    "rule `{rule}` names the module `{value}`; write a module path as `crate::a::b`"
                )));
            }
            Ok(segments)
        })
        .collect()
}

fn starts_with(path: &[String], prefix: &[String]) -> bool {
    path.len() >= prefix.len() && path[..prefix.len()] == *prefix
}

/// The walk of one file for the module rules.
struct PathScan<'a> {
    text: &'a str,
    used: Vec<UsedPath>,
    decls: Vec<ModuleDecl>,
}

impl PathScan<'_> {
    /// Walks `node` in the module `module`. `inline` is the path of the inline
    /// modules since the file module.
    fn items(&mut self, node: &SyntaxNode, module: &[String], inline: &mut Vec<String>) {
        for child in node.children() {
            match child.kind() {
                SyntaxKind::MODULE => self.module(&child, module, inline),
                SyntaxKind::USE => {
                    if let Some(tree) =
                        ast::Use::cast(child.clone()).and_then(|item| item.use_tree())
                    {
                        self.use_tree(&tree, &[], module);
                    }
                }
                SyntaxKind::PATH => self.path(&child, module),
                _ => self.items(&child, module, inline),
            }
        }
    }

    fn module(&mut self, node: &SyntaxNode, module: &[String], inline: &mut Vec<String>) {
        let Some(item) = ast::Module::cast(node.clone()) else {
            return;
        };
        let Some(name) = item.name() else {
            return;
        };
        let name = crate::attrs::plain_name(&name);
        let Some(list) = item.item_list() else {
            let decl = ModuleDecl {
                name,
                inline_path: inline.clone(),
                path_attr: None,
                under_cfg: false,
                cfg_off: false,
                test_only: false,
            };
            self.decls
                .extend(decl.for_files(crate::attrs::module_files(&item)));
            return;
        };
        let mut inner = module.to_vec();
        inner.push(name.clone());
        inline.push(name);
        self.items(list.syntax(), &inner, inline);
        inline.pop();
    }

    fn use_tree(&mut self, tree: &ast::UseTree, prefix: &[String], module: &[String]) {
        let mut path = prefix.to_vec();
        if let Some(own) = tree.path() {
            path.extend(segments(&own));
        }
        if let Some(list) = tree.use_tree_list() {
            for inner in list.use_trees() {
                self.use_tree(&inner, &path, module);
            }
            return;
        }
        if path.last().map(String::as_str) == Some("self") && path.len() > 1 {
            path.pop();
        }
        let offset = tree.syntax().text_range().start();
        self.push(module, &path, offset.into());
    }

    fn path(&mut self, node: &SyntaxNode, module: &[String]) {
        let Some(path) = ast::Path::cast(node.clone()) else {
            return;
        };
        self.push(module, &segments(&path), node.text_range().start().into());
        // A path can hold more paths, for example in generic arguments.
        for child in node.descendants().skip(1) {
            if child.kind() == SyntaxKind::PATH
                && child
                    .parent()
                    .is_some_and(|parent| parent.kind() != SyntaxKind::PATH)
                && let Some(inner) = ast::Path::cast(child.clone())
            {
                self.push(module, &segments(&inner), child.text_range().start().into());
            }
        }
    }

    fn push(&mut self, module: &[String], path: &[String], offset: usize) {
        let Some(absolute) = absolute(module, path) else {
            return;
        };
        let (line, col) = line_col(self.text, offset);
        self.used.push(UsedPath {
            module: module.to_vec(),
            path: absolute,
            line,
            col,
        });
    }
}

fn segments(path: &ast::Path) -> Vec<String> {
    path.segments()
        .map(|segment| {
            segment.name_ref().map_or_else(
                || segment.syntax().text().to_string(),
                |name| name.text().to_string(),
            )
        })
        .collect()
}

/// The path from the crate root, or `None` for a path that does not start at
/// `crate`, `self` or `super`. A plain path can name a local item or another
/// crate, and the scan cannot tell which, so it skips such a path.
fn absolute(module: &[String], path: &[String]) -> Option<Vec<String>> {
    let first = path.first()?;
    match first.as_str() {
        "crate" => Some(path.to_vec()),
        "self" => {
            let mut full = module.to_vec();
            full.extend(path[1..].iter().cloned());
            Some(full)
        }
        "super" => {
            let mut full = module.to_vec();
            let mut rest = path;
            while rest.first().map(String::as_str) == Some("super") {
                if full.len() <= 1 {
                    return None;
                }
                full.pop();
                rest = &rest[1..];
            }
            full.extend(rest.iter().cloned());
            Some(full)
        }
        _ => None,
    }
}

fn human(report: &Report<Summary, Finding>) -> String {
    let summary = &report.summary;
    let rules = summary.crate_rules
        + summary.source_rules
        + summary.module_rules
        + summary.item_rules
        + summary.reexport_rules
        + summary.cfg_rules;
    let mut text = format!(
        "cargo fallow boundaries: {rules} {}, {} workspace {}, {} source {}\n",
        plural(rules, "rule", "rules"),
        summary.workspace_edges,
        plural(summary.workspace_edges, "edge", "edges"),
        summary.source_files,
        plural(summary.source_files, "file", "files"),
    );
    if rules == 0 {
        text.push_str("No rules. Add them to `[workspace.metadata.fallow.boundaries]`.\n");
    } else if report.findings.is_empty() {
        text.push_str("No violations\n");
    }
    for finding in &report.findings {
        let _ = writeln!(
            text,
            "  {}: {} ({}:{})",
            finding.rule, finding.message, finding.path, finding.line
        );
    }
    if summary.unresolved > 0 {
        let _ = writeln!(
            text,
            "  {} {} that the item rules cannot resolve, with the name of a denied item",
            summary.unresolved,
            plural(summary.unresolved, "use", "uses")
        );
    }
    text.push_str(&gates_text(&report.gate_outcomes));
    text
}

fn sarif_log(report: &Report<Summary, Finding>, findings: &[shared::Finding]) -> serde_json::Value {
    // A source rule can give `source-pattern` and `allow-entry` findings. The
    // rule text comes from a `source-pattern` finding when the rule has one,
    // so it does not depend on the order of the findings.
    let mut rules: BTreeMap<String, String> = BTreeMap::new();
    for finding in &report.findings {
        let text = match finding.kind {
            Kind::CrateDependency => "A workspace crate depends on a crate that a rule denies.",
            Kind::SourcePattern => "A file contains a pattern that a rule denies.",
            Kind::ModulePath => "A module uses a module that a rule denies.",
            Kind::ItemPath => "Code uses a workspace item that a rule denies.",
            Kind::GlobReexport => "A glob re-export reads a crate or a module that a rule denies.",
            Kind::CfgPath => {
                "Code that a cfg build compiles uses an item that the build does not compile."
            }
            Kind::AllowEntry => {
                "An allow entry of a source rule does not fit its file: fewer matches than \
                     max, or a missing required pattern."
            }
        };
        if finding.kind == Kind::AllowEntry {
            rules
                .entry(finding.rule.clone())
                .or_insert_with(|| text.to_owned());
        } else {
            rules.insert(finding.rule.clone(), text.to_owned());
        }
    }
    sarif::log(&rules, findings, &report.gate_outcomes)
}

#[cfg(test)]
mod tests;
