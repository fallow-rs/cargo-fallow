//! The visibility check on the module graph.
//!
//! The model is the closed-world model of hawk: the workspace is the only user
//! of its libraries. Two kinds of library are an exception: a library that a
//! registry can publish, and a library that `exclude-crates` names. Their
//! exported items are an external API, so they are entry points.
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

pub mod config;
pub mod precise;
pub mod report;
pub mod trace;

use std::path::PathBuf;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::graph::extract::{Binding, ItemFlags, ItemKind, Origin, RefKind, Vis};
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
    /// The workspace root, for the files of the build scripts.
    pub root: PathBuf,
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
    /// Each kind.
    #[cfg(test)]
    pub const ALL: [Self; 3] = [
        Self::DeadPublic,
        Self::UnnecessaryPublic,
        Self::UnnecessaryRestricted,
    ];

    /// The rule id in the report: the hawk name in kebab-case.
    pub const fn label(self) -> &'static str {
        match self {
            Self::DeadPublic => "dead-public",
            Self::UnnecessaryPublic => "unnecessary-public",
            Self::UnnecessaryRestricted => "unnecessary-restricted-visibility",
        }
    }

    /// The kind with this report name. The snake_case form of hawk, for
    /// example `dead_public`, names the same kind.
    pub fn from_label(label: &str) -> Option<Self> {
        let label = label.replace('_', "-");
        [
            Self::DeadPublic,
            Self::UnnecessaryPublic,
            Self::UnnecessaryRestricted,
        ]
        .into_iter()
        .find(|kind| kind.label() == label)
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Analysis {
    /// The findings, sorted by item.
    pub findings: Vec<Finding>,
    /// The findings that only untyped credits stop.
    pub undecided: Vec<Finding>,
    /// The items with a use in code under a `cfg` other than a test condition.
    /// rust-analyzer does not see such code when the condition is off, so
    /// `--precise` mode does not decide these items. Sorted.
    pub cfg_used: Vec<ItemId>,
    /// The undecided candidates that code the graph cannot read can use: the
    /// output of an `include!` with a path that is not a literal, for example
    /// a file in `OUT_DIR`. rust-analyzer does not search those files either,
    /// so `--precise` mode does not decide these candidates. Sorted.
    pub opaque: Vec<Finding>,
}

impl Analysis {
    /// Keeps only the findings and candidates of these kinds. An empty list
    /// keeps each kind.
    pub fn retain_kinds(&mut self, kinds: &[FindingKind]) {
        if kinds.is_empty() {
            return;
        }
        self.findings
            .retain(|finding| kinds.contains(&finding.kind));
        self.undecided
            .retain(|finding| kinds.contains(&finding.kind));
        self.opaque.retain(|finding| kinds.contains(&finding.kind));
    }

    /// Keeps only the findings and candidates of items in a file of `scope`,
    /// and the findings that the baseline of `scope` does not hold. Call it
    /// after [`refine`] and [`Self::retain_kinds`]: a re-export finding
    /// outside the scope still holds back its target.
    pub fn retain_scope(&mut self, graph: &Graph, scope: &crate::scope::Scope) {
        let in_files = |finding: &Finding| scope.keeps(&graph.items[finding.item].file);
        self.findings.retain(|finding| {
            let item = &graph.items[finding.item];
            in_files(finding)
                && !scope.hides(
                    "visibility",
                    finding.kind.label(),
                    &item.file,
                    Some(&(item.line..=item.line)),
                )
                && scope.keeps_entry_of(|| report::baseline_entry(graph, finding))
        });
        self.undecided.retain(in_files);
        self.opaque.retain(in_files);
    }
}

/// The options of the analysis for the resolved command options.
pub fn check_options(options: &crate::cli::VisibilityOptions, root: PathBuf) -> Options {
    Options {
        exclude: options.exclude.clone(),
        uniform_fields: options.uniform_fields,
        root,
    }
}

/// The report of a fast-mode run with the resolved command options.
pub fn fast_report(
    graph: &Graph,
    options: &crate::cli::VisibilityOptions,
    workspace: &crate::workspace::Workspace,
    scope: &crate::scope::Scope,
) -> report::VisibilityReport {
    let check_options = check_options(options, workspace.root.clone().into_std_path_buf());
    let mut analysis = analyze(graph, &check_options);
    skip_generated(graph, &mut analysis);
    analysis.retain_kinds(&options.kinds);
    analysis.retain_scope(graph, scope);
    let mut report = report::VisibilityReport::new(report::Inputs {
        graph,
        analysis: &analysis,
        precise: None,
        enforced: options.fail_on_issues,
    });
    report.regate(scope.rules());
    report
}

/// Runs the check.
pub fn analyze(graph: &Graph, options: &Options) -> Analysis {
    let resolver = Resolver::new(graph);
    let credits = resolver.credits();
    let surface = Surface::new(graph, &resolver, options);
    let findings = decide(graph, &surface, &credits, options, |_| true);
    // A doc block credit and a doc link credit stay: a type-aware check of
    // the crate cannot see the doc tests and the doc links either.
    let typed = decide(graph, &surface, &credits, options, |credit| {
        credit.class != Class::Untyped || matches!(credit.origin, Origin::Doc | Origin::DocLink)
    });
    let known: FxHashSet<(ItemId, FindingKind)> = findings
        .iter()
        .map(|finding| (finding.item, finding.kind))
        .collect();
    let undecided = typed
        .into_iter()
        .filter(|finding| !known.contains(&(finding.item, finding.kind)))
        .collect();
    let mut cfg_used: Vec<ItemId> = credits
        .iter()
        .filter(|credit| credit.under_cfg)
        .map(|credit| credit.item)
        .collect();
    cfg_used.sort_unstable();
    cfg_used.dedup();
    let mut analysis = Analysis {
        findings,
        undecided,
        cfg_used,
        opaque: Vec::new(),
    };
    hold_opaque(graph, options, &mut analysis);
    analysis
}

/// Removes the findings and candidates in a generated file. The generator
/// writes the visibility, so a change by hand does not last. Call it last: a
/// re-export finding in a generated file still holds back its target in
/// [`refine`].
pub fn skip_generated(graph: &Graph, analysis: &mut Analysis) {
    if graph.generated_files.is_empty() {
        return;
    }
    let keep = |finding: &Finding| {
        !graph
            .generated_files
            .contains(&graph.items[finding.item].file)
    };
    analysis.findings.retain(keep);
    analysis.undecided.retain(keep);
    analysis.opaque.retain(keep);
}

/// How far code that the graph cannot read reaches into a crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// The code can name each `pub` item of the crate.
    Public,
    /// The code is in the crate, so it can name each item.
    All,
}

