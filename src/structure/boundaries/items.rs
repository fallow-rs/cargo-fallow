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
use crate::structure::boundaries::{Finding, Kind, Names, Origin as FindingOrigin, fill, invalid};
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
    /// The names of the items that each module declares.
    declared: FxHashSet<(ModuleId, &'g str)>,
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
        for item in &graph.items {
            if item.parent.is_none() && !matches!(item.kind, ItemKind::Use | ItemKind::Impl) {
                index.declared.insert((item.module, item.name.as_str()));
            }
        }
        index
    }
}

impl Context<'_> {
    /// The places of `allow-via`. Each one must be a module.
    fn via_places(&self, rule: &ItemRule) -> Result<Places, CliError> {
        let via = self.places(&rule.rule, &rule.allow_via, false)?;
        if let Some(value) = via.labels.iter().find_map(|(place, label)| match place {
            Place::Item(item) if self.graph.items[*item].child_module.is_none() => Some(label),
            _ => None,
        }) {
            return Err(invalid(&format!(
                "rule `{}` names `{value}` in `allow-via`, which is not a module; `allow-via` lists modules",
                rule.rule
            )));
        }
        Ok(via)
    }

    fn item_rule(
        &self,
        rule: &ItemRule,
        names: &Names<'_>,
        outcome: &mut Outcome,
    ) -> Result<(), CliError> {
        let from = names.expand(&rule.rule, &rule.from)?;
        let deny = self.places(&rule.rule, &rule.deny, true)?;
        let via = self.via_places(rule)?;
        let denied_names = self.denied_names(&deny);
        let checked = |krate: usize| from.contains(&self.graph.crates[krate].package);
        let mut found: BTreeMap<(String, u32, String), Finding> = BTreeMap::new();
        let settled = self.import_findings(rule, &checked, &deny, &via, &mut found);
        let scope = RuleScope {
            deny: &deny,
            via: &via,
            denied_names: &denied_names,
            settled: &settled,
        };
        for reference in &self.graph.refs {
            if !checked(reference.krate) || outside_code(reference.origin) {
                continue;
            }
            match self.reference_verdict(reference, &scope) {
                RefVerdict::Skip => {}
                RefVerdict::Unresolved => outcome.unresolved += 1,
                RefVerdict::Denied(to) => {
                    let use_site = Use {
                        krate: reference.krate,
                        file: reference.file,
                        line: reference.line,
                        origin: finding_origin(reference.origin),
                    };
                    self.record(rule, &use_site, to, &mut found);
                }
            }
        }
        outcome.findings.extend(found.into_values());
        Ok(())
    }

    /// Records a finding for each named import of a checked crate that
    /// resolves to a denied place. Returns the named imports with a finding
    /// or a path through `allow-via`. A path that starts with the name of
    /// such an import gives no second finding.
    fn import_findings(
        &self,
        rule: &ItemRule,
        checked: &impl Fn(usize) -> bool,
        deny: &Places,
        via: &Places,
        found: &mut BTreeMap<(String, u32, String), Finding>,
    ) -> FxHashSet<usize> {
        let mut settled: FxHashSet<usize> = FxHashSet::default();
        for (index, import) in self.graph.imports.iter().enumerate() {
            if !checked(import.krate) || import.binding == Binding::Glob {
                continue;
            }
            let resolved = self.resolver.resolve_import(index);
            if self.goes_via(&resolved, via) {
                settled.insert(index);
                continue;
            }
            if let Some(to) = self.denied_target(&resolved, deny) {
                settled.insert(index);
                let use_site = Use {
                    krate: import.krate,
                    file: import.file,
                    line: import.line,
                    origin: FindingOrigin::Code,
                };
                self.record(rule, &use_site, to, found);
            }
        }
        settled
    }

    /// Decides what one path, method call or field access gives for a rule.
    fn reference_verdict<'p>(
        &self,
        reference: &graph::Ref,
        scope: &RuleScope<'p>,
    ) -> RefVerdict<'p> {
        let (segments, global) = match &reference.kind {
            RefKind::Path { segments, global }
            | RefKind::RecordField {
                segments, global, ..
            } => (segments, *global),
            RefKind::Method(name) | RefKind::Field(name) | RefKind::Member(name) => {
                return RefVerdict::unresolved(scope.denied_names.contains(name.as_str()));
            }
            RefKind::Macro(_) => return RefVerdict::Skip,
        };
        let Some(first) = segments.first() else {
            return RefVerdict::Skip;
        };
        if !global
            && self.bound_through(
                reference.module,
                first,
                scope.via,
                scope.settled,
                &mut FxHashSet::default(),
            )
        {
            return RefVerdict::Skip;
        }
        let resolved = self
            .resolver
            .resolve(reference.krate, reference.module, segments, global);
        if !resolved.exact {
            let last = segments.last().map(String::as_str).unwrap_or_default();
            return RefVerdict::unresolved(scope.denied_names.contains(last));
        }
        // A guessed first name that resolves is outside the workspace.
        if resolved.guessed {
            return RefVerdict::Skip;
        }
        // A name that only a glob import brings in can be a local binding:
        // a variable, a parameter or a generic parameter shadows it, and
        // the graph does not see local bindings.
        if !global && self.glob_only(reference.krate, reference.module, first) {
            return RefVerdict::unresolved(self.denied_target(&resolved, scope.deny).is_some());
        }
        if self.goes_via(&resolved, scope.via) {
            return RefVerdict::Skip;
        }
        self.denied_target(&resolved, scope.deny)
            .map_or(RefVerdict::Skip, RefVerdict::Denied)
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
                || !self.exported_module(import.module)
            {
                continue;
            }
            let (modules, enums) = self.glob_targets(index);
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
            self.add_place(rule, value, items, &mut places)?;
        }
        Ok(places)
    }

    /// Resolves one path of a rule and adds its items and modules to `places`.
    fn add_place(
        &self,
        rule: &str,
        value: &str,
        items: bool,
        places: &mut Places,
    ) -> Result<(), CliError> {
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
            return Ok(());
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
            return Ok(());
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
        Ok(())
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
    /// `use super::*` in a test module. It visits each module one time.
    fn bound_through(
        &self,
        module: ModuleId,
        name: &str,
        via: &Places,
        settled: &FxHashSet<usize>,
        visited: &mut FxHashSet<ModuleId>,
    ) -> bool {
        if settled.is_empty() && via.is_empty() {
            return false;
        }
        if !visited.insert(module) {
            return false;
        }
        if self.module_in(module, via).is_some() {
            return true;
        }
        if let Some(named) = self.index.named.get(&(module, name)) {
            return named.iter().any(|index| settled.contains(index));
        }
        let globs = self.index.globs.get(&module).cloned().unwrap_or_default();
        globs.into_iter().any(|index| {
            self.glob_targets(index)
                .0
                .into_iter()
                .any(|source| self.bound_through(source, name, via, settled, visited))
        })
    }

    /// True when only a glob import of `module` can bring the name `first`
    /// in: the module declares no item with the name, no named import binds
    /// it, and it is no crate name.
    fn glob_only(&self, krate: usize, module: ModuleId, first: &str) -> bool {
        !matches!(first, "crate" | "self" | "super" | "Self")
            && self.index.globs.contains_key(&module)
            && !self.index.declared.contains(&(module, first))
            && !self.index.named.contains_key(&(module, first))
            && !self.graph.crates[krate]
                .externs
                .iter()
                .any(|entry| entry.name == first)
    }

    /// The modules and the enums that a glob import reads, when its path
    /// resolves to its end.
    fn glob_targets(&self, index: usize) -> (Vec<ModuleId>, Vec<ItemId>) {
        let resolved = self.resolver.resolve_import(index);
        if !resolved.exact || resolved.guessed {
            return (Vec::new(), Vec::new());
        }
        let mut modules = resolved.modules;
        let mut enums = Vec::new();
        for item in resolved.items {
            let entry = &self.graph.items[item];
            match (entry.child_module, entry.kind) {
                (Some(child), _) => modules.push(child),
                (None, ItemKind::Enum) => enums.push(item),
                _ => {}
            }
        }
        (modules, enums)
    }

    /// True when another crate can name the module: each module on the way
    /// from the crate root is `pub`.
    fn exported_module(&self, module: ModuleId) -> bool {
        let mut current = Some(module);
        while let Some(id) = current {
            let entry = &self.graph.modules[id];
            if entry
                .item
                .is_some_and(|item| self.graph.items[item].vis != Vis::Public)
            {
                return false;
            }
            current = entry.parent;
        }
        true
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
    fn is_empty(&self) -> bool {
        self.items.is_empty() && self.modules.is_empty()
    }

    fn insert(&mut self, place: Place, label: &str) {
        match place {
            Place::Item(item) => self.items.insert(item),
            Place::Module(module) => self.modules.insert(module),
        };
        self.labels.entry(place).or_insert_with(|| label.to_owned());
    }
}

/// The places of one item rule, and the named imports that its import pass
/// settled.
struct RuleScope<'p> {
    deny: &'p Places,
    via: &'p Places,
    denied_names: &'p FxHashSet<&'p str>,
    settled: &'p FxHashSet<usize>,
}

/// What one path, method call or field access gives for an item rule.
enum RefVerdict<'p> {
    /// The use gives nothing.
    Skip,
    /// The rule cannot judge the use, and the use can reach a denied item.
    Unresolved,
    /// The use reaches the denied place with this label.
    Denied(&'p str),
}

impl RefVerdict<'_> {
    /// [`RefVerdict::Unresolved`] when a denied item can match, else
    /// [`RefVerdict::Skip`].
    const fn unresolved(denied: bool) -> Self {
        if denied { Self::Unresolved } else { Self::Skip }
    }
}

/// True for a reference in a doc comment, a doc link or an attribute. An
/// item rule checks only code.
const fn outside_code(origin: Origin) -> bool {
    matches!(origin, Origin::Doc | Origin::DocLink | Origin::Attr)
}

/// The origin of a finding for a reference of this origin.
const fn finding_origin(origin: Origin) -> FindingOrigin {
    match origin {
        Origin::Macro | Origin::PublicMacro => FindingOrigin::Macro,
        _ => FindingOrigin::Code,
    }
}

/// The place of one use.
struct Use {
    krate: usize,
    file: graph::FileId,
    line: u32,
    origin: FindingOrigin,
}
