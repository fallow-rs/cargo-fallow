//! The rules of the `deps` command.

use crate::fix::deps::fixes;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use cargo_metadata::{DependencyKind, Metadata, Package, PackageId};
use serde_json::Value;

use super::manifest::{self, Entry, Section, normalize_platform};
use super::report::{Action, Finding, Rule, Severity, SkippedPackage};
use super::usage::{self, Context, PackageUsage};
use crate::graph::{Graph, crate_name};
use crate::workspace::{self, Workspace};

/// Why `fix` leaves an entry that turns on features.
const MANUAL_FEATURES: &str = "The entry turns on features, and another dependency can need them through feature unification.";
/// Why `fix` leaves a misplaced entry that a feature names.
const MANUAL_FEATURE_NAMES: &str =
    "A feature of the package names the dependency, and a feature cannot name a dev-dependency.";
/// Why `fix` leaves a workspace entry.
const MANUAL_INHERITED: &str = "A member entry that `fix` keeps inherits it.";

/// The table of the cargo-fallow ignore list in a package.
const PACKAGE_IGNORE_TABLE: &str = "package.metadata.fallow.deps";
/// The table of the cargo-fallow ignore list in the workspace root.
const WORKSPACE_IGNORE_TABLE: &str = "workspace.metadata.fallow.deps";
/// The target kinds that give a crate name to other crates.
const LIB_KINDS: [&str; 6] = ["lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"];

/// The result of the rules.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    /// The findings, not sorted.
    pub findings: Vec<Finding>,
    /// The packages that the rules did not judge.
    pub skipped: Vec<SkippedPackage>,
}

/// One declared dependency with the facts that the rules need.
struct Declared<'a> {
    dependency: &'a cargo_metadata::Dependency,
    key: String,
    section: Section,
    platform: Option<String>,
}

/// The ignore lists of one scope, with the entries that hid a finding.
#[derive(Default)]
struct IgnoreList {
    /// The names of the cargo-fallow list, which the redundant check covers.
    own: Vec<String>,
    /// The names of the cargo-shear list, which the run reads for migration.
    shear: Vec<String>,
    /// The names that hid a finding.
    used: BTreeSet<String>,
}

impl IgnoreList {
    fn from_metadata(value: &Value) -> Self {
        Self {
            own: strings(&value["fallow"]["deps"]["ignored"]),
            shear: strings(&value["cargo-shear"]["ignored"]),
            used: BTreeSet::new(),
        }
    }

    /// True when the list hides a finding for one of `names`. A hit marks the
    /// entry as used.
    fn hides(&mut self, names: &[&str]) -> bool {
        let mut hit = false;
        for name in names {
            if self.own.iter().any(|own| own == name) {
                self.used.insert((*name).to_owned());
                hit = true;
            }
            hit |= self.shear.iter().any(|shear| shear == name);
        }
        hit
    }