/// The crates that code from an `include!` with a path that the graph cannot
/// read can name. The code is in the crate of the call, so that crate is open
/// for each kind. The code can name the `pub` items of each dependency. When
/// the package has a build script, the generated code can name only the crates
/// that the build script text names as `name::`, as in the `deps` check.
fn opaque_crates(graph: &Graph, options: &Options) -> FxHashMap<usize, Reach> {
    let including: FxHashSet<usize> = graph
        .refs
        .iter()
        .filter(|reference| matches!(&reference.kind, RefKind::Macro(name) if name == "include"))
        .map(|reference| reference.krate)
        .collect();
    let mut open = FxHashMap::default();
    if including.is_empty() {
        return open;
    }
    let generated = crate::deps::usage::build_script_names(graph, &options.root);
    for krate in including {
        open.insert(krate, Reach::All);
        let entry = &graph.crates[krate];
        let names = generated.get(&entry.package);
        for dependency in &entry.externs {
            let Some(target) = dependency.target else {
                continue;
            };
            if names.is_none_or(|names| names.contains(&dependency.name)) {
                open.entry(target).or_insert(Reach::Public);
            }
        }
    }
    open
}

/// Moves each finding that code the graph cannot read can make wrong to the
/// undecided list, and records those candidates in [`Analysis::opaque`].
fn hold_opaque(graph: &Graph, options: &Options, analysis: &mut Analysis) {
    let open = opaque_crates(graph, options);
    if open.is_empty() {
        return;
    }
    let is_opaque = |finding: &Finding| match open.get(&graph.items[finding.item].krate) {
        Some(Reach::All) => true,
        Some(Reach::Public) => finding.kind != FindingKind::UnnecessaryRestricted,
        None => false,
    };
    let (held, kept): (Vec<Finding>, Vec<Finding>) = analysis
        .findings
        .drain(..)
        .partition(|finding| is_opaque(finding));
    analysis.findings = kept;
    analysis.undecided.extend(held);
    analysis.undecided.sort();
    analysis.undecided.dedup();
    analysis.opaque = analysis
        .undecided
        .iter()
        .copied()
        .filter(|finding| is_opaque(finding))
        .collect();
}

