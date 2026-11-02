//! The visibility check on the module graph. This is an experiment for the M2
//! gap measurement, not a stable command.
//!
//! The model is the closed-world model of hawk: the workspace is the only user
//! of its libraries, unless a library is excluded.
//!
//! - `dead_public`: a `pub` item that the library exports, and that no
//!   production root and no test, bench, example or doctest reaches.
//! - `unnecessary_public`: an exported `pub` item that code reaches, but that
//!   no other crate uses. It can be `pub(crate)`.
//! - `unnecessary_restricted_visibility`: a `pub(crate)`, `pub(super)` or
//!   `pub(in path)` item that only its own module and its child modules use.
//!
//! Each rule gives credit when the syntax cannot decide. The `undecided` list
//! holds the findings that only an untyped credit stops: a method call, a field
//! access, or a name in macro tokens, in an attribute or in a doc code block.

pub mod report;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::graph::extract::{Binding, ItemKind, Origin, Vis};
use crate::graph::resolve::{Class, Credit, Resolver};
use crate::graph::{CrateKind, Graph, ItemId, ModuleId};

/// The options of the check.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The crate names whose public items are an external API.
    pub exclude: Vec<String>,
    /// True when the fields of a struct with one written visibility change as
    /// a group, as hawk's `preserve-uniform-field-visibility`.
    pub uniform_fields: bool,
}

/// The kind of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FindingKind {
    /// An exported `pub` item that nothing reaches.
    DeadPublic,
    /// An exported `pub` item that only its own crate uses.
    UnnecessaryPublic,
    /// A restricted item that only its own module uses.
    UnnecessaryRestricted,
}

impl FindingKind {
    /// The name in the report. The names are the same as in hawk.
    pub const fn label(self) -> &'static str {
        match self {
            Self::DeadPublic => "dead_public",
            Self::UnnecessaryPublic => "unnecessary_public",
            Self::UnnecessaryRestricted => "unnecessary_restricted_visibility",
        }
    }
}

/// One finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Finding {
    /// The kind.
    pub kind: FindingKind,
    /// The item.
    pub item: ItemId,
    /// True when only test code reaches the item.
    pub test_only: bool,
}

/// The result of the check.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    /// The findings, sorted by item.
    pub findings: Vec<Finding>,
    /// The findings that only untyped credits stop.
    pub undecided: Vec<Finding>,
}

/// Runs the check.
pub fn analyze(graph: &Graph, options: &Options) -> Analysis {
    let resolver = Resolver::new(graph);
    let credits = resolver.credits();
    let surface = Surface::new(graph, &resolver, options);
    let findings = decide(graph, &surface, &credits, options, |_| true);
    let typed = decide(graph, &surface, &credits, options, |credit| {
        credit.class != Class::Untyped
    });
    let known: FxHashSet<(ItemId, FindingKind)> = findings
        .iter()
        .map(|finding| (finding.item, finding.kind))
        .collect();
    let undecided = typed
        .into_iter()
        .filter(|finding| !known.contains(&(finding.item, finding.kind)))
        .collect();
    Analysis {
        findings,
        undecided,
    }
}

/// The exported surface of the libraries.
struct Surface {
    exported_modules: FxHashSet<ModuleId>,
    /// The `use` items that re-export one local item that is not a module.
    local_reexports: FxHashSet<ItemId>,
    /// The items that a `pub use` in an exported module names. The re-export
    /// is the candidate for the item, but the members of the item are
    /// candidates too.
    reexported: FxHashSet<ItemId>,
    glob_exported: FxHashSet<ItemId>,
    reexport_targets: FxHashMap<ItemId, Vec<ItemId>>,
    modules_with_reexports: FxHashSet<ModuleId>,
    excluded: FxHashSet<usize>,
    /// The types of each crate by name, for the self type of an `impl` block.
    types: FxHashMap<(usize, String), Vec<ItemId>>,
    /// The result of [`Surface::exported`] for each item.
    exported: Vec<bool>,
}

