//! Item rules and glob re-export rules on the workspace module graph.
//!
//! An item rule names workspace items that the code of some packages must not
//! use. The rule reads each import and each path in the code of the packages,
//! and resolves it through `use`, aliases and re-exports. Only a path that
//! resolves to its end counts. A path that does not resolve, a method call and
//! a field access give no finding: the summary counts them in `unresolved`
//! when the last name is the name of a denied item or of a member of a
//! denied type. Paths in documentation and in
//! attributes do not count. Paths in macro tokens count when they resolve.
//!
//! A glob re-export rule names crates or modules that a `pub use <path>::*` in
//! some packages must not read. The rule uses the module that the glob path
//! resolves to, so an alias of the crate does not hide it.

use std::collections::BTreeMap;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::error::CliError;
use crate::graph::extract::{Binding, ItemKind, Origin, RefKind, Vis};
use crate::graph::resolve::{PathResolution, Resolver};
use crate::graph::{self, Graph, ItemId, ModuleId};
use crate::render::warn;
use crate::structure::boundaries::{Finding, Kind, Names, fill, invalid};
use crate::structure::config::{ItemRule, ReexportRule};
use crate::structure::report::Action;
use crate::workspace::Workspace;

/// The findings of the item rules and the glob re-export rules, and the
/// count of the uses that did not resolve.
#[derive(Debug, Default)]
pub struct Outcome {
    /// The findings.
    pub findings: Vec<Finding>,
    /// The paths, method calls and field accesses that did not resolve, with
    /// a denied item of the same last name.
    pub unresolved: usize,
}

/// Checks the item rules and the glob re-export rules. The module graph is
/// built only when a rule exists.
pub fn check(
    workspace: &Workspace,
    items: &[ItemRule],
    reexports: &[ReexportRule],
    names: &Names<'_>,
) -> Result<Outcome, CliError> {
    if items.is_empty() && reexports.is_empty() {
        return Ok(Outcome::default());
    }
    let module_graph = graph::build(workspace, &graph::Production::default());
    let resolver = Resolver::new(&module_graph);
    let context = Context {
        graph: &module_graph,
        resolver: &resolver,
        index: ImportIndex::new(&module_graph),
    };
    let mut outcome = Outcome::default();
    for rule in items {
        context.item_rule(rule, names, &mut outcome)?;
    }
    for rule in reexports {
        outcome.findings.extend(context.reexport_rule(rule, names)?);
    }
    Ok(outcome)
}

/// A set of denied or allowed places: items and modules, with their contents.
#[derive(Debug, Default)]
struct Places {
    items: FxHashSet<ItemId>,
    modules: FxHashSet<ModuleId>,
    /// The written path of each place, for the `to` field.
    labels: FxHashMap<Place, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Place {
    Item(ItemId),
    Module(ModuleId),
}

/// The deepest chain of glob imports that a binding walk follows.
const MAX_GLOB_DEPTH: usize = 8;

struct Context<'g> {
    graph: &'g Graph,
    resolver: &'g Resolver<'g>,
    index: ImportIndex<'g>,
}

/// The imports of each module, by binding.
#[derive(Default)]
struct ImportIndex<'g> {
    named: FxHashMap<(ModuleId, &'g str), Vec<usize>>,
    globs: FxHashMap<ModuleId, Vec<usize>>,
}

impl<'g> ImportIndex<'g> {
    fn new(graph: &'g Graph) -> Self {
        let mut index = Self::default();
        for (position, import) in graph.imports.iter().enumerate() {
            match &import.binding {
                Binding::Name(name) => index
                    .named
                    .entry((import.module, name.as_str()))
                    .or_default()
                    .push(position),
                Binding::Glob => index.globs.entry(import.module).or_default().push(position),
                Binding::Underscore => {}
            }
        }
        index
    }
}