    fn redundant(&self) -> impl Iterator<Item = &String> {
        self.own.iter().filter(|name| !self.used.contains(*name))
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Applies the rules to each member and to the workspace dependency table.
pub fn analyze(workspace: &Workspace, metadata: &Metadata, graph: &Graph) -> Analysis {
    let mut usage = usage::collect(graph, workspace.root.as_std_path());
    add_codegen_names(metadata, &mut usage);
    let mut analysis = Analysis::default();
    let skipped = skipped_packages(workspace, graph);
    let features = super::features::FeatureRule::new(workspace, metadata);
    let mut workspace_ignores = IgnoreList::from_metadata(&metadata.workspace_metadata);
    let mut member_keys: BTreeSet<String> = BTreeSet::new();
    let empty = PackageUsage::default();
    // The workspace keys that a member entry inherits, where `fix` keeps the
    // member entry. Without the workspace entry, that member entry breaks.
    let mut kept_inherited: BTreeSet<String> = BTreeSet::new();
    for package in metadata.workspace_packages() {
        let manifest_path = workspace::relative_path(&workspace.root, &package.manifest_path);
        let text = match fs::read_to_string(&package.manifest_path) {
            Ok(text) => text,
            Err(err) => {
                analysis.skipped.push(SkippedPackage {
                    package: package.name.to_string(),
                    reason: format!("cannot read `{manifest_path}`: {err}"),
                });
                continue;
            }
        };
        let entries = manifest::entries(&text);
        if let Some(reason) = skipped.get(package.name.as_str()) {
            member_keys.extend(inherited_keys(&entries, &[]));
            // The rules did not judge the package, so an ignore entry for one of
            // its dependencies is not redundant.
            for dependency in &package.dependencies {
                let key = dependency.rename.as_deref().unwrap_or(&dependency.name);
                workspace_ignores.hides(&[key, dependency.name.as_str()]);
            }
            analysis.skipped.push(SkippedPackage {
                package: package.name.to_string(),
                reason: reason.clone(),
            });
            continue;
        }
        let used = usage.get(package.name.as_str()).unwrap_or(&empty);
        let mut ignores = IgnoreList::from_metadata(&package.metadata);
        let found = package_findings(metadata, package, used)
            .into_iter()
            .chain(feature_findings(package, &text, &features));
        let removed = add_findings(
            &mut analysis,
            found,
            [&mut ignores, &mut workspace_ignores],
            &manifest_path,
            &entries,
        );
        member_keys.extend(inherited_keys(&entries, &removed));
        kept_inherited.extend(kept_inherited_keys(&entries, &removed));
        for name in ignores.redundant() {
            analysis.findings.push(redundant_ignore(
                Some(package.name.to_string()),
                &manifest_path,
                PACKAGE_IGNORE_TABLE,
                name,
                manifest::line_in_table(&text, PACKAGE_IGNORE_TABLE, name),
            ));
        }
    }
    let root_manifest = workspace.root.join("Cargo.toml");
    if let Ok(text) = fs::read_to_string(&root_manifest) {
        let root_path = workspace::relative_path(&workspace.root, &root_manifest);
        for entry in manifest::entries(&text) {
            if entry.section != Section::Workspace
                || member_keys.contains(&entry.key)
                || workspace_ignores.hides(&[entry.key.as_str()])
            {
                continue;
            }
            let mut finding = finding(
                Rule::UnusedWorkspaceDependency,
                None,
                Section::Workspace.label().to_owned(),
                None,
                entry.key.clone(),
            );
            finding.manifest_path.clone_from(&root_path);
            finding.line = Some(entry.line);
            finding.manual = kept_inherited
                .contains(&entry.key)
                .then_some(MANUAL_INHERITED);
            finding.actions = actions(&finding);
            analysis.findings.push(finding);
        }
        for name in workspace_ignores.redundant() {
            analysis.findings.push(redundant_ignore(
                None,
                &root_path,
                WORKSPACE_IGNORE_TABLE,
                name,
                manifest::line_in_table(&text, WORKSPACE_IGNORE_TABLE, name),
            ));
        }
    }
    analysis
}

/// Adds the findings of one package that no ignore list hides, with their
/// manifest path, line and actions. Returns the findings whose fix removes
/// the entry.
fn add_findings(
    analysis: &mut Analysis,
    found: impl Iterator<Item = Finding>,
    ignores: [&mut IgnoreList; 2],
    manifest_path: &str,
    entries: &[Entry],
) -> Vec<Finding> {
    let [own, above] = ignores;
    let mut removed = Vec::new();
    for mut finding in found {
        let names = [finding.dependency.as_str(), package_name(&finding)];
        // Each list must see the names, so that each list marks its hits.
        let hidden_here = own.hides(&names);
        let hidden_above = above.hides(&names);
        if hidden_here || hidden_above {
            continue;
        }
        manifest_path.clone_into(&mut finding.manifest_path);
        // A feature finding has the line of its key already.
        if finding.rule != Rule::UnusedFeature {
            finding.line = line_of(entries, &finding);
        }
        finding.actions = actions(&finding);
        if is_removal(finding.rule) {
            removed.push(finding.clone());
        }
        analysis.findings.push(finding);
    }
    removed
}

/// The `unused-feature` findings of one package, with the line of each key.
fn feature_findings(
    package: &Package,
    manifest: &str,
    features: &super::features::FeatureRule,
) -> Vec<Finding> {
    features
        .unused(package, manifest)
        .into_iter()
        .map(|(name, line)| {
            let mut found = finding(
                Rule::UnusedFeature,
                Some(package.name.to_string()),
                "features".to_owned(),
                None,
                name,
            );
            found.line = line;
            found
        })
        .collect()
}

/// The dev-dependencies that compile test files outside the targets.
const COMPILE_TEST_HARNESSES: [&str; 4] = ["trybuild", "macrotest", "ui_test", "compiletest_rs"];

/// The suffixes of a build dependency that writes code for a runtime crate,
/// for example `prost-build` for `prost`.
const CODEGEN_SUFFIXES: [&str; 4] = ["-build", "_build", "-codegen", "_codegen"];

/// Adds the runtime crate of each code generator in `[build-dependencies]` to
/// the names that the build script can write.
pub(super) fn add_codegen_names(metadata: &Metadata, usage: &mut BTreeMap<String, PackageUsage>) {
    for package in metadata.workspace_packages() {
        let Some(names) = usage
            .get_mut(package.name.as_str())
            .and_then(|used| used.generated.as_mut())
        else {
            continue;
        };
        for dependency in &package.dependencies {
            if dependency.kind != DependencyKind::Build {
                continue;
            }
            for suffix in CODEGEN_SUFFIXES {
                if let Some(stem) = dependency.name.strip_suffix(suffix) {
                    names.insert(crate_name(stem));
                }
            }
        }
    }
}

/// True for a rule whose fix removes the entry, so that the entry no longer
/// inherits from `[workspace.dependencies]`.
const fn is_removal(rule: Rule) -> bool {
    matches!(
        rule,
        Rule::UnusedDependency | Rule::UnusedDevDependency | Rule::UnusedBuildDependency
    )
}

/// The keys that the entries inherit from `[workspace.dependencies]`, without
/// the entries that a finding in `removed` removes.
/// The keys of the inheriting entries that `fix` keeps.
fn kept_inherited_keys<'a>(
    entries: &'a [Entry],
    removed: &'a [Finding],
) -> impl Iterator<Item = String> + 'a {
    entries
        .iter()
        .filter(|entry| entry.inherits)
        .filter(|entry| {
            removed.iter().any(|finding| {
                finding.manual.is_some() && line_of(std::slice::from_ref(*entry), finding).is_some()
            })
        })
        .map(|entry| entry.key.clone())
}