impl Surface {
    fn new(graph: &Graph, resolver: &Resolver<'_>, options: &Options) -> Self {
        let excluded: FxHashSet<usize> = graph
            .crates
            .iter()
            .enumerate()
            .filter(|(_, entry)| options.exclude.contains(&entry.name))
            .map(|(id, _)| id)
            .collect();
        let mut surface = Self {
            exported_modules: graph
                .crates
                .iter()
                .filter(|entry| entry.kind.is_library())
                .map(|entry| entry.root_module)
                .collect(),
            local_reexports: FxHashSet::default(),
            reexported: FxHashSet::default(),
            glob_exported: FxHashSet::default(),
            reexport_targets: FxHashMap::default(),
            modules_with_reexports: FxHashSet::default(),
            excluded,
            types: FxHashMap::default(),
            exported: Vec::new(),
        };
        for (id, entry) in graph.items.iter().enumerate() {
            let is_type = matches!(
                entry.kind,
                ItemKind::Struct | ItemKind::Enum | ItemKind::Union | ItemKind::TypeAlias
            );
            if is_type && entry.parent.is_none() {
                surface
                    .types
                    .entry((entry.krate, entry.name.clone()))
                    .or_default()
                    .push(id);
            }
        }
        loop {
            let before = surface.exported_modules.len();
            surface.grow(graph, resolver);
            if before == surface.exported_modules.len() {
                break;
            }
        }
        surface.exported = (0..graph.items.len())
            .map(|item| surface.compute_exported(graph, item))
            .collect();
        surface
    }

    fn grow(&mut self, graph: &Graph, resolver: &Resolver<'_>) {
        for (id, module) in graph.modules.iter().enumerate() {
            let public = module
                .item
                .is_some_and(|item| graph.items[item].vis == Vis::Public);
            if public
                && module
                    .parent
                    .is_some_and(|parent| self.exported_modules.contains(&parent))
            {
                self.exported_modules.insert(id);
            }
        }
        for (index, import) in graph.imports.iter().enumerate() {
            if import.vis != Vis::Public {
                continue;
            }
            self.modules_with_reexports.insert(import.module);
            if !self.exported_modules.contains(&import.module) {
                continue;
            }
            if import.binding == Binding::Glob {
                let (modules, enums) = resolver.glob_sources(index);
                for module in modules {
                    self.exported_modules.insert(module);
                    for (item, entry) in graph.items.iter().enumerate() {
                        if entry.module == module
                            && entry.parent.is_none()
                            && entry.vis == Vis::Public
                        {
                            self.glob_exported.insert(item);
                        }
                    }
                }
                self.glob_exported.extend(enums);
                continue;
            }
            let (items, modules) = resolver.import_items(index);
            self.reexported.extend(items.iter().copied());
            if let Some(own) = import.item {
                let local = !items.is_empty()
                    && modules.is_empty()
                    && items
                        .iter()
                        .all(|item| graph.items[*item].krate == import.krate);
                if local {
                    self.local_reexports.insert(own);
                }
                self.reexport_targets.insert(own, items);
            }
        }
    }

    /// True when another crate can name the item.
    fn exported(&self, item: ItemId) -> bool {
        self.exported.get(item).copied().unwrap_or(false)
    }

    fn compute_exported(&self, graph: &Graph, item: ItemId) -> bool {
        let entry = &graph.items[item];
        if entry.vis != Vis::Public && entry.kind != ItemKind::Variant {
            return false;
        }
        match entry.parent.map(|parent| (parent, &graph.items[parent])) {
            None if entry.kind == ItemKind::Use => {
                self.exported_modules.contains(&entry.module)
                    && self.local_reexports.contains(&item)
            }
            None => self.exported_modules.contains(&entry.module),
            Some((parent, owner)) => match owner.kind {
                ItemKind::Impl => {
                    !owner.impl_trait && self.type_exported(graph, owner, entry.krate)
                }
                ItemKind::Trait => false,
                _ => self.reachable(graph, parent),
            },
        }
    }