impl Context<'_> {
    fn item_rule(
        &self,
        rule: &ItemRule,
        names: &Names<'_>,
        outcome: &mut Outcome,
    ) -> Result<(), CliError> {
        let from = names.expand(&rule.rule, &rule.from)?;
        let deny = self.places(&rule.rule, &rule.deny, true)?;
        let via = self.places(&rule.rule, &rule.allow_via, false)?;
        let denied_names = self.denied_names(&deny);
        let checked = |krate: usize| from.contains(&self.graph.crates[krate].package);
        // The named imports with a finding or a path through `allow-via`. A
        // path that starts with the name of such an import gives no second
        // finding.
        let mut settled: FxHashSet<usize> = FxHashSet::default();
        let mut found: BTreeMap<(String, u32, String), Finding> = BTreeMap::new();
        for (index, import) in self.graph.imports.iter().enumerate() {
            if !checked(import.krate) || import.binding == Binding::Glob {
                continue;
            }
            let resolved = self.resolver.resolve_import(index);
            let Binding::Name(name) = &import.binding else {
                continue;
            };
            if self.goes_via(&resolved, &via) {
                settled.insert(index);
                continue;
            }
            if let Some(to) = self.denied_target(&resolved, &deny) {
                settled.insert(index);
                let use_site = Use {
                    krate: import.krate,
                    file: import.file,
                    line: import.line,
                    origin: "code",
                };
                self.record(rule, &use_site, to, &mut found);
            }
        }
        for reference in &self.graph.refs {
            if !checked(reference.krate)
                || matches!(
                    reference.origin,
                    Origin::Doc | Origin::DocLink | Origin::Attr
                )
            {
                continue;
            }
            let (segments, global) = match &reference.kind {
                RefKind::Path { segments, global }
                | RefKind::RecordField {
                    segments, global, ..
                } => (segments, *global),
                RefKind::Method(name) | RefKind::Field(name) | RefKind::Member(name) => {
                    outcome.unresolved += usize::from(denied_names.contains(name.as_str()));
                    continue;
                }
                RefKind::Macro(_) => continue,
            };
            let Some(first) = segments.first() else {
                continue;
            };
            if !global && self.bound_through(reference.module, first, &via, &settled, 0) {
                continue;
            }
            let resolved =
                self.resolver
                    .resolve(reference.krate, reference.module, segments, global);
            if !resolved.exact {
                let last = segments.last().map(String::as_str).unwrap_or_default();
                outcome.unresolved += usize::from(denied_names.contains(last));
                continue;
            }
            // A guessed first name that resolves is outside the workspace.
            if resolved.guessed {
                continue;
            }
            if self.goes_via(&resolved, &via) {
                continue;
            }
            if let Some(to) = self.denied_target(&resolved, &deny) {
                let origin = match reference.origin {
                    Origin::Macro | Origin::PublicMacro => "macro",
                    _ => "code",
                };
                let use_site = Use {
                    krate: reference.krate,
                    file: reference.file,
                    line: reference.line,
                    origin,
                };
                self.record(rule, &use_site, to, &mut found);
            }
        }
        outcome.findings.extend(found.into_values());
        Ok(())
    }

    fn record(
        &self,
        rule: &ItemRule,
        site: &Use,
        to: &str,
        found: &mut BTreeMap<(String, u32, String), Finding>,
    ) {
        let path = self.graph.paths[site.file as usize].clone();
        let from = self.graph.crates[site.krate].package.clone();
        let message = rule.message.as_deref().map_or_else(
            || format!("{from} must not use {to}"),
            |template| fill(template, &[("from", &from), ("to", to)]),
        );
        let advice = if rule.allow_via.is_empty() {
            format!("Remove the use of {to} from {from}")
        } else {
            format!(
                "Use {to} through {}, or remove the use from {from}",
                rule.allow_via.join(" or ")
            )
        };
        found
            .entry((path.clone(), site.line, to.to_owned()))
            .or_insert_with(|| Finding {
                rule: rule.rule.clone(),
                kind: Kind::ItemPath,
                from,
                to: to.to_owned(),
                path,
                line: site.line,
                col: 1,
                pattern: None,
                dependency_kinds: Vec::new(),
                origin: Some(site.origin),
                message,
                actions: vec![Action::manual("move-code", advice)],
            });
    }

    fn reexport_rule(
        &self,
        rule: &ReexportRule,
        names: &Names<'_>,
    ) -> Result<Vec<Finding>, CliError> {
        let from = names.expand(&rule.rule, &rule.from)?;
        let deny = self.places(&rule.rule, &rule.deny, false)?;
        let mut found = Vec::new();
        for (index, import) in self.graph.imports.iter().enumerate() {
            let krate = &self.graph.crates[import.krate];
            if import.binding != Binding::Glob
                || import.vis != Vis::Public
                || !from.contains(&krate.package)
            {
                continue;
            }
            let (modules, enums) = self.resolver.glob_sources(index);
            let hit = modules
                .iter()
                .find_map(|&module| self.module_in(module, &deny))
                .or_else(|| enums.iter().find_map(|&item| self.item_in(item, &deny)));
            let Some(to) = hit else {
                continue;
            };
            let path = self.graph.paths[import.file as usize].clone();
            let message = rule.message.as_deref().map_or_else(
                || format!("{} must not re-export {to} with a glob", krate.package),
                |template| fill(template, &[("from", &krate.package), ("to", to)]),
            );
            found.push(Finding {
                rule: rule.rule.clone(),
                kind: Kind::GlobReexport,
                from: krate.package.clone(),
                to: to.to_owned(),
                path,
                line: import.line,
                col: 1,
                pattern: None,
                dependency_kinds: Vec::new(),
                origin: None,
                message,
                actions: vec![Action::manual(
                    "name-reexports",
                    format!("Re-export the items of {to} by name"),
                )],
            });
        }
        Ok(found)
    }

    /// Resolves the paths of a rule. Each path must name a workspace item or
    /// module, and an item rule cannot name a macro.
    fn places(&self, rule: &str, values: &[String], items: bool) -> Result<Places, CliError> {
        let mut places = Places::default();
        for value in values {
            let segments: Vec<String> = value
                .split("::")
                .map(|part| part.trim().to_owned())
                .collect();
            let fail =
                |reason: &str| invalid(&format!("rule `{rule}` names `{value}`, which {reason}"));
            if segments.iter().any(String::is_empty) {
                return Err(fail("is not a path"));
            }
            let Some(root) = self
                .graph
                .crates
                .iter()
                .position(|krate| krate.kind.is_library() && krate.name == segments[0])
            else {
                return Err(fail("is not a library crate of the workspace"));
            };
            let module = self.graph.crates[root].root_module;
            if segments.len() == 1 {
                places.insert(Place::Module(module), value);
                continue;
            }
            let mut path = vec!["crate".to_owned()];
            path.extend(segments[1..].iter().cloned());
            let resolved = self.resolver.resolve(root, module, &path, false);
            let targets = self.owned_items(&resolved);
            // The code can remove a denied item. The rule then has nothing to
            // check, but the configuration stays valid.
            if !resolved.exact || (targets.is_empty() && resolved.modules.is_empty()) {
                warn(&format!(
                    "rule `{rule}` names `{value}`, which is not an item of the workspace"
                ));
                continue;
            }
            for &item in &targets {
                let entry = &self.graph.items[item];
                if items && entry.kind == ItemKind::Macro {
                    return Err(fail("is a macro; an item rule cannot follow macro calls"));
                }
                places.insert(Place::Item(item), value);
                if let Some(child) = entry.child_module {
                    places.insert(Place::Module(child), value);
                }
            }
            for &module in &resolved.modules {
                places.insert(Place::Module(module), value);
            }
        }
        Ok(places)
    }

    /// The items of a resolved path whose owner the path names. The resolver
    /// finds a member by the last name of its type, so a member of another
    /// type with the same name does not count.
    fn owned_items(&self, resolved: &PathResolution) -> Vec<ItemId> {
        resolved
            .items
            .iter()
            .copied()
            .filter(|&item| match self.owner(item) {
                Owner::None => true,
                Owner::Known(owner) => resolved.through.contains(&owner),
                Owner::Unknown => false,
            })
            .collect()
    }

    fn owner(&self, item: ItemId) -> Owner {
        let Some(parent) = self.graph.items[item].parent else {
            return Owner::None;
        };
        if self.graph.items[parent].kind == ItemKind::Impl {
            return self
                .resolver
                .impl_self(parent)
                .map_or(Owner::Unknown, Owner::Known);
        }
        Owner::Known(parent)
    }

    /// The label of the denied place that a resolved path names, when the
    /// path names one.
    fn denied_target<'p>(&self, resolved: &PathResolution, deny: &'p Places) -> Option<&'p str> {
        self.owned_items(resolved)
            .into_iter()
            .find_map(|item| self.item_in(item, deny))
            .or_else(|| {
                resolved
                    .modules
                    .iter()
                    .find_map(|&module| self.module_in(module, deny))
            })
    }

    /// True when the path goes through a place of `via`.
    fn goes_via(&self, resolved: &PathResolution, via: &Places) -> bool {
        resolved
            .through
            .iter()
            .any(|&item| self.item_in(item, via).is_some())
    }

    /// True when the name `name` in `module` comes from an import of
    /// `settled` (an import with a finding or a path through `allow-via`), or
    /// from a module in `via`. The walk follows glob imports, for example
    /// `use super::*` in a test module.
    fn bound_through(
        &self,
        module: ModuleId,
        name: &str,
        via: &Places,
        settled: &FxHashSet<usize>,
        depth: usize,
    ) -> bool {
        if depth > MAX_GLOB_DEPTH {
            return false;
        }
        if self.module_in(module, via).is_some() {
            return true;
        }
        let named = self.index.named.get(&(module, name));
        if let Some(named) = named {
            return named.iter().any(|index| settled.contains(index));
        }
        self.index
            .globs
            .get(&module)
            .into_iter()
            .flatten()
            .any(|&index| {
                self.resolver
                    .glob_sources(index)
                    .0
                    .into_iter()
                    .filter(|&source| source != module)
                    .any(|source| self.bound_through(source, name, via, settled, depth + 1))
            })
    }

    /// The names of the denied items and of the members of a denied type. The
    /// items of a denied module do not count: a whole crate has too many
    /// common names, so the count would tell nothing.
    fn denied_names(&self, deny: &Places) -> FxHashSet<&str> {
        self.graph
            .items
            .iter()
            .enumerate()
            .filter(|(item, _)| self.member_of(*item, deny))
            .map(|(_, entry)| entry.name.as_str())
            .collect()
    }

    /// True when `item` or a type that holds it is an item of `places`.
    fn member_of(&self, item: ItemId, places: &Places) -> bool {
        let mut current = item;
        loop {
            if places.items.contains(&current) {
                return true;
            }
            match self.owner(current) {
                Owner::Known(owner) => current = owner,
                Owner::None | Owner::Unknown => return false,
            }
        }
    }

    /// The label of the place of `places` that holds `item`: the item, its
    /// owner type, or a module around it.
    fn item_in<'p>(&self, item: ItemId, places: &'p Places) -> Option<&'p str> {
        let mut current = item;
        loop {
            if places.items.contains(&current) {
                return places.labels.get(&Place::Item(current)).map(String::as_str);
            }
            match self.owner(current) {
                Owner::Known(owner) => current = owner,
                Owner::Unknown => return None,
                Owner::None => break,
            }
        }
        self.module_in(self.graph.items[current].module, places)
    }

    /// The label of the place of `places` that holds `module`.
    fn module_in<'p>(&self, module: ModuleId, places: &'p Places) -> Option<&'p str> {
        let mut current = Some(module);
        while let Some(id) = current {
            if places.modules.contains(&id) {
                return places.labels.get(&Place::Module(id)).map(String::as_str);
            }
            let entry = &self.graph.modules[id];
            if let Some(item) = entry.item
                && places.items.contains(&item)
            {
                return places.labels.get(&Place::Item(item)).map(String::as_str);
            }
            current = entry.parent;
        }
        None
    }
}

/// The owner of an item.
enum Owner {
    /// The item is at module level.
    None,
    /// The type, the trait or the enum that holds the item.
    Known(ItemId),
    /// The item is in an `impl` block whose type does not resolve.
    Unknown,
}

impl Places {
    fn insert(&mut self, place: Place, label: &str) {
        match place {
            Place::Item(item) => self.items.insert(item),
            Place::Module(module) => self.modules.insert(module),
        };
        self.labels.entry(place).or_insert_with(|| label.to_owned());
    }
}

/// The place of one use.
struct Use {
    krate: usize,
    file: graph::FileId,
    line: u32,
    origin: &'static str,
}
