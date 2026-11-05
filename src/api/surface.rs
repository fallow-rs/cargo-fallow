//! The public API of each published library of a workspace.
//!
//! The key of an item is each path that another crate can write: the path
//! through `pub` modules, each `pub use` path and each path through a
//! `pub use <module>::*`. The members of a public type (the `pub` fields, the
//! variants and the `pub` items of an inherent `impl` block) and the members
//! of a public trait have a key below the key of their owner. An `impl` block
//! counts for a type only when its self path resolves to that type. A
//! `#[doc(hidden)]` item and the items below it are not API.
//!
//! One key can hold more than one item, for example one item for each `cfg`
//! variant.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::api::headers::{self, Fact, Facts};
use crate::graph::extract::{Binding, ItemKind, Vis};
use crate::graph::resolve::Resolver;
use crate::graph::{self, Graph, ItemId, ModuleId};
use crate::workspace::Workspace;

/// The deepest module chain of a walk. A deeper chain is a cycle of
/// re-exports.
const MAX_DEPTH: usize = 32;

/// One item at one key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Entry {
    /// The kind, as `item_kind` in the report.
    pub kind: &'static str,
    /// The normalized header.
    pub header: String,
    /// The file of the item, relative to the workspace root.
    pub file: String,
    /// The line of the name.
    pub line: u32,
}

/// The API of one published library.
#[derive(Debug, Clone, Default)]
pub struct CrateApi {
    /// The `Cargo.toml` of the package, relative to the workspace root.
    pub manifest: String,
    /// The root file of the library.
    pub root_file: String,
    /// The features of the package.
    pub features: BTreeSet<String>,
    /// The items at each key.
    pub entries: BTreeMap<String, Vec<Entry>>,
}

/// The packages of the workspace, by name: the version, and the API when the
/// package is a published library.
#[derive(Debug, Clone, Default)]
pub struct Surface {
    /// The API of each published library, by package name.
    pub crates: BTreeMap<String, CrateApi>,
    /// The version of each package, also an unpublished one.
    pub versions: BTreeMap<String, String>,
}

/// The libraries that `api` reads.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// The `exclude-crates` list of the `visibility` table.
    pub exclude: Vec<String>,
    /// The `public-crates` patterns of the `visibility` table, or `None`.
    pub public: Option<Vec<String>>,
}

impl Selection {
    /// The libraries that a registry can publish, with no table.
    pub const fn published() -> Self {
        Self {
            exclude: Vec::new(),
            public: None,
        }
    }

    /// Adds the names of the table of the base tree. A base table that is not
    /// valid adds nothing. Without `public-crates` at HEAD, the base list
    /// adds nothing either.
    pub fn add_base(&mut self, base: &Workspace) {
        let Ok((exclude, public)) = crate::visibility::config::external_api(base) else {
            return;
        };
        for name in exclude {
            if !self.exclude.contains(&name) {
                self.exclude.push(name);
            }
        }
        if let (Some(list), Some(names)) = (&mut self.public, public) {
            for name in names {
                if !list.contains(&name) {
                    list.push(name);
                }
            }
        }
    }

    /// True when `api` reads the library: a library that `visibility` treats
    /// as an external API. Without `public-crates`, these are the libraries
    /// that a registry can publish and the excluded libraries.
    fn reads(&self, krate: &graph::Crate) -> bool {
        krate.kind.is_library()
            && crate::visibility::is_api(krate, &self.exclude, self.public.as_deref())
    }
}

/// Reads the API of each published library of `workspace`.
pub fn surface(workspace: &Workspace, selection: &Selection) -> Surface {
    let module_graph = graph::build(workspace, &graph::Production::default());
    let resolver = Resolver::new(&module_graph);
    let walker = Walker::new(workspace, &module_graph, &resolver);
    let mut found = Surface::default();
    for member in &workspace.members {
        found
            .versions
            .insert(member.name.clone(), member.version.clone());
    }
    for (id, krate) in module_graph.crates.iter().enumerate() {
        if !selection.reads(krate) {
            continue;
        }
        let Some(member) = workspace
            .members
            .iter()
            .find(|member| member.name == krate.package)
        else {
            continue;
        };
        let root_file = member
            .targets
            .iter()
            .find(|target| {
                target
                    .kind
                    .iter()
                    .any(|kind| kind == "lib" || kind == "proc-macro")
            })
            .map(|target| target.src_path.clone())
            .unwrap_or_default();
        let mut api = CrateApi {
            manifest: member.manifest_path.clone(),
            root_file,
            features: member.features.iter().cloned().collect(),
            entries: BTreeMap::new(),
        };
        walker.crate_api(id, &mut api.entries);
        found.crates.insert(krate.package.clone(), api);
    }
    found
}