    /// True when another crate can reach the item by a path: through its
    /// module chain or through a re-export.
    fn reachable(&self, graph: &Graph, item: ItemId) -> bool {
        self.reexported.contains(&item) || self.compute_exported(graph, item)
    }

    fn type_exported(&self, graph: &Graph, block: &crate::graph::Item, krate: usize) -> bool {
        let Some(name) = block.impl_self.as_ref().and_then(|path| path.last()) else {
            return false;
        };
        self.types
            .get(&(krate, name.clone()))
            .into_iter()
            .flatten()
            .any(|&id| self.reachable(graph, id))
    }

    /// True when the item is a `pub` candidate of a library.
    fn public_candidate(&self, graph: &Graph, item: ItemId) -> bool {
        let entry = &graph.items[item];
        let krate = &graph.crates[entry.krate];
        if krate.kind != CrateKind::Lib || self.excluded.contains(&entry.krate) {
            return false;
        }
        if entry.flags.macro_export || matches!(entry.kind, ItemKind::Impl | ItemKind::Macro) {
            return false;
        }
        if self.glob_exported.contains(&item) {
            return false;
        }
        if entry.kind == ItemKind::Module
            && entry
                .child_module
                .is_some_and(|module| self.modules_with_reexports.contains(&module))
        {
            return false;
        }
        if entry.test_only && entry.vis == Vis::Public && !matches!(entry.kind, ItemKind::Use) {
            return true;
        }
        self.exported(item)
    }
}

/// The facts of one item, from the credits.
#[derive(Debug, Clone, Default)]
struct Uses {
    cross_crate: bool,
    from_modules: Vec<ModuleId>,
    /// True when the signature of an item with a written visibility names the
    /// item. A private type in that signature makes rustc warn.
    exposed: bool,
}

fn decide(
    graph: &Graph,
    surface: &Surface,
    credits: &[Credit],
    options: &Options,
    keep: impl Fn(&Credit) -> bool,
) -> Vec<Finding> {
    let credits: Vec<&Credit> = credits.iter().filter(|credit| keep(credit)).collect();
    let mut uses: Vec<Uses> = vec![Uses::default(); graph.items.len()];
    for credit in &credits {
        let target = &graph.items[credit.item];
        let entry = &mut uses[credit.item];
        if credit.origin == Origin::Signature
            && let Some(owner) = credit.owner
            && owner != credit.item
            && graph.items[owner].vis != Vis::Private
        {
            entry.exposed = true;
        }
        let from_macro_export = credit
            .owner
            .is_some_and(|owner| graph.items[owner].flags.macro_export);
        if credit.krate != target.krate || credit.origin == Origin::Doc || from_macro_export {
            entry.cross_crate = true;
        } else if credit.owner != Some(credit.item) {
            entry.from_modules.push(credit.module);
        }
    }
    let required = required_items(graph, surface, &credits, &uses);
    let (production, any) = live_items(graph, &credits);
    let live_modules = modules_with_live_items(graph, &any);
    let mut findings = Vec::new();
    for (item, entry) in graph.items.iter().enumerate() {
        let test_only = !production.contains(&item) && any.contains(&item);
        if surface.public_candidate(graph, item) && !required.contains(&item) {
            let live = any.contains(&item)
                || graph.items[item]
                    .child_module
                    .is_some_and(|module| live_modules.contains(&module));
            let kind = if live {
                FindingKind::UnnecessaryPublic
            } else {
                FindingKind::DeadPublic
            };
            if !(entry.kind == ItemKind::Variant && kind == FindingKind::UnnecessaryPublic) {
                findings.push(Finding {
                    kind,
                    item,
                    test_only,
                });
            }
        }
        let excluded = surface.excluded.contains(&entry.krate);
        if !excluded
            && restricted_candidate(entry)
            && !uses[item].exposed
            && only_own_module(graph, item, &uses[item])
        {
            findings.push(Finding {
                kind: FindingKind::UnnecessaryRestricted,
                item,
                test_only,
            });
        }
    }
    if options.uniform_fields {
        keep_field_groups(graph, &mut findings);
    }
    findings.sort();
    findings
}

