//! The `cfg` rules: a path from code that a build compiles to an item that
//! the same build does not compile.
//!
//! A rule lists builds. A build is the set of the `cfg` options that are on.
//! An option that a build of the rule names is on in the builds that name it
//! and off in the other builds. Each other option, for example a feature or a
//! target that no build names, is unknown. The logic has three values, so a
//! condition with an unknown option can be unknown.
//!
//! The rule reads each path in a function body or a signature and each `use`
//! item, with the resolution of the module graph. It reports a path only when
//! the path resolves to items with no doubt, the build surely compiles the
//! code, and the build surely compiles none of the items. When the syntax
//! cannot decide, the rule gives credit and does not report.

use std::collections::{BTreeMap, BTreeSet};

use super::{Finding, Kind, invalid, package_of};
use crate::attrs::split_list;
use crate::error::CliError;
use crate::graph::extract::{Binding, Origin, RefKind, UNKNOWN_CFG};
use crate::graph::resolve::{Class, Resolver, Source};
use crate::graph::{CrateId, Graph, ItemId, ModuleId};
use crate::structure::config::CfgRule;
use crate::structure::report::Action;
use crate::workspace::Workspace;

/// One build: the options that are on, and the options that the rule names.
struct Build<'r> {
    on: BTreeSet<String>,
    named: &'r BTreeSet<String>,
    label: String,
}

impl Build<'_> {
    /// The value of a condition with no whitespace, or `None` when it is
    /// unknown.
    fn eval(&self, condition: &str) -> Option<bool> {
        if let Some(list) = call(condition, "all") {
            return split_list(list)
                .into_iter()
                .try_fold(true, |all, part| match self.eval(part) {
                    Some(false) => Err(()),
                    Some(true) => Ok(all),
                    None => Ok(false),
                })
                .map_or(Some(false), |all| all.then_some(true));
        }
        if let Some(list) = call(condition, "any") {
            let mut unknown = false;
            for part in split_list(list) {
                match self.eval(part) {
                    Some(true) => return Some(true),
                    Some(false) => {}
                    None => unknown = true,
                }
            }
            return (!unknown).then_some(false);
        }
        if let Some(inner) = call(condition, "not") {
            return self.eval(inner).map(|value| !value);
        }
        if condition == UNKNOWN_CFG || !self.named.contains(condition) {
            return None;
        }
        Some(self.on.contains(condition))
    }

    /// The value of a list of conditions that must all hold.
    fn eval_all(&self, conditions: &[String]) -> Option<bool> {
        let mut unknown = false;
        for condition in conditions {
            match self.eval(condition) {
                Some(false) => return Some(false),
                Some(true) => {}
                None => unknown = true,
            }
        }
        (!unknown).then_some(true)
    }
}

/// The list in `name(list)`.
fn call<'t>(condition: &'t str, name: &str) -> Option<&'t str> {
    condition
        .strip_prefix(name)?
        .strip_prefix('(')?
        .strip_suffix(')')
}