fn inherited_keys(entries: &[Entry], removed: &[Finding]) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| entry.inherits)
        .filter(|entry| {
            !removed
                .iter()
                .any(|finding| line_of(std::slice::from_ref(*entry), finding).is_some())
        })
        .map(|entry| entry.key.clone())
        .collect()
}

fn package_name(finding: &Finding) -> &str {
    finding
        .package_name
        .as_deref()
        .unwrap_or(finding.dependency.as_str())
}

/// The packages with a file that did not parse, keyed by package name. The
/// graph does not see the code of such a file, so each rule stays silent.
fn skipped_packages(workspace: &Workspace, graph: &Graph) -> BTreeMap<String, String> {
    let mut skipped = BTreeMap::new();
    for file in &graph.skipped {
        let owner = workspace
            .members
            .iter()
            .filter(|member| file.path.starts_with(manifest_dir(&member.manifest_path)))
            .max_by_key(|member| member.manifest_path.len());
        if let Some(member) = owner {
            skipped
                .entry(member.name.clone())
                .or_insert_with(|| format!("`{}` is not in the graph: {}", file.path, file.reason));
        }
    }
    skipped
}

fn manifest_dir(manifest_path: &str) -> &str {
    manifest_path
        .strip_suffix("Cargo.toml")
        .unwrap_or(manifest_path)
}