fn restricted_candidate(entry: &crate::graph::Item) -> bool {
    entry.vis.is_restricted()
        && !matches!(
            entry.kind,
            ItemKind::Use | ItemKind::Impl | ItemKind::Variant | ItemKind::Macro
        )
        && !entry.flags.trait_impl
}

/// True when each use of the item is in the module of the item or below it.
fn only_own_module(graph: &Graph, item: ItemId, uses: &Uses) -> bool {
    let home = graph.items[item].module;
    uses.from_modules.iter().all(|&from| {
        let mut current = Some(from);
        while let Some(module) = current {
            if module == home {
                return true;
            }
            current = graph.modules[module].parent;
        }
        false
    })
}

/// The modules that hold a live item, with their parent modules. A module
/// counts as live when an item in it is live or when code names it.
fn modules_with_live_items(graph: &Graph, live: &FxHashSet<ItemId>) -> FxHashSet<ModuleId> {
    let mut modules = FxHashSet::default();
    for &item in live {
        let mut current = Some(graph.items[item].module);
        while let Some(id) = current {
            if !modules.insert(id) {
                break;
            }
            current = graph.modules[id].parent;
        }
    }
    modules
}

/// The items that must stay `pub`: another crate uses them, a required
/// signature names them, or a required item is inside them.
fn required_items(
    graph: &Graph,
    surface: &Surface,
    credits: &[&Credit],
    uses: &[Uses],
) -> FxHashSet<ItemId> {
    let mut signature: FxHashMap<ItemId, Vec<ItemId>> = FxHashMap::default();
    for credit in credits {
        if let (Some(owner), Origin::Signature) = (credit.owner, credit.origin) {
            signature.entry(owner).or_default().push(credit.item);
        }
    }
    let mut required: FxHashSet<ItemId> = FxHashSet::default();
    let mut queue: Vec<ItemId> = (0..graph.items.len())
        .filter(|&item| uses[item].cross_crate || surface.glob_exported.contains(&item))
        .collect();
    queue.extend(surface.excluded_surface(graph));
    while let Some(item) = queue.pop() {
        if !required.insert(item) {
            continue;
        }
        queue.extend(signature.get(&item).into_iter().flatten().copied());
        queue.extend(
            surface
                .reexport_targets
                .get(&item)
                .into_iter()
                .flatten()
                .copied(),
        );
        let mut module = Some(graph.items[item].module);
        while let Some(id) = module {
            queue.extend(graph.modules[id].item);
            module = graph.modules[id].parent;
        }
        queue.extend(graph.items[item].parent);
    }
    required
}

impl Surface {
    /// The exported items of the excluded crates. They are an external API.
    fn excluded_surface(&self, graph: &Graph) -> Vec<ItemId> {
        (0..graph.items.len())
            .filter(|&item| self.excluded.contains(&graph.items[item].krate))
            .filter(|&item| self.exported(item))
            .collect()
    }
}