/// An option of a build with no whitespace: `test` or `feature="x"`.
fn option(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// True for `name` or `name="value"`.
fn is_option(text: &str) -> bool {
    let (name, value) = text.split_once('=').unwrap_or((text, ""));
    let plain = |part: &str| {
        !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    plain(name)
        && (value.is_empty()
            || (value.len() >= 2 && value.starts_with('"') && value.ends_with('"')))
}

/// Checks the build options of the `cfg` rules.
pub fn check_rules(rules: &[CfgRule]) -> Result<(), CliError> {
    for rule in rules {
        if rule.builds.is_empty() || rule.builds.iter().any(Vec::is_empty) {
            return Err(invalid(&format!(
                "rule `{}` needs at least one build, and each build needs an option",
                rule.rule
            )));
        }
        if let Some(bad) = rule
            .builds
            .iter()
            .flatten()
            .find(|text| !is_option(&option(text)))
        {
            return Err(invalid(&format!(
                "rule `{}` has the build option `{bad}`. An option is a name or `name = \"value\"`",
                rule.rule
            )));
        }
    }
    Ok(())
}

/// The findings of the cfg rules on the module graph. `check_rules` must
/// accept the rules first.
pub fn findings(workspace: &Workspace, graph: &Graph, rules: &[CfgRule]) -> Vec<Finding> {
    let sites = sites(graph);
    let mut found = Vec::new();
    for rule in rules {
        let named: BTreeSet<String> = rule.builds.iter().flatten().map(|o| option(o)).collect();
        let builds: Vec<Build<'_>> = rule
            .builds
            .iter()
            .map(|build| Build {
                on: build.iter().map(|o| option(o)).collect(),
                named: &named,
                label: build.join(", "),
            })
            .collect();
        for site in &sites {
            let failed: Vec<&str> = builds
                .iter()
                .filter(|build| {
                    build.eval_all(&site.conditions) == Some(true)
                        && site
                            .items
                            .iter()
                            .all(|item| build.eval_all(item) == Some(false))
                })
                .map(|build| build.label.as_str())
                .collect();
            if !failed.is_empty() {
                found.push(finding(workspace, rule, site, &failed));
            }
        }
    }
    found
}

/// One path of the code with exact targets.
struct Site {
    path: String,
    line: u32,
    /// The name of the deepest target, as `module::item`.
    target: String,
    /// The conditions of the code.
    conditions: Vec<String>,
    /// The conditions of each target.
    items: Vec<Vec<String>>,
}

/// The paths whose credits are all exact, with their conditions.
fn sites(graph: &Graph) -> Vec<Site> {
    let resolver = Resolver::new(graph);
    let doubt = Doubt::new(graph, &resolver);
    let mut modules = vec![None; graph.modules.len()];
    let mut sites = Vec::new();
    for ((source, index), (exact, krate, mut items)) in credit_groups(&resolver) {
        items.sort_unstable();
        items.dedup();
        if !exact || items.is_empty() || !doubt.sure(graph, krate, &items) {
            continue;
        }
        let Some((module, own, path, line)) = place(graph, source, index) else {
            continue;
        };
        let mut conditions = own.clone();
        conditions.extend(module_conditions(
            graph,
            module,
            &doubt.shared,
            &mut modules,
        ));
        let targets = items
            .iter()
            .map(|&item| {
                let mut list = graph.items[item].cfg.clone();
                let around = graph.items[item].module;
                list.extend(module_conditions(
                    graph,
                    around,
                    &doubt.shared,
                    &mut modules,
                ));
                list
            })
            .collect();
        sites.push(Site {
            path: path.clone(),
            line,
            // A path credits each item on its way, for example the module and
            // the function. The deepest item names the target.
            target: items
                .iter()
                .map(|&item| graph.item_path(item))
                .max_by_key(String::len)
                .unwrap_or_default(),
            conditions,
            items: targets,
        });
    }
    sites
}

/// The credits of each import (`0`) and each reference (`1`): true when each
/// credit is exact, in a body or a signature and inside the workspace, the
/// crate of the code, and the items.
type Groups = BTreeMap<(u8, usize), (bool, CrateId, Vec<ItemId>)>;

fn credit_groups(resolver: &Resolver<'_>) -> Groups {
    let credits = resolver.credits();
    let mut groups = Groups::new();
    let named = credits.sites.iter().map(|site| (&site.credit, None));
    let direct = credits
        .direct
        .iter()
        .map(|credit| (credit, Some(credit.item)));
    for (credit, item) in direct.chain(named) {
        let key = match credit.source {
            Source::Import(index) => (0, index),
            Source::Ref(index) => (1, index),
        };
        let entry = groups
            .entry(key)
            .or_insert((true, credit.krate, Vec::new()));
        // A name credit is never exact, so its source gives credit.
        entry.0 &= item.is_some()
            && credit.class == Class::Exact
            && !credit.outside
            && matches!(credit.origin, Origin::Body | Origin::Signature);
        entry.2.extend(item);
    }
    groups
}

/// The module, the conditions, the file and the line of an anchored import
/// (`source` 0) or path (`source` 1). A `use` in a block can name a local
/// item, which the graph does not see, so an import must be anchored too.
fn place(
    graph: &Graph,
    source: u8,
    index: usize,
) -> Option<(ModuleId, &Vec<String>, &String, u32)> {
    let (module, own, file, line) = if source == 0 {
        let import = &graph.imports[index];
        anchored(&import.path, import.global).then_some(())?;
        (import.module, &import.cfg, import.file, import.line)
    } else {
        let reference = &graph.refs[index];
        let RefKind::Path { segments, global } = &reference.kind else {
            return None;
        };
        anchored(segments, *global).then_some(())?;
        (
            reference.module,
            &reference.cfg,
            reference.file,
            reference.line,
        )
    };
    let path = graph.paths.get(file as usize).filter(|_| line > 0)?;
    Some((module, own, path, line))
}

/// True for a path that starts at a module: `crate`, `self`, `super` or `::`.
/// A local item or a binding cannot shadow such a path, and the graph does not
/// see local items and bindings.
fn anchored(segments: &[String], global: bool) -> bool {
    global
        || segments
            .first()
            .is_some_and(|first| matches!(first.as_str(), "crate" | "self" | "super"))
}

/// What a glob import can give.
enum Glob {
    /// A glob of a workspace path that does not resolve: any name.
    Unknown,
    /// The modules and the enums that the glob reads.
    Sources(Vec<ModuleId>, Vec<ItemId>),
}

/// The modules where the graph can miss an item or a declaration.
struct Doubt {
    /// A macro call or an `include!` call at the level of the items can make
    /// items that the graph does not see.
    macros: BTreeSet<ModuleId>,
    /// Two `mod` declarations, for example under two exclusive `cfg`s, share
    /// one module of the graph.
    shared: BTreeSet<ModuleId>,
    /// The glob imports of each module. An item of the module hides a name of
    /// a glob, so the glob can give the name in a build without the item. A
    /// glob of a crate outside the workspace counts as a glob that gives no
    /// name of the workspace: the syntax cannot read that crate.
    globs: BTreeMap<ModuleId, Vec<Glob>>,
    /// The names of the items of each module.
    names: BTreeMap<ModuleId, BTreeSet<String>>,
}

impl Doubt {
    fn new(graph: &Graph, resolver: &Resolver<'_>) -> Self {
        let mut globs: BTreeMap<ModuleId, Vec<Glob>> = BTreeMap::new();
        for (index, import) in graph.imports.iter().enumerate() {
            if import.binding != Binding::Glob {
                continue;
            }
            let (modules, items) = resolver.glob_sources(index);
            let glob = if !modules.is_empty() || !items.is_empty() {
                Glob::Sources(modules, items)
            } else if in_workspace(graph, import) {
                Glob::Unknown
            } else {
                continue;
            };
            globs.entry(import.module).or_default().push(glob);
        }
        let mut declared: BTreeMap<ModuleId, usize> = BTreeMap::new();
        let mut names: BTreeMap<ModuleId, BTreeSet<String>> = BTreeMap::new();
        for item in &graph.items {
            if let Some(child) = item.child_module {
                *declared.entry(child).or_default() += 1;
            }
            if item.parent.is_none() {
                names
                    .entry(item.module)
                    .or_default()
                    .insert(item.name.clone());
            }
        }
        Self {
            macros: graph.macro_modules.clone(),
            shared: declared
                .into_iter()
                .filter(|(_, count)| *count > 1)
                .map(|(module, _)| module)
                .collect(),
            globs,
            names,
        }
    }

    /// True when a module or a module around it has a macro call or a shared
    /// declaration.
    fn in_chain(&self, graph: &Graph, module: ModuleId) -> bool {
        let mut current = Some(module);
        while let Some(id) = current {
            if self.macros.contains(&id) || self.shared.contains(&id) {
                return true;
            }
            current = graph.modules[id].parent;
        }
        false
    }

    /// True when a glob import of the module can give one of the names.
    fn glob_gives(&self, graph: &Graph, module: ModuleId, names: &BTreeSet<&str>) -> bool {
        self.globs
            .get(&module)
            .into_iter()
            .flatten()
            .any(|glob| match glob {
                Glob::Unknown => true,
                Glob::Sources(modules, items) => {
                    modules.iter().any(|&source| {
                        self.globs.contains_key(&source)
                            || self.in_chain(graph, source)
                            || self.names.get(&source).is_some_and(|declared| {
                                names.iter().any(|name| declared.contains(*name))
                            })
                    }) || items.iter().any(|&owner| {
                        graph.items.iter().any(|item| {
                            item.parent == Some(owner) && names.contains(item.name.as_str())
                        })
                    })
                }
            })
    }

    /// True when the graph surely sees each variant of the targets: each
    /// target is in the crate of the code and is not a member of a type or a
    /// trait, and no module of a lookup has a doubt. `cfg(test)` and the
    /// features are options of one crate and one package, so a target in
    /// another crate gives credit.
    fn sure(&self, graph: &Graph, krate: CrateId, items: &[ItemId]) -> bool {
        let names: BTreeSet<&str> = items
            .iter()
            .map(|&item| graph.items[item].name.as_str())
            .collect();
        items.iter().all(|&item| {
            let target = &graph.items[item];
            let lookups = std::iter::once(target.module).chain(target.child_module);
            target.krate == krate
                && target.parent.is_none()
                && lookups.into_iter().all(|module| {
                    !self.in_chain(graph, module) && !self.glob_gives(graph, module, &names)
                })
        })
    }
}

/// True for a glob import of a workspace path: it starts with `crate`,
/// `self` or `super`, or with the name of a workspace crate.
fn in_workspace(graph: &Graph, import: &crate::graph::Import) -> bool {
    let Some(first) = import.path.first() else {
        return true;
    };
    matches!(first.as_str(), "crate" | "self" | "super")
        || graph.crates[import.krate]
            .externs
            .iter()
            .any(|known| known.target.is_some() && known.name == *first)
}

/// The conditions of a module: those of its `mod` item and of the modules
/// around it.
fn module_conditions(
    graph: &Graph,
    module: ModuleId,
    shared: &BTreeSet<ModuleId>,
    memo: &mut Vec<Option<Vec<String>>>,
) -> Vec<String> {
    if let Some(known) = &memo[module] {
        return known.clone();
    }
    let conditions = match graph.modules[module].item {
        Some(item) => {
            let mut list = graph.items[item].cfg.clone();
            list.extend(module_conditions(
                graph,
                graph.items[item].module,
                shared,
                memo,
            ));
            list
        }
        None => Vec::new(),
    };
    // A module of two declarations keeps the item of one of them, so its
    // conditions are not known.
    let conditions = if shared.contains(&module) {
        vec![UNKNOWN_CFG.to_owned()]
    } else {
        conditions
    };
    memo[module] = Some(conditions.clone());
    conditions
}

fn finding(workspace: &Workspace, rule: &CfgRule, site: &Site, builds: &[&str]) -> Finding {
    let build = builds.join("; ");
    let message = rule.message.as_deref().map_or_else(
        || {
            format!(
                "{} uses {} in the build with {build}, but that build does not compile {}",
                site.path, site.target, site.target
            )
        },
        |template| {
            super::fill(
                template,
                &[
                    ("path", &site.path),
                    ("item", &site.target),
                    ("build", &build),
                ],
            )
        },
    );
    Finding {
        rule: rule.rule.clone(),
        kind: Kind::CfgPath,
        from: package_of(workspace, &site.path),
        to: site.target.clone(),
        path: site.path.clone(),
        line: site.line,
        col: 1,
        pattern: None,
        dependency_kinds: Vec::new(),
        origin: None,
        message,
        actions: vec![Action::manual(
            "align-cfg",
            format!(
                "Put the `cfg` of {} on the code, or compile {} in the build with {build}",
                site.target, site.target
            ),
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build<'r>(on: &[&str], named: &'r BTreeSet<String>) -> Build<'r> {
        Build {
            on: on.iter().map(|o| (*o).to_owned()).collect(),
            named,
            label: String::new(),
        }
    }

    #[test]
    fn the_logic_has_three_values() {
        let named: BTreeSet<String> = ["test", "miri"].map(str::to_owned).into();
        let miri = build(&["test", "miri"], &named);
        assert_eq!(miri.eval("test"), Some(true));
        assert_eq!(miri.eval("not(miri)"), Some(false));
        assert_eq!(miri.eval("all(test,not(miri))"), Some(false));
        assert_eq!(miri.eval("unix"), None);
        assert_eq!(miri.eval("all(test,unix)"), None);
        assert_eq!(miri.eval("all(not(miri),unix)"), Some(false));
        assert_eq!(miri.eval("any(miri,unix)"), Some(true));
        assert_eq!(miri.eval("any(not(test),unix)"), None);
        assert_eq!(miri.eval("not(unix)"), None);
        assert_eq!(miri.eval("?"), None);
        assert_eq!(miri.eval_all(&["test".to_owned(), "unix".to_owned()]), None);
        assert_eq!(
            miri.eval_all(&["unix".to_owned(), "not(test)".to_owned()]),
            Some(false)
        );
    }

    #[test]
    fn a_comma_in_a_value_gives_no_wrong_value() {
        let named: BTreeSet<String> = [option("feature = \"a,b\"")].into();
        let on = build(&["feature=\"a,b\""], &named);
        assert_ne!(on.eval("all(feature=\"a,b\",unix)"), Some(false));
    }

    #[test]
    fn an_option_is_a_name_or_a_name_with_a_value() {
        assert!(is_option("test"));
        assert!(is_option("feature=\"x\""));
        assert!(!is_option(""));
        assert!(!is_option("not(miri)"));
        assert!(!is_option("feature=x"));
    }

    #[test]
    fn a_feature_option_matches_without_whitespace() {
        let named: BTreeSet<String> = [option("feature = \"x\"")].into();
        let on = build(&["feature=\"x\""], &named);
        assert_eq!(on.eval("feature=\"x\""), Some(true));
        assert_eq!(on.eval("feature=\"y\""), None);
    }
}