/// Runs the filters that keep each reported change valid alone, again, after
/// `--precise` mode added findings: a re-export before its target, field
/// groups, and files in more than one crate.
pub fn refine(graph: &Graph, options: &Options, analysis: &mut Analysis) {
    let resolver = Resolver::new(graph);
    let surface = Surface::new(graph, &resolver, options);
    wait_for_reexports(graph, &surface, &mut analysis.findings);
    if options.uniform_fields {
        keep_field_groups(graph, &surface, &mut analysis.findings);
    }
    keep_shared_files(graph, &mut analysis.findings);
    analysis.findings.sort();
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
    /// The library crates whose exported items are an external API: the
    /// excluded crates and the libraries that a registry can publish.
    api: FxHashSet<usize>,
    /// The types of each crate by name, for the self type of an `impl` block.
    types: FxHashMap<(usize, String), Vec<ItemId>>,
    /// The result of [`Surface::exported`] for each item.
    exported: Vec<bool>,
    /// True for each item that an attribute macro covers: on the item, on an
    /// enclosing item or on an enclosing module. The output of the macro can
    /// use the item from any place, and the macro can need its visibility.
    macro_covered: Vec<bool>,
    /// True for each item that an `allow(dead_code)` covers, also through an
    /// enclosing item or module.
    dead_allowed: Vec<bool>,
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
        let api: FxHashSet<usize> = graph
            .crates
            .iter()
            .enumerate()
            .filter(|(id, entry)| {
                excluded.contains(id) || (entry.published && entry.kind.is_library())
            })
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
            api,
            types: FxHashMap::default(),
            exported: Vec::new(),
            macro_covered: Vec::new(),
            dead_allowed: Vec::new(),
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
        surface.macro_covered = inherited(graph, |flags| flags.attr_macro, |_| false);
        surface.dead_allowed =
            inherited(graph, |flags| flags.allow_dead, |module| module.allow_dead);
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
        if krate.kind != CrateKind::Lib || self.api.contains(&entry.krate) {
            return false;
        }
        if entry.flags.macro_export
            || self.macro_covered[item]
            || matches!(entry.kind, ItemKind::Impl | ItemKind::Macro)
        {
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
            && signature_visible(graph, owner)
        {
            entry.exposed = true;
        }
        let from_macro_export = credit
            .owner
            .is_some_and(|owner| graph.items[owner].flags.macro_export);
        if credit.krate != target.krate || credit.origin == Origin::Doc || from_macro_export {
            entry.cross_crate = true;
        } else if credit.owner != Some(credit.item) {
            // A `macro_rules!` body expands at each call site, in any module.
            let in_macro = credit
                .owner
                .is_some_and(|owner| graph.items[owner].kind == ItemKind::Macro);
            entry.from_modules.push(if in_macro {
                graph.crates[credit.krate].root_module
            } else {
                credit.module
            });
        }
    }
    let (production, any) = live_items(graph, surface, &credits);
    let live_modules = modules_with_live_items(graph, &any);
    let facts = Facts {
        uses: &uses,
        production: &production,
        any: &any,
        live_modules: &live_modules,
    };
    // A filter can keep a candidate `pub`. The candidate then holds the types
    // of its signature, so the findings are computed again with it held.
    let mut held: FxHashSet<ItemId> = FxHashSet::default();
    loop {
        let required = required_items(graph, surface, &credits, &uses, &held);
        let findings = judge(graph, surface, options, &facts, &required);
        let public: FxHashSet<ItemId> = findings
            .iter()
            .filter(|finding| finding.kind != FindingKind::UnnecessaryRestricted)
            .map(|finding| finding.item)
            .collect();
        let kept: Vec<ItemId> = (0..graph.items.len())
            .filter(|&item| {
                graph.items[item].kind != ItemKind::Variant
                    && !required.contains(&item)
                    && !public.contains(&item)
                    && !held.contains(&item)
                    && surface.public_candidate(graph, item)
            })
            .collect();
        if kept.is_empty() {
            return findings;
        }
        held.extend(kept);
    }
}