struct Walker<'g> {
    graph: &'g Graph,
    resolver: &'g Resolver<'g>,
    /// The items that each module declares at module level.
    declared: FxHashMap<ModuleId, Vec<ItemId>>,
    /// The imports of each module.
    imports: FxHashMap<ModuleId, Vec<usize>>,
    /// The children of each item.
    children: FxHashMap<ItemId, Vec<ItemId>>,
    /// The inherent `impl` blocks of each type.
    impls: FxHashMap<ItemId, Vec<ItemId>>,
    /// The facts of each file.
    facts: FxHashMap<String, Facts>,
}

impl<'g> Walker<'g> {
    fn new(workspace: &Workspace, graph: &'g Graph, resolver: &'g Resolver<'g>) -> Self {
        let mut declared: FxHashMap<ModuleId, Vec<ItemId>> = FxHashMap::default();
        let mut children: FxHashMap<ItemId, Vec<ItemId>> = FxHashMap::default();
        let mut impls: FxHashMap<ItemId, Vec<ItemId>> = FxHashMap::default();
        for (id, item) in graph.items.iter().enumerate() {
            match item.parent {
                Some(parent) => children.entry(parent).or_default().push(id),
                None => declared.entry(item.module).or_default().push(id),
            }
            if item.kind == ItemKind::Impl
                && !item.impl_trait
                && let Some(owner) = resolver.impl_self(id)
            {
                impls.entry(owner).or_default().push(id);
            }
        }
        let mut imports: FxHashMap<ModuleId, Vec<usize>> = FxHashMap::default();
        for (index, import) in graph.imports.iter().enumerate() {
            imports.entry(import.module).or_default().push(index);
        }
        Self {
            graph,
            resolver,
            declared,
            imports,
            children,
            impls,
            facts: read_facts(workspace, graph),
        }
    }

    fn crate_api(&self, krate: usize, entries: &mut BTreeMap<String, Vec<Entry>>) {
        let root = self.graph.crates[krate].root_module;
        let name = self.graph.crates[krate].name.clone();
        let mut seen: FxHashSet<(String, ItemId)> = FxHashSet::default();
        let mut walk = Walk {
            entries,
            seen: &mut seen,
            modules: FxHashSet::default(),
            stack: Vec::new(),
        };
        self.module(root, &name, 0, &mut walk);
        // A `#[macro_export]` macro is at the crate root.
        for (id, item) in self.graph.items.iter().enumerate() {
            if item.krate == krate && item.flags.macro_export && !self.fact(id).hidden {
                self.add(&format!("{name}::{}", item.name), id, &mut walk);
            }
        }
    }

    /// Adds the public items of `module` below `prefix`. A module that is
    /// already on the walk stack, for example through `pub use super::*`,
    /// adds nothing: its paths only repeat paths that the walk has.
    fn module(&self, module: ModuleId, prefix: &str, depth: usize, walk: &mut Walk<'_>) {
        if depth > MAX_DEPTH
            || walk.stack.contains(&module)
            || !walk.modules.insert((module, prefix.to_owned()))
        {
            return;
        }
        walk.stack.push(module);
        for &item in self.declared.get(&module).into_iter().flatten() {
            let entry = &self.graph.items[item];
            let skipped = matches!(entry.kind, ItemKind::Use | ItemKind::Impl | ItemKind::Macro);
            if entry.vis == Vis::Public && !skipped {
                self.item(&format!("{prefix}::{}", entry.name), item, depth, walk);
            }
        }
        for &index in self.imports.get(&module).into_iter().flatten() {
            self.import(index, prefix, depth, walk);
        }
        walk.stack.pop();
    }