/// The items that a production root reaches, and the items that any root
/// reaches.
fn live_items(graph: &Graph, credits: &[&Credit]) -> (FxHashSet<ItemId>, FxHashSet<ItemId>) {
    let mut edges: FxHashMap<ItemId, Vec<(ItemId, bool)>> = FxHashMap::default();
    let mut production_roots = Vec::new();
    let mut other_roots = Vec::new();
    for credit in credits {
        if credit.import {
            continue;
        }
        match (credit.owner, credit.origin) {
            (_, Origin::Doc) => other_roots.push(credit.item),
            (Some(owner), _) => edges
                .entry(owner)
                .or_default()
                .push((credit.item, credit_test_only(graph, credit))),
            (None, _) => production_roots.push(credit.item),
        }
    }
    for (item, entry) in graph.items.iter().enumerate() {
        let krate = &graph.crates[entry.krate];
        let main = entry.name == "main"
            && entry.kind == ItemKind::Function
            && entry.parent.is_none()
            && entry.module == krate.root_module;
        let derived_field = entry.kind == ItemKind::Field
            && entry.parent.is_some_and(|parent| derived(graph, parent));
        if (main && krate.production)
            || entry.flags.external
            || entry.flags.allow_dead
            || entry.flags.trait_impl
            || entry.flags.trait_item
            || entry.flags.proc_macro
            || derived_field
        {
            production_roots.push(item);
        } else if main || entry.flags.test_fn {
            other_roots.push(item);
        }
    }
    let production = reach(&edges, production_roots.iter().copied(), false);
    let mut all_roots = production_roots;
    all_roots.extend(other_roots);
    let any = reach(&edges, all_roots.into_iter(), true);
    (production, any)
}

fn derived(graph: &Graph, item: ItemId) -> bool {
    let mut current = Some(item);
    while let Some(id) = current {
        if graph.items[id].flags.derive {
            return true;
        }
        current = graph.items[id].parent;
    }
    false
}

fn credit_test_only(graph: &Graph, credit: &Credit) -> bool {
    credit.test_only
        || credit
            .owner
            .is_some_and(|owner| graph.items[owner].test_only || graph.items[owner].flags.test_fn)
}

fn reach(
    edges: &FxHashMap<ItemId, Vec<(ItemId, bool)>>,
    roots: impl Iterator<Item = ItemId>,
    with_tests: bool,
) -> FxHashSet<ItemId> {
    let mut seen = FxHashSet::default();
    let mut queue: Vec<ItemId> = roots.collect();
    while let Some(item) = queue.pop() {
        if !seen.insert(item) {
            continue;
        }
        for &(next, test_only) in edges.get(&item).into_iter().flatten() {
            if with_tests || !test_only {
                queue.push(next);
            }
        }
    }
    seen
}

/// With uniform field groups, the fields of one struct with one written
/// visibility change together. When one field must keep its visibility, the
/// other fields of the group keep it too.
fn keep_field_groups(graph: &Graph, findings: &mut Vec<Finding>) {
    let mut groups: FxHashMap<ItemId, Vec<ItemId>> = FxHashMap::default();
    for (item, entry) in graph.items.iter().enumerate() {
        if entry.kind == ItemKind::Field
            && let Some(parent) = entry.parent
        {
            groups.entry(parent).or_default().push(item);
        }
    }
    let reduced: FxHashSet<ItemId> = findings
        .iter()
        .filter(|finding| finding.kind != FindingKind::DeadPublic)
        .map(|finding| finding.item)
        .collect();
    let dead: FxHashSet<ItemId> = findings
        .iter()
        .filter(|finding| finding.kind == FindingKind::DeadPublic)
        .map(|finding| finding.item)
        .collect();
    let mut blocked = FxHashSet::default();
    for fields in groups.values() {
        let uniform = fields
            .windows(2)
            .all(|pair| graph.items[pair[0]].vis == graph.items[pair[1]].vis);
        if !uniform || graph.items[fields[0]].vis == Vis::Private {
            continue;
        }
        let all_reduced = fields
            .iter()
            .all(|field| reduced.contains(field) || dead.contains(field));
        let any_cfg = fields
            .iter()
            .any(|field| graph.items[*field].under_cfg || graph.items[*field].flags.allow_dead);
        if !all_reduced || any_cfg {
            blocked.extend(fields.iter().copied());
        }
    }
    findings.retain(|finding| {
        finding.kind == FindingKind::DeadPublic || !blocked.contains(&finding.item)
    });
}