/// The facts of [`decide`] that do not change when more items are held.
struct Facts<'a> {
    uses: &'a [Uses],
    production: &'a FxHashSet<ItemId>,
    any: &'a FxHashSet<ItemId>,
    live_modules: &'a FxHashSet<ModuleId>,
}

/// The findings for one set of required items, after the filters that keep
/// each change valid alone.
fn judge(
    graph: &Graph,
    surface: &Surface,
    options: &Options,
    facts: &Facts<'_>,
    required: &FxHashSet<ItemId>,
) -> Vec<Finding> {
    let Facts {
        uses,
        production,
        any,
        live_modules,
    } = *facts;
    let mut findings = Vec::new();
    for (item, entry) in graph.items.iter().enumerate() {
        let test_only = !production.contains(&item) && any.contains(&item);
        // A consumer path to a required item can go through the re-export,
        // and the syntax cannot show which path it took.
        let reexports_required = entry.kind == ItemKind::Use
            && surface
                .reexport_targets
                .get(&item)
                .is_some_and(|targets| targets.iter().any(|target| required.contains(target)));
        if surface.public_candidate(graph, item) && !required.contains(&item) && !reexports_required
        {
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
            && !surface.macro_covered[item]
            && !uses[item].exposed
            && !uses[item].cross_crate
            && only_own_module(graph, item, &uses[item])
        {
            findings.push(Finding {
                kind: FindingKind::UnnecessaryRestricted,
                item,
                test_only,
            });
        }
    }
    wait_for_reexports(graph, surface, &mut findings);
    if options.uniform_fields {
        keep_field_groups(graph, surface, &mut findings);
    }
    keep_shared_files(graph, &mut findings);
    findings.sort();
    findings
}

/// True when the signature of `owner` has a visibility wider than private. An
/// item of a trait impl and an enum variant have no keyword, but the compiler
/// checks their signatures against the type, the trait and the enum.
fn signature_visible(graph: &Graph, owner: ItemId) -> bool {
    let mut current = Some(owner);
    while let Some(id) = current {
        let entry = &graph.items[id];
        if entry.flags.trait_impl {
            return true;
        }
        match entry.kind {
            // A trait item has the visibility of its trait.
            _ if entry.flags.trait_item => {}
            ItemKind::Variant => {}
            ItemKind::Field
                if entry
                    .parent
                    .is_some_and(|parent| graph.items[parent].kind == ItemKind::Variant) => {}
            _ => return entry.vis != Vis::Private,
        }
        current = entry.parent;
    }
    false
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
///
/// An item in `held` keeps `pub`, but no other crate needs a path to it, for
/// example an item that an attribute macro covers. It needs no enclosing item,
/// only the types of its signature and the targets of its doc links. Each
/// exported item that is no candidate is held too.
fn required_items(
    graph: &Graph,
    surface: &Surface,
    credits: &[&Credit],
    uses: &[Uses],
    held: &FxHashSet<ItemId>,
) -> FxHashSet<ItemId> {
    // rustdoc warns when the documentation of an item that stays `pub` links
    // to a less visible item, so a doc link holds its target as a signature
    // does. The inner documentation of a file belongs to its `mod` item. The
    // documentation of a library root is always public.
    let mut signature: FxHashMap<ItemId, Vec<ItemId>> = FxHashMap::default();
    let mut work: Vec<Work> = Vec::new();
    for credit in credits {
        // The output of a macro can hold a `pub` item that names the type,
        // and the syntax cannot show its signature.
        if credit.origin == Origin::PublicMacro && is_type(graph.items[credit.item].kind) {
            work.push(Work::Require(credit.item));
        }
        if !matches!(credit.origin, Origin::Signature | Origin::DocLink) {
            continue;
        }
        let owner = credit.owner.or_else(|| {
            (credit.origin == Origin::DocLink)
                .then(|| graph.modules[credit.module].item)
                .flatten()
        });
        let Some(owner) = owner else {
            let root = graph.crates[credit.krate].root_module == credit.module;
            if credit.origin == Origin::DocLink
                && root
                && graph.crates[credit.krate].kind.is_library()
            {
                work.push(Work::Require(credit.item));
            }
            continue;
        };
        signature.entry(owner).or_default().push(credit.item);
        // A field of a variant is as visible as its enum, so a required
        // enum needs the types of its variant fields.
        if let Some(enumeration) = variant_enum(graph, owner) {
            signature.entry(enumeration).or_default().push(credit.item);
        }
    }
    let members = members_of_types(graph);
    work.extend(
        (0..graph.items.len())
            .filter(|&item| uses[item].cross_crate || surface.glob_exported.contains(&item))
            .map(Work::Require),
    );
    work.extend(surface.api_surface(graph).into_iter().map(Work::Require));
    work.extend(
        (0..graph.items.len())
            .filter(|&item| held.contains(&item) || surface.stays_exported(graph, item))
            .map(Work::Hold),
    );
    let mut required: FxHashSet<ItemId> = FxHashSet::default();
    let mut holding: FxHashSet<ItemId> = FxHashSet::default();
    while let Some(next) = work.pop() {
        let item = match next {
            Work::Require(item) if required.insert(item) => item,
            Work::Hold(item) if !required.contains(&item) && holding.insert(item) => item,
            _ => continue,
        };
        work.extend(
            signature
                .get(&item)
                .into_iter()
                .flatten()
                .copied()
                .map(Work::Require),
        );
        // rustc checks the members of a type that stays `pub` against the
        // visibility of the type. A member that is no candidate keeps its
        // visibility, so it holds the types of its signature.
        work.extend(
            members
                .get(&item)
                .into_iter()
                .flatten()
                .copied()
                .filter(|&member| keeps_visibility(graph, surface, member))
                .map(Work::Hold),
        );
        if matches!(next, Work::Hold(_)) {
            continue;
        }
        work.extend(
            surface
                .reexport_targets
                .get(&item)
                .into_iter()
                .flatten()
                .copied()
                .map(Work::Require),
        );
        let mut module = Some(graph.items[item].module);
        while let Some(id) = module {
            work.extend(graph.modules[id].item.map(Work::Require));
            module = graph.modules[id].parent;
        }
        work.extend(graph.items[item].parent.map(Work::Require));
    }
    required
}

/// True for a kind that can be a type in a signature.
const fn is_type(kind: ItemKind) -> bool {
    matches!(
        kind,
        ItemKind::Struct | ItemKind::Enum | ItemKind::Union | ItemKind::Trait | ItemKind::TypeAlias
    )
}

/// One step of [`required_items`].
#[derive(Debug, Clone, Copy)]
enum Work {
    /// The item must stay `pub`, with a path from outside.
    Require(ItemId),
    /// The item keeps `pub`, but needs no path.
    Hold(ItemId),
}

/// True when a member of a type keeps its visibility when the type stays
/// `pub`: it has no keyword, as the items of a trait or a trait impl and a
/// variant, or it is `pub` and no candidate of a finding.
fn keeps_visibility(graph: &Graph, surface: &Surface, member: ItemId) -> bool {
    let entry = &graph.items[member];
    let in_variant = entry
        .parent
        .is_some_and(|parent| graph.items[parent].kind == ItemKind::Variant);
    entry.flags.trait_impl
        || entry.flags.trait_item
        || in_variant
        || matches!(entry.kind, ItemKind::Variant | ItemKind::Impl)
        || (entry.vis == Vis::Public && !surface.public_candidate(graph, member))
}

/// The enum of a variant field, or `None` for another item.
fn variant_enum(graph: &Graph, field: ItemId) -> Option<ItemId> {
    let entry = &graph.items[field];
    if entry.kind != ItemKind::Field {
        return None;
    }
    let variant = entry.parent?;
    if graph.items[variant].kind != ItemKind::Variant {
        return None;
    }
    graph.items[variant].parent
}

/// The members of each item: the fields of a struct, the variants of an enum,
/// the fields of a variant and the items of a trait. A type also has each
/// `impl` block with the type as self type, and the items of those blocks.
fn members_of_types(graph: &Graph) -> FxHashMap<ItemId, Vec<ItemId>> {
    let mut types: FxHashMap<(usize, &str), Vec<ItemId>> = FxHashMap::default();
    for (id, entry) in graph.items.iter().enumerate() {
        let is_type = matches!(
            entry.kind,
            ItemKind::Struct | ItemKind::Enum | ItemKind::Union
        );
        if is_type && entry.parent.is_none() {
            types
                .entry((entry.krate, entry.name.as_str()))
                .or_default()
                .push(id);
        }
    }
    let types_of = |block: &crate::graph::Item| -> Vec<ItemId> {
        block
            .impl_self
            .as_ref()
            .and_then(|path| path.last())
            .and_then(|name| types.get(&(block.krate, name.as_str())))
            .cloned()
            .unwrap_or_default()
    };
    let mut members: FxHashMap<ItemId, Vec<ItemId>> = FxHashMap::default();
    for (id, entry) in graph.items.iter().enumerate() {
        if entry.kind == ItemKind::Impl {
            for ty in types_of(entry) {
                members.entry(ty).or_default().push(id);
            }
        }
        let Some(parent) = entry.parent else {
            continue;
        };
        let owner = &graph.items[parent];
        if owner.kind == ItemKind::Impl {
            for ty in types_of(owner) {
                members.entry(ty).or_default().push(id);
            }
        } else {
            members.entry(parent).or_default().push(id);
        }
    }
    members
}

impl Surface {
    /// True when the item is exported and keeps `pub`, because it is no
    /// candidate of a public finding: for example an attribute macro covers
    /// it. Its signature then needs each type that it names.
    fn stays_exported(&self, graph: &Graph, item: ItemId) -> bool {
        let entry = &graph.items[item];
        entry.vis == Vis::Public
            && !matches!(entry.kind, ItemKind::Use | ItemKind::Impl)
            && graph.crates[entry.krate].kind.is_library()
            && self.exported(item)
            && !self.public_candidate(graph, item)
    }

    /// The exported items of the API crates. They are an external API.
    fn api_surface(&self, graph: &Graph) -> Vec<ItemId> {
        (0..graph.items.len())
            .filter(|&item| self.api.contains(&graph.items[item].krate))
            .filter(|&item| self.exported(item))
            .collect()
    }
}

/// The items that a production root reaches, and the items that any root
/// reaches.
fn live_items(
    graph: &Graph,
    surface: &Surface,
    credits: &[&Credit],
) -> (FxHashSet<ItemId>, FxHashSet<ItemId>) {
    let mut edges: FxHashMap<ItemId, Vec<(ItemId, bool)>> = FxHashMap::default();
    let mut production_roots = Vec::new();
    let mut other_roots = Vec::new();
    for credit in credits {
        if credit.import {
            continue;
        }
        match (credit.owner, credit.origin) {
            // A doc link is no use in code, so it makes nothing live.
            (_, Origin::DocLink) => {}
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
        // A derive outside the standard library, for example `Deserialize`,
        // can construct each variant.
        let derived_variant = entry.kind == ItemKind::Variant
            && entry
                .parent
                .is_some_and(|parent| graph.items[parent].flags.helper_derive);
        if (main && krate.production)
            || entry.flags.external
            || surface.dead_allowed[item]
            || entry.flags.trait_impl
            || entry.flags.trait_item
            || entry.flags.proc_macro
            || surface.macro_covered[item]
            || derived_field
            || derived_variant
        {
            production_roots.push(item);
        } else if main || entry.flags.test_fn {
            other_roots.push(item);
        }
    }
    // An external user can call each item of an external API.
    production_roots.extend(surface.api_surface(graph));
    let production = reach(&edges, production_roots.iter().copied(), false);
    let mut all_roots = production_roots;
    all_roots.extend(other_roots);
    let any = reach(&edges, all_roots.into_iter(), true);
    (production, any)
}

/// A flag of each item that an enclosing item or module passes on: the item
/// has it when the item, an enclosing item, the `mod` item of an enclosing
/// module, or an enclosing module itself has it.
fn inherited(
    graph: &Graph,
    item_flag: impl Fn(&ItemFlags) -> bool,
    module_flag: impl Fn(&crate::graph::Module) -> bool,
) -> Vec<bool> {
    let mut modules: Vec<Option<bool>> = vec![None; graph.modules.len()];
    let mut module_has = |start: ModuleId| -> bool {
        let mut chain = Vec::new();
        let mut current = Some(start);
        let mut found = false;
        while let Some(id) = current {
            if let Some(known) = modules[id] {
                found = known;
                break;
            }
            let module = &graph.modules[id];
            if module_flag(module)
                || module
                    .item
                    .is_some_and(|item| item_flag(&graph.items[item].flags))
            {
                found = true;
                chain.push(id);
                break;
            }
            chain.push(id);
            current = module.parent;
        }
        for id in chain {
            modules[id] = Some(found);
        }
        found
    };
    (0..graph.items.len())
        .map(|item| {
            let mut current = Some(item);
            while let Some(id) = current {
                if item_flag(&graph.items[id].flags) {
                    return true;
                }
                current = graph.items[id].parent;
            }
            module_has(graph.items[item].module)
        })
        .collect()
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

/// A `pub use` of an item needs the item to stay `pub`. When the re-export has
/// a public finding, the item gets no public finding in this run, so each
/// reported change compiles alone. The next run reports the item.
fn wait_for_reexports(graph: &Graph, surface: &Surface, findings: &mut Vec<Finding>) {
    let public = |kind: FindingKind| kind != FindingKind::UnnecessaryRestricted;
    let waiting: FxHashSet<ItemId> = findings
        .iter()
        .filter(|finding| public(finding.kind) && graph.items[finding.item].kind == ItemKind::Use)
        .filter_map(|finding| surface.reexport_targets.get(&finding.item))
        .flatten()
        .copied()
        .collect();
    findings.retain(|finding| !(public(finding.kind) && waiting.contains(&finding.item)));
}

/// A file can be in more than one crate, for example a `mod data;` in each
/// bench target. A change of a keyword holds for each of those crates, so a
/// restricted finding needs the same finding in each crate that holds the
/// item. A public finding needs a public finding in each library that holds
/// it, and the weaker kind wins.
fn keep_shared_files(graph: &Graph, findings: &mut Vec<Finding>) {
    type Place<'a> = (&'a str, u32, u32);
    let place = |item: ItemId| -> Place<'_> {
        let entry = &graph.items[item];
        (entry.file.as_str(), entry.line, entry.column)
    };
    let mut instances: FxHashMap<Place<'_>, Vec<ItemId>> = FxHashMap::default();
    for (id, entry) in graph.items.iter().enumerate() {
        if entry.kind != ItemKind::Impl {
            instances.entry(place(id)).or_default().push(id);
        }
    }
    let by_item: FxHashMap<ItemId, Vec<FindingKind>> =
        findings
            .iter()
            .fold(FxHashMap::default(), |mut map, finding| {
                map.entry(finding.item).or_default().push(finding.kind);
                map
            });
    let has = |item: ItemId, test: &dyn Fn(FindingKind) -> bool| {
        by_item
            .get(&item)
            .is_some_and(|kinds| kinds.iter().any(|kind| test(*kind)))
    };
    let mut weaker: FxHashSet<ItemId> = FxHashSet::default();
    findings.retain(|finding| {
        let Some(others) = instances.get(&place(finding.item)) else {
            return true;
        };
        if others.len() < 2 {
            return true;
        }
        match finding.kind {
            FindingKind::UnnecessaryRestricted => others
                .iter()
                .all(|&other| has(other, &|kind| kind == FindingKind::UnnecessaryRestricted)),
            FindingKind::DeadPublic | FindingKind::UnnecessaryPublic => {
                let libraries: Vec<ItemId> = others
                    .iter()
                    .copied()
                    .filter(|&other| graph.crates[graph.items[other].krate].kind.is_library())
                    .collect();
                let all_public = libraries
                    .iter()
                    .all(|&other| has(other, &|kind| kind != FindingKind::UnnecessaryRestricted));
                if all_public
                    && finding.kind == FindingKind::DeadPublic
                    && libraries
                        .iter()
                        .any(|&other| has(other, &|kind| kind == FindingKind::UnnecessaryPublic))
                {
                    weaker.insert(finding.item);
                }
                all_public
            }
        }
    });
    for finding in findings.iter_mut() {
        if weaker.contains(&finding.item) {
            finding.kind = FindingKind::UnnecessaryPublic;
        }
    }
}

/// With uniform field groups, the fields of one struct with one written
/// visibility change together. When one field must keep its visibility, the
/// other fields of the group keep it too.
fn keep_field_groups(graph: &Graph, surface: &Surface, findings: &mut Vec<Finding>) {
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
            .any(|field| graph.items[*field].under_cfg || surface.dead_allowed[*field]);
        if !all_reduced || any_cfg {
            blocked.extend(fields.iter().copied());
        }
    }
    findings.retain(|finding| {
        finding.kind == FindingKind::DeadPublic || !blocked.contains(&finding.item)
    });
}

#[cfg(test)]
mod tests;
