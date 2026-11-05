//! Name resolution on the module graph.
//!
//! The resolver follows paths through modules, imports, glob imports,
//! re-exports, crate names and `impl` blocks. When a path does not resolve,
//! the last segment counts as a reference to each item with that name in the
//! visible crates. A method call, a field access and a name in macro tokens
//! work the same way, because the syntax does not show the type.

use std::cell::RefCell;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::graph::extract::{ALL_FIELDS, Binding, ItemKind, Origin, RefKind};
use crate::graph::{CrateId, Extern, Graph, ItemId, ModuleId};
use crate::parse::Edition;

/// The names that resolve outside the workspace when no item has them.
const OUTSIDE_ROOTS: [&str; 5] = ["std", "core", "alloc", "proc_macro", "test"];

/// The deepest chain of glob imports that a lookup follows.
const MAX_GLOB_DEPTH: usize = 16;

/// What a name resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Target {
    Item(ItemId),
    Module(ModuleId),
    Outside,
}

/// How sure a credit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// The path resolved to the item.
    Exact,
    /// The path did not resolve, so each item with the last name gets credit.
    Fallback,
    /// The syntax cannot decide: a method call, a field access, or a name in
    /// macro tokens, in an attribute or in a documentation code block.
    Untyped,
}

/// The code that gives a credit: an index into [`Graph::imports`] or into
/// [`Graph::refs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// An import.
    Import(usize),
    /// A reference.
    Ref(usize),
}

/// One credit: a place in code that can use an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Credit {
    /// The item.
    pub item: ItemId,
    /// The crate of the code.
    pub krate: CrateId,
    /// The module of the code.
    pub module: ModuleId,
    /// The item that holds the code. `None` for an import, for code at module
    /// level and for documentation.
    pub owner: Option<ItemId>,
    /// How sure the credit is.
    pub class: Class,
    /// Where the code is.
    pub origin: Origin,
    /// True for an import.
    pub import: bool,
    /// True in test code.
    pub test_only: bool,
    /// True when a `cfg` other than a test condition is on the code.
    pub under_cfg: bool,
    /// The code that gives the credit.
    pub source: Source,
}

/// The result of one path for a rule that names items: what the path names,
/// and the items that it goes through.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathResolution {
    /// The items that the path names.
    pub items: Vec<ItemId>,
    /// The modules that the path names.
    pub modules: Vec<ModuleId>,
    /// The modules, re-exports and types that the path goes through.
    pub through: Vec<ItemId>,
    /// True when the path resolved to its end.
    pub exact: bool,
    /// True when the first name is not in scope, so the resolver guessed it.
    pub guessed: bool,
}