    /// Adds the paths of one `pub use` below `prefix`.
    fn import(&self, index: usize, prefix: &str, depth: usize, walk: &mut Walk<'_>) {
        let import = &self.graph.imports[index];
        let file = &self.graph.paths[import.file as usize];
        if import.vis != Vis::Public || self.hidden_line(file, import.line) {
            return;
        }
        match &import.binding {
            Binding::Name(name) => {
                let key = format!("{prefix}::{name}");
                let (items, modules) = self.resolver.import_items(index);
                for item in items {
                    self.item(&key, item, depth, walk);
                }
                // A module with a `mod` item is in `items` too. A crate root
                // has none: `pub use other_crate as name`.
                for module in modules {
                    if self.graph.modules[module].item.is_none() {
                        self.module(module, &key, depth + 1, walk);
                    }
                }
            }
            Binding::Glob => {
                let (modules, enums) = self.resolver.glob_sources(index);
                for source in modules {
                    self.module(source, prefix, depth + 1, walk);
                }
                for item in enums {
                    self.members(prefix, item, walk);
                }
            }
            Binding::Underscore => {}
        }
    }

    /// True when a `#[doc(hidden)]` `use` item or `impl` block covers the line.
    fn hidden_line(&self, file: &str, line: u32) -> bool {
        self.facts
            .get(file)
            .is_some_and(|facts| facts.hidden_lines.contains(&line))
    }

    /// Adds `item` at `key`, with its members, and walks it when it is a
    /// module.
    fn item(&self, key: &str, item: ItemId, depth: usize, walk: &mut Walk<'_>) {
        if self.fact(item).hidden {
            return;
        }
        let entry = &self.graph.items[item];
        if entry.kind == ItemKind::Use {
            return;
        }
        self.add(key, item, walk);
        if let Some(child) = entry.child_module {
            self.module(child, key, depth + 1, walk);
        }
        self.members(key, item, walk);
    }

    /// Adds the members of a public type or trait below `key`.
    fn members(&self, key: &str, item: ItemId, walk: &mut Walk<'_>) {
        let entry = &self.graph.items[item];
        let trait_like = entry.kind == ItemKind::Trait;
        let mut members: Vec<ItemId> = self.children.get(&item).cloned().unwrap_or_default();
        for &block in self.impls.get(&item).into_iter().flatten() {
            let entry = &self.graph.items[block];
            if !self.hidden_line(&entry.file, entry.line) {
                members.extend(self.children.get(&block).into_iter().flatten().copied());
            }
        }
        for member in members {
            let child = &self.graph.items[member];
            let public = trait_like || child.kind == ItemKind::Variant || child.vis == Vis::Public;
            if public && !self.fact(member).hidden && child.kind != ItemKind::Impl {
                self.add(&format!("{key}::{}", child.name), member, walk);
            }
        }
    }

    fn add(&self, key: &str, item: ItemId, walk: &mut Walk<'_>) {
        if !walk.seen.insert((key.to_owned(), item)) {
            return;
        }
        let entry = &self.graph.items[item];
        walk.entries.entry(key.to_owned()).or_default().push(Entry {
            kind: entry.kind.label(),
            header: self.fact(item).header,
            file: entry.file.clone(),
            line: entry.line,
        });
    }

    fn fact(&self, item: ItemId) -> Fact {
        let entry = &self.graph.items[item];
        self.facts
            .get(&entry.file)
            .and_then(|facts| facts.items.get(&(entry.line, entry.name.clone())))
            .cloned()
            .unwrap_or_default()
    }
}

/// The state of the walk of one crate.
struct Walk<'a> {
    entries: &'a mut BTreeMap<String, Vec<Entry>>,
    seen: &'a mut FxHashSet<(String, ItemId)>,
    modules: FxHashSet<(ModuleId, String)>,
    /// The modules that the walk is in now.
    stack: Vec<ModuleId>,
}

/// Parses each file with a public item. A published library can re-export
/// an item of a crate that is not published, so each crate counts.
fn read_facts(workspace: &Workspace, graph: &Graph) -> FxHashMap<String, Facts> {
    let mut files: BTreeMap<&str, crate::parse::Edition> = BTreeMap::new();
    for item in &graph.items {
        if item.vis == Vis::Public {
            files
                .entry(item.file.as_str())
                .or_insert(graph.crates[item.krate].edition);
        }
    }
    let root = workspace.root.as_std_path();
    files
        .into_iter()
        .filter_map(|(file, edition)| {
            let text = fs::read_to_string(root.join(file)).ok()?;
            Some((file.to_owned(), headers::facts(&text, edition)))
        })
        .collect()
}