/// The findings of one package, before the ignore lists. The manifest path,
/// the line and the actions come later.
fn package_findings(metadata: &Metadata, package: &Package, used: &PackageUsage) -> Vec<Finding> {
    let declared: Vec<Declared<'_>> = package
        .dependencies
        .iter()
        .map(|dependency| Declared {
            dependency,
            key: dependency
                .rename
                .clone()
                .unwrap_or_else(|| dependency.name.clone()),
            section: match dependency.kind {
                DependencyKind::Development => Section::Dev,
                DependencyKind::Build => Section::Build,
                _ => Section::Normal,
            },
            platform: dependency.target.as_ref().map(ToString::to_string),
        })
        .collect();
    // A compile-test harness builds test files that no target declares, with
    // the dev-dependencies of the package.
    let compile_tests = declared.iter().any(|entry| {
        entry.section == Section::Dev
            && COMPILE_TEST_HARNESSES.contains(&entry.dependency.name.as_str())
    });
    let mut findings = Vec::new();
    for entry in &declared {
        if compile_tests && entry.section == Section::Dev {
            continue;
        }
        let Some(resolved) = resolved_packages(metadata, package, &entry.dependency.name) else {
            continue;
        };
        let links = resolved.iter().any(|dep| dep.links.is_some());
        let code_name = entry.dependency.rename.as_deref().map_or_else(
            || lib_name(&resolved).unwrap_or_else(|| crate_name(&entry.dependency.name)),
            crate_name,
        );
        let contexts = used.contexts(&code_name);
        let also_normal = declared.iter().any(|other| {
            other.section == Section::Normal && other.dependency.name == entry.dependency.name
        });
        let Some((rule, used_by)) = rule_of(
            entry,
            &contexts,
            also_normal,
            feature_configures(package, &entry.key),
        ) else {
            continue;
        };
        // A package with a `links` key can link a native library, or give facts
        // to the build script of this package, with no name in the code. Only
        // a warning stays.
        if links && rule.severity() == Severity::Error {
            continue;
        }
        let mut found = finding(
            rule,
            Some(package.name.to_string()),
            entry.section.label().to_owned(),
            entry.platform.clone(),
            entry.key.clone(),
        );
        found.package_name =
            (entry.dependency.name != entry.key).then(|| entry.dependency.name.clone());
        found.optional = entry.dependency.optional;
        found.used_by = used_by;
        found.manual = if !entry.dependency.features.is_empty() {
            Some(MANUAL_FEATURES)
        } else if rule == Rule::MisplacedDependency && feature_configures(package, &entry.key) {
            Some(MANUAL_FEATURE_NAMES)
        } else {
            None
        };
        // Only a dev entry with the same key, on each platform of the normal
        // entry, makes the normal entry a plain removal.
        found.in_dev = declared.iter().any(|other| {
            other.section == Section::Dev
                && other.dependency.name == entry.dependency.name
                && other.key == entry.key
                && (other.platform.is_none() || other.platform == entry.platform)
        });
        findings.push(found);
    }
    findings
}

/// The rule for one dependency and the contexts that use it, or `None` when
/// the dependency is in use.
fn rule_of(
    entry: &Declared<'_>,
    contexts: &BTreeSet<Context>,
    also_normal: bool,
    feature_only: bool,
) -> Option<(Rule, Vec<&'static str>)> {
    let optional = entry.dependency.optional;
    let unused = |rule: Rule| {
        let rule = if optional {
            Rule::UnusedOptionalDependency
        } else if feature_only {
            Rule::FeatureOnlyDependency
        } else {
            rule
        };
        Some((rule, Vec::new()))
    };
    match entry.section {
        Section::Normal => {
            if contexts.iter().any(|context| context.is_production()) {
                return None;
            }
            let tests: Vec<&'static str> = contexts
                .iter()
                .filter(|context| context.is_test())
                .map(|context| context.label())
                .collect();
            if tests.is_empty() {
                unused(Rule::UnusedDependency)
            } else if optional {
                // A feature cannot enable a dev-dependency, so the entry stays.
                None
            } else {
                Some((Rule::MisplacedDependency, tests))
            }
        }
        Section::Dev => {
            let used = contexts
                .iter()
                .any(|context| context.is_test() || context.is_production());
            // A dev-dependency that repeats a normal one often turns on more
            // features for the tests.
            (!used && !also_normal).then_some((Rule::UnusedDevDependency, Vec::new()))
        }
        Section::Build => {
            if contexts.contains(&Context::Build) {
                None
            } else {
                unused(Rule::UnusedBuildDependency)
            }
        }
        Section::Workspace => None,
    }
}

/// The resolved packages of a declared dependency, or `None` when the
/// resolve does not have it, for example a package with no library.
pub(super) fn resolved_packages<'a>(
    metadata: &'a Metadata,
    package: &Package,
    name: &str,
) -> Option<Vec<&'a Package>> {
    let node = metadata
        .resolve
        .as_ref()?
        .nodes
        .iter()
        .find(|node| node.id == package.id)?;
    let ids: BTreeSet<&PackageId> = node.deps.iter().map(|dep| &dep.pkg).collect();
    let found: Vec<&Package> = metadata
        .packages
        .iter()
        .filter(|candidate| ids.contains(&candidate.id) && candidate.name.as_str() == name)
        .collect();
    (!found.is_empty()).then_some(found)
}

/// The crate name of the library of a resolved package.
pub(super) fn lib_name(resolved: &[&Package]) -> Option<String> {
    resolved.iter().find_map(|package| {
        package
            .targets
            .iter()
            .find(|target| {
                target
                    .kind
                    .iter()
                    .any(|kind| LIB_KINDS.contains(&kind.to_string().as_str()))
            })
            .map(|target| crate_name(&target.name))
    })
}