/// The resolver. Build it once for a graph.
pub struct Resolver<'g> {
    graph: &'g Graph,
    declared: Vec<FxHashMap<&'g str, Vec<ItemId>>>,
    members: FxHashMap<(CrateId, &'g str), Vec<ItemId>>,
    by_name: FxHashMap<&'g str, Vec<ItemId>>,
    named: FxHashMap<(ModuleId, &'g str), Vec<usize>>,
    globs: Vec<Vec<usize>>,
    memo: RefCell<FxHashMap<usize, Vec<Target>>>,
    glob_memo: RefCell<FxHashMap<usize, Vec<Target>>>,
    lookup_memo: RefCell<Vec<FxHashMap<String, Vec<Target>>>>,
    active: RefCell<FxHashSet<usize>>,
    /// For each lookup at depth 0 that runs now, the modules that its glob walk
    /// expanded, with the smallest depth. A nested lookup gets its own map.
    expanded: RefCell<Vec<FxHashMap<ModuleId, usize>>>,
}

impl<'g> Resolver<'g> {
    /// Indexes the graph.
    pub fn new(graph: &'g Graph) -> Self {
        let mut declared: Vec<FxHashMap<&str, Vec<ItemId>>> =
            vec![FxHashMap::default(); graph.modules.len()];
        let mut members: FxHashMap<(CrateId, &str), Vec<ItemId>> = FxHashMap::default();
        let mut by_name: FxHashMap<&str, Vec<ItemId>> = FxHashMap::default();
        for (id, item) in graph.items.iter().enumerate() {
            if item.kind == ItemKind::Impl {
                continue;
            }
            by_name.entry(item.name.as_str()).or_default().push(id);
            match item.parent.map(|parent| &graph.items[parent]) {
                None => {
                    if item.kind != ItemKind::Use {
                        declared[item.module]
                            .entry(item.name.as_str())
                            .or_default()
                            .push(id);
                    }
                }
                Some(parent) => {
                    let owner = match parent.kind {
                        ItemKind::Impl => parent.impl_self.as_ref().and_then(|path| path.last()),
                        _ => Some(&parent.name),
                    };
                    if let Some(owner) = owner {
                        members
                            .entry((item.krate, owner.as_str()))
                            .or_default()
                            .push(id);
                    }
                }
            }
        }
        let mut named: FxHashMap<(ModuleId, &str), Vec<usize>> = FxHashMap::default();
        let mut globs = vec![Vec::new(); graph.modules.len()];
        for (index, import) in graph.imports.iter().enumerate() {
            match &import.binding {
                Binding::Name(name) => named
                    .entry((import.module, name.as_str()))
                    .or_default()
                    .push(index),
                Binding::Glob => globs[import.module].push(index),
                Binding::Underscore => {}
            }
        }
        Self {
            graph,
            declared,
            members,
            by_name,
            named,
            globs,
            memo: RefCell::new(FxHashMap::default()),
            glob_memo: RefCell::new(FxHashMap::default()),
            lookup_memo: RefCell::new(vec![FxHashMap::default(); graph.modules.len()]),
            active: RefCell::new(FxHashSet::default()),
            expanded: RefCell::new(Vec::new()),
        }
    }

    /// The credits of every import and every reference of the graph.
    pub fn credits(&self) -> Vec<Credit> {
        let mut credits = Vec::new();
        for (index, import) in self.graph.imports.iter().enumerate() {
            let base = Credit {
                item: 0,
                krate: import.krate,
                module: import.module,
                owner: None,
                class: Class::Exact,
                origin: Origin::Body,
                import: true,
                test_only: false,
                under_cfg: false,
                source: Source::Import(index),
            };
            if import.binding == Binding::Glob {
                let (targets, class, modules) = self.resolve_path(
                    import.krate,
                    import.module,
                    &import.path,
                    import.global,
                    false,
                );
                push_targets(&mut credits, base, class, &targets, &modules, self.graph);
                continue;
            }
            let targets = self.import_targets(index);
            let (_, class, modules) = self.resolve_path(
                import.krate,
                import.module,
                &import.path,
                import.global,
                false,
            );
            push_targets(&mut credits, base, class, &targets, &modules, self.graph);
        }
        for (index, reference) in self.graph.refs.iter().enumerate() {
            let base = Credit {
                item: 0,
                krate: reference.krate,
                module: reference.module,
                owner: reference.owner,
                class: Class::Exact,
                origin: reference.origin,
                import: false,
                test_only: reference.test_only,
                under_cfg: reference.under_cfg,
                source: Source::Ref(index),
            };
            self.credit_ref(&mut credits, base, &reference.kind);
        }
        credits
    }

    /// The items and the modules that a named import resolves to, without the
    /// item of the import itself.
    pub fn import_items(&self, index: usize) -> (Vec<ItemId>, Vec<ModuleId>) {
        let own = self.graph.imports[index].item;
        let mut items = Vec::new();
        let mut modules = Vec::new();
        for target in self.import_targets(index) {
            match target {
                Target::Item(item) if Some(item) != own => {
                    items.push(item);
                    modules.extend(self.graph.items[item].child_module);
                }
                Target::Module(module) => modules.push(module),
                _ => {}
            }
        }
        (items, modules)
    }

    /// The modules and the enums that a glob import reads.
    pub fn glob_sources(&self, index: usize) -> (Vec<ModuleId>, Vec<ItemId>) {
        let import = &self.graph.imports[index];
        let (targets, _, _) = self.resolve_path(
            import.krate,
            import.module,
            &import.path,
            import.global,
            false,
        );
        let mut modules = Vec::new();
        let mut items = Vec::new();
        for target in targets {
            match (self.module_of(target), target) {
                (Some(module), _) => modules.push(module),
                (None, Target::Item(item)) => items.push(item),
                _ => {}
            }
        }
        (modules, items)
    }

    /// Resolves a path from `module`, for the item rules of `boundaries`.
    pub fn resolve(
        &self,
        krate: CrateId,
        module: ModuleId,
        segments: &[String],
        global: bool,
    ) -> PathResolution {
        let guessed = segments
            .first()
            .is_some_and(|first| self.start(krate, module, first, global, false).is_empty());
        let (targets, class, through) = self.resolve_path(krate, module, segments, global, false);
        let mut found = PathResolution {
            through,
            exact: class == Class::Exact,
            guessed,
            ..PathResolution::default()
        };
        for target in targets {
            match target {
                Target::Item(item) => found.items.push(item),
                Target::Module(module) => found.modules.push(module),
                Target::Outside => {}
            }
        }
        found
    }

    /// Resolves the path of the import at `index`, with the path rule of its
    /// edition.
    pub fn resolve_import(&self, index: usize) -> PathResolution {
        let import = &self.graph.imports[index];
        let path = self.edition_path(import.krate, import.global, &import.path);
        self.resolve(import.krate, import.module, &path, import.global)
    }

    /// The type that an `impl` block is for, when its self path resolves to
    /// one item. The resolver keys a member by the last name of the type, so
    /// this check tells two types with one name apart.
    pub fn impl_self(&self, item: ItemId) -> Option<ItemId> {
        let entry = &self.graph.items[item];
        let path = entry.impl_self.as_ref()?;
        let found = self.resolve(entry.krate, entry.module, path, false);
        match (found.exact, found.items.as_slice()) {
            (true, [one]) => Some(*one),
            _ => None,
        }
    }

    fn credit_ref(&self, credits: &mut Vec<Credit>, base: Credit, kind: &RefKind) {
        let untyped_origin = matches!(
            base.origin,
            Origin::Macro | Origin::PublicMacro | Origin::Attr | Origin::Doc
        );
        let doc = base.origin == Origin::Doc;
        match kind {
            RefKind::Path { segments, global } => {
                if untyped_origin && segments.len() == 1 {
                    // A bare name cannot name a field or a method. A `Member`
                    // reference covers the names that can.
                    self.credit_names(credits, base, &segments[0], Class::Untyped, |kind| {
                        !matches!(kind, ItemKind::Field | ItemKind::Method)
                    });
                    return;
                }
                let (targets, class, modules) =
                    self.resolve_path(base.krate, base.module, segments, *global, doc);
                let class = if untyped_origin && class == Class::Fallback {
                    Class::Untyped
                } else {
                    class
                };
                push_targets(credits, base, class, &targets, &modules, self.graph);
                if base.origin != Origin::Signature {
                    self.credit_tuple_fields(credits, base, class, &targets);
                }
                // A path that did not resolve to its end, for example through a
                // type alias, can name any item with its last name.
                if class != Class::Exact
                    && let Some(last) = segments.last()
                {
                    // A path names no field. A one-name path names no method.
                    let single = segments.len() == 1;
                    self.credit_names(credits, base, last, class, |kind| {
                        kind != ItemKind::Field && !(single && kind == ItemKind::Method)
                    });
                }
            }
            RefKind::Method(name) => {
                self.credit_names(credits, base, name, Class::Untyped, |kind| {
                    matches!(kind, ItemKind::Method)
                });
            }
            RefKind::Field(name) => {
                self.credit_names(credits, base, name, Class::Untyped, |kind| {
                    matches!(kind, ItemKind::Field)
                });
            }
            RefKind::RecordField {
                segments,
                global,
                field,
            } => {
                let (targets, class, _) =
                    self.resolve_path(base.krate, base.module, segments, *global, doc);
                let fields: Vec<ItemId> = targets
                    .iter()
                    .filter_map(|target| match target {
                        Target::Item(item) => Some(*item),
                        _ => None,
                    })
                    .flat_map(|item| self.fields_of(item, field))
                    .collect();
                let outside =
                    !targets.is_empty() && targets.iter().all(|target| *target == Target::Outside);
                if !fields.is_empty() {
                    for item in fields {
                        credits.push(Credit {
                            item,
                            class,
                            ..base
                        });
                    }
                } else if !outside {
                    // The struct is unknown, so any field with the name can be
                    // the one.
                    self.credit_names(credits, base, field, Class::Untyped, |kind| {
                        kind == ItemKind::Field
                    });
                }
            }
            RefKind::Member(name) => {
                self.credit_names(credits, base, name, Class::Untyped, |kind| {
                    matches!(kind, ItemKind::Field | ItemKind::Method)
                });
            }
            RefKind::Macro(name) => self.credit_names(credits, base, name, Class::Exact, |kind| {
                kind == ItemKind::Macro
            }),
        }
    }

    fn fields_of(&self, item: ItemId, field: &str) -> Vec<ItemId> {
        let graph = self.graph;
        let owner = &graph.items[item];
        let key = (owner.krate, owner.name.as_str());
        self.members
            .get(&key)
            .into_iter()
            .flatten()
            .copied()
            .filter(|id| {
                let candidate = &graph.items[*id];
                candidate.kind == ItemKind::Field
                    && (candidate.name == field || field == ALL_FIELDS)
                    && candidate.parent == Some(item)
            })
            .collect()
    }

    /// A call or a pattern of a tuple struct or a tuple variant, `Wrapper(x)`,
    /// needs each tuple field to be visible here.
    fn credit_tuple_fields(
        &self,
        credits: &mut Vec<Credit>,
        base: Credit,
        class: Class,
        targets: &[Target],
    ) {
        for target in targets {
            let Target::Item(item) = *target else {
                continue;
            };
            if !matches!(
                self.graph.items[item].kind,
                ItemKind::Struct | ItemKind::Variant
            ) {
                continue;
            }
            let owner = &self.graph.items[item];
            for &field in self
                .members
                .get(&(owner.krate, owner.name.as_str()))
                .into_iter()
                .flatten()
            {
                let candidate = &self.graph.items[field];
                let tuple = candidate.name.bytes().all(|byte| byte.is_ascii_digit());
                if candidate.kind == ItemKind::Field && candidate.parent == Some(item) && tuple {
                    credits.push(Credit {
                        item: field,
                        class,
                        ..base
                    });
                }
            }
        }
    }

    /// Credits each item called `name` in the crate of `base` and in the
    /// crates that it can use.
    fn credit_names(
        &self,
        credits: &mut Vec<Credit>,
        base: Credit,
        name: &str,
        class: Class,
        keep: impl Fn(ItemKind) -> bool,
    ) {
        let Some(items) = self.by_name.get(name) else {
            return;
        };
        let visible = self.visible_crates(base.krate, base.origin == Origin::Doc);
        for &item in items {
            let candidate = &self.graph.items[item];
            if keep(candidate.kind) && visible.contains(&candidate.krate) {
                credits.push(Credit {
                    item,
                    class,
                    ..base
                });
            }
        }
    }

    /// The crate and the workspace crates that it depends on, also through
    /// other crates: a re-export can give a type of a deeper crate, and the
    /// code can call its methods.
    fn visible_crates(&self, krate: CrateId, doc: bool) -> Vec<CrateId> {
        let entry = &self.graph.crates[krate];
        let externs = if doc {
            &entry.doc_externs
        } else {
            &entry.externs
        };
        let mut crates: Vec<CrateId> = externs.iter().filter_map(|name| name.target).collect();
        if !doc {
            crates.push(krate);
        }
        let mut index = 0;
        while let Some(&next) = crates.get(index) {
            for name in &self.graph.crates[next].externs {
                if let Some(target) = name.target
                    && !crates.contains(&target)
                {
                    crates.push(target);
                }
            }
            index += 1;
        }
        crates
    }

    fn externs(&self, krate: CrateId, doc: bool) -> &'g [Extern] {
        let entry = &self.graph.crates[krate];
        if doc {
            &entry.doc_externs
        } else {
            &entry.externs
        }
    }

    /// Resolves a path. Returns the targets, how sure the result is, and the
    /// items that the path goes through: modules, re-exports and types.
    fn resolve_path(
        &self,
        krate: CrateId,
        module: ModuleId,
        segments: &[String],
        global: bool,
        doc: bool,
    ) -> (Vec<Target>, Class, Vec<ItemId>) {
        let Some((first, rest)) = segments.split_first() else {
            return (Vec::new(), Class::Fallback, Vec::new());
        };
        let mut current = self.start(krate, module, first, global, doc);
        let mut through = Vec::new();
        let guessed = current.is_empty();
        if guessed {
            if rest.is_empty() {
                return (Vec::new(), Class::Fallback, through);
            }
            // The first name is not in scope. When no visible crate has a type
            // or a module with that name, the path names something outside
            // the workspace, such as `String::new`.
            current = self.types_named(krate, first, doc);
            if current.is_empty() {
                return (vec![Target::Outside], Class::Exact, through);
            }
        }
        for segment in rest {
            if current.iter().all(|target| *target == Target::Outside) {
                return (current, Class::Exact, through);
            }
            let mut next = Vec::new();
            for target in &current {
                match target {
                    Target::Item(item) => through.push(*item),
                    Target::Module(module) => through.extend(self.graph.modules[*module].item),
                    Target::Outside => {}
                }
                next.extend(self.step(*target, segment));
            }
            next.sort_unstable();
            next.dedup();
            if next.is_empty() {
                return (current, Class::Fallback, through);
            }
            current = next;
        }
        let class = if guessed {
            Class::Fallback
        } else {
            Class::Exact
        };
        (current, class, through)
    }

    /// The types and modules called `name` in the visible crates.
    fn types_named(&self, krate: CrateId, name: &str, doc: bool) -> Vec<Target> {
        let visible = self.visible_crates(krate, doc);
        self.by_name
            .get(name)
            .into_iter()
            .flatten()
            .filter(|&&item| {
                let entry = &self.graph.items[item];
                entry.parent.is_none()
                    && visible.contains(&entry.krate)
                    && matches!(
                        entry.kind,
                        ItemKind::Module
                            | ItemKind::Struct
                            | ItemKind::Enum
                            | ItemKind::Union
                            | ItemKind::Trait
                            | ItemKind::TypeAlias
                    )
            })
            .map(|&item| Target::Item(item))
            .collect()
    }

    fn start(
        &self,
        krate: CrateId,
        module: ModuleId,
        first: &str,
        global: bool,
        doc: bool,
    ) -> Vec<Target> {
        if global {
            return self.extern_targets(krate, first, doc);
        }
        match first {
            "crate" if !doc => return vec![Target::Module(self.graph.crates[krate].root_module)],
            "self" if !doc => return vec![Target::Module(module)],
            "super" if !doc => {
                return self.graph.modules[module]
                    .parent
                    .map(Target::Module)
                    .into_iter()
                    .collect();
            }
            _ => {}
        }
        if !doc {
            let local = self.lookup(module, first, 0);
            if !local.is_empty() {
                return local;
            }
        }
        self.extern_targets(krate, first, doc)
    }

    fn extern_targets(&self, krate: CrateId, name: &str, doc: bool) -> Vec<Target> {
        let mut found: Vec<Target> = self
            .externs(krate, doc)
            .iter()
            .filter(|entry| entry.name == name)
            .map(|entry| {
                entry.target.map_or(Target::Outside, |target| {
                    Target::Module(self.graph.crates[target].root_module)
                })
            })
            .collect();
        if found.is_empty() && OUTSIDE_ROOTS.contains(&name) {
            found.push(Target::Outside);
        }
        found
    }

    fn module_of(&self, target: Target) -> Option<ModuleId> {
        match target {
            Target::Module(module) => Some(module),
            Target::Item(item) => self.graph.items[item].child_module,
            Target::Outside => None,
        }
    }

    /// The targets of `segment` after `target`.
    fn step(&self, target: Target, segment: &str) -> Vec<Target> {
        if let Some(module) = self.module_of(target) {
            return match segment {
                "super" => self.graph.modules[module]
                    .parent
                    .map(Target::Module)
                    .into_iter()
                    .collect(),
                "self" => vec![Target::Module(module)],
                _ => self.lookup(module, segment, 0),
            };
        }
        let Target::Item(item) = target else {
            return vec![Target::Outside];
        };
        let owner = &self.graph.items[item];
        if !matches!(
            owner.kind,
            ItemKind::Struct
                | ItemKind::Enum
                | ItemKind::Union
                | ItemKind::Trait
                | ItemKind::TypeAlias
        ) {
            return Vec::new();
        }
        self.members
            .get(&(owner.krate, owner.name.as_str()))
            .into_iter()
            .flatten()
            .filter(|id| self.graph.items[**id].name == segment)
            .map(|id| Target::Item(*id))
            .collect()
    }

    /// The targets of `name` in the scope of `module`: declared items, named
    /// imports and then glob imports. The result of a call with no glob in
    /// progress is kept, because a glob cycle can cut a result short.
    fn lookup(&self, module: ModuleId, name: &str, depth: usize) -> Vec<Target> {
        if let Some(found) = self.lookup_memo.borrow()[module].get(name) {
            return found.clone();
        }
        if depth == 0 {
            self.expanded.borrow_mut().push(FxHashMap::default());
            let found = self.lookup_in(module, name, depth);
            self.expanded.borrow_mut().pop();
            return found;
        }
        // The walk of this query already expanded the module at this depth or
        // less deep, so its targets are in the result already. Without this stop
        // a graph of glob layers is walked once for each path.
        {
            let mut expanded = self.expanded.borrow_mut();
            if let Some(seen) = expanded.last_mut() {
                match seen.get(&module) {
                    Some(&first) if first <= depth => return Vec::new(),
                    _ => {
                        seen.insert(module, depth);
                    }
                }
            }
        }
        self.lookup_in(module, name, depth)
    }

    fn lookup_in(&self, module: ModuleId, name: &str, depth: usize) -> Vec<Target> {
        let mut found: Vec<Target> = self.declared[module]
            .get(name)
            .into_iter()
            .flatten()
            .map(|item| Target::Item(*item))
            .collect();
        for &index in self.named.get(&(module, name)).into_iter().flatten() {
            found.extend(self.import_targets(index));
        }
        if found.is_empty() && depth < MAX_GLOB_DEPTH {
            for &index in &self.globs[module] {
                if self.active.borrow().contains(&index) {
                    continue;
                }
                self.active.borrow_mut().insert(index);
                for source in self.glob_targets(index) {
                    found.extend(self.glob_step(source, name, depth + 1));
                }
                self.active.borrow_mut().remove(&index);
            }
        }
        if self.active.borrow().is_empty() {
            self.lookup_memo.borrow_mut()[module].insert(name.to_owned(), found.clone());
        }
        found
    }

    fn glob_targets(&self, index: usize) -> Vec<Target> {
        if let Some(found) = self.glob_memo.borrow().get(&index) {
            return found.clone();
        }
        let import = &self.graph.imports[index];
        let (sources, _, _) = self.resolve_path(
            import.krate,
            import.module,
            &import.path,
            import.global,
            false,
        );
        self.glob_memo.borrow_mut().insert(index, sources.clone());
        sources
    }

    fn glob_step(&self, source: Target, name: &str, depth: usize) -> Vec<Target> {
        match self.module_of(source) {
            Some(module) => self.lookup(module, name, depth),
            None => match source {
                Target::Item(_) => self.step(source, name),
                _ => Vec::new(),
            },
        }
    }

    /// The targets of a named import. A named `pub` re-export also gives its
    /// own item, so a path through the re-export credits it.
    fn import_targets(&self, index: usize) -> Vec<Target> {
        if let Some(found) = self.memo.borrow().get(&index) {
            return found.clone();
        }
        if !self.active.borrow_mut().insert(index) {
            return Vec::new();
        }
        let import = &self.graph.imports[index];
        let path = self.edition_path(import.krate, import.global, &import.path);
        let (mut targets, class, _) =
            self.resolve_path(import.krate, import.module, &path, import.global, false);
        if class != Class::Exact {
            targets.clear();
        }
        if let Some(item) = import.item {
            targets.push(Target::Item(item));
        }
        self.active.borrow_mut().remove(&index);
        self.memo.borrow_mut().insert(index, targets.clone());
        targets
    }

    /// In edition 2015 an import path starts at the crate root.
    fn edition_path(&self, krate: CrateId, global: bool, path: &[String]) -> Vec<String> {
        let entry = &self.graph.crates[krate];
        let relative = path
            .first()
            .is_some_and(|first| !matches!(first.as_str(), "crate" | "self" | "super"));
        if entry.edition == Edition::Edition2015 && !global && relative {
            let root = &self.graph.modules[entry.root_module];
            if root.parent.is_none()
                && path.first().is_some_and(|first| {
                    self.declared[entry.root_module].contains_key(first.as_str())
                })
            {
                let mut full = vec!["crate".to_owned()];
                full.extend(path.iter().cloned());
                return full;
            }
        }
        path.to_vec()
    }
}

fn push_targets(
    credits: &mut Vec<Credit>,
    base: Credit,
    class: Class,
    targets: &[Target],
    through: &[ItemId],
    graph: &Graph,
) {
    for target in targets {
        let item = match target {
            Target::Item(item) => Some(*item),
            Target::Module(module) => graph.modules[*module].item,
            Target::Outside => None,
        };
        if let Some(item) = item {
            credits.push(Credit {
                item,
                class,
                ..base
            });
        }
    }
    for &item in through {
        credits.push(Credit {
            item,
            class: Class::Exact,
            ..base
        });
    }
}