/// True when a feature of the package sets a feature of the dependency, as
/// `key/feature` or `key?/feature`.
fn feature_configures(package: &Package, key: &str) -> bool {
    let plain = format!("{key}/");
    let weak = format!("{key}?/");
    package
        .features
        .values()
        .flatten()
        .any(|value| value.starts_with(&plain) || value.starts_with(&weak))
}

fn finding(
    rule: Rule,
    package: Option<String>,
    section: String,
    platform: Option<String>,
    dependency: String,
) -> Finding {
    Finding {
        rule,
        severity: rule.severity(),
        package,
        manifest_path: String::new(),
        line: None,
        section,
        platform,
        dependency,
        package_name: None,
        optional: false,
        used_by: Vec::new(),
        in_dev: false,
        manual: None,
        version: None,
        workspace_only_features: Vec::new(),
        member_only_features: Vec::new(),
        actions: Vec::new(),
    }
}

fn redundant_ignore(
    package: Option<String>,
    manifest_path: &str,
    table: &str,
    name: &str,
    line: Option<u32>,
) -> Finding {
    let mut found = finding(
        Rule::RedundantIgnore,
        package,
        table.to_owned(),
        None,
        name.to_owned(),
    );
    manifest_path.clone_into(&mut found.manifest_path);
    found.line = line;
    found.actions = actions(&found);
    found
}

fn line_of(entries: &[Entry], finding: &Finding) -> Option<u32> {
    let platform = finding.platform.as_deref().map(normalize_platform);
    entries
        .iter()
        .find(|entry| {
            entry.section.label() == finding.section
                && entry.key == finding.dependency
                && entry.platform.as_deref().map(normalize_platform) == platform
        })
        .map(|entry| entry.line)
}

/// The written table name, for example `[target.'cfg(windows)'.dependencies]`.
fn table_name(finding: &Finding) -> String {
    finding.platform.as_ref().map_or_else(
        || format!("[{}]", finding.section),
        |platform| format!("[target.'{platform}'.{}]", finding.section),
    )
}

fn actions(finding: &Finding) -> Vec<Action> {
    let key = &finding.dependency;
    let table = table_name(finding);
    let (kind, description) = match finding.rule {
        Rule::MisplacedDependency if finding.in_dev => (
            "remove-dependency",
            format!("Remove `{key}` from {table}. [dev-dependencies] has it already."),
        ),
        Rule::MisplacedDependency => (
            "move-to-dev-dependencies",
            format!("Move `{key}` from {table} to [dev-dependencies]."),
        ),
        Rule::UnusedOptionalDependency => (
            "remove-dependency",
            format!("Remove `{key}` from {table} and from each feature that names it."),
        ),
        Rule::FeatureOnlyDependency => (
            "remove-dependency",
            format!(
                "Remove `{key}` from {table} and from each feature that names it. If the \
                 feature must set the features of `{key}`, add `{key}` to `ignored` in \
                 [{PACKAGE_IGNORE_TABLE}]."
            ),
        ),
        Rule::UnusedFeature => (
            "remove-feature",
            format!(
                "Remove the feature `{key}` from [features], or use it in a `cfg`. To keep \
                 it, add `{key}` to `ignored` in [{PACKAGE_IGNORE_TABLE}]."
            ),
        ),
        Rule::RedundantIgnore => (
            "remove-ignore",
            format!("Remove `{key}` from `ignored` in [{}].", finding.section),
        ),
        // The drift module writes its own action.
        Rule::FeatureUnificationDrift => (
            "pin-features",
            format!("Pin the features of `{key}` in the manifest of the member."),
        ),
        Rule::UnusedDependency
        | Rule::UnusedDevDependency
        | Rule::UnusedBuildDependency
        | Rule::UnusedWorkspaceDependency => {
            ("remove-dependency", format!("Remove `{key}` from {table}."))
        }
    };
    let manual = finding.manual.filter(|_| fixes(finding.rule));
    let description = match manual {
        Some(reason) => format!("{description} {reason} So `cargo fallow fix` does not change it."),
        None => description,
    };
    vec![Action {
        kind,
        auto_fixable: fixes(finding.rule) && manual.is_none(),
        description,
        file: finding.manifest_path.clone(),
        line: finding.line,
    }]
}
