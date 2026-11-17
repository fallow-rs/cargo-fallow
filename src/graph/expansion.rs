//! The crates where the output of a proc-macro crate can be.
//!
//! A proc-macro writes its output tokens into the crate that calls the macro.
//! A crate can call the macro when it can name the macro: it depends on the
//! proc-macro crate, or on a crate that re-exports the macro. A crate that
//! only depends on a caller cannot name the macro.
//!
//! A helper crate of a proc-macro crate writes output tokens too, and the
//! proc-macro crate returns them. Its output goes to the same crates.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::graph::extract::{Binding, Vis};
use crate::graph::resolve::visible_crates;
use crate::graph::{CrateId, CrateKind, Graph, Import};

/// The crates where the output of a proc-macro crate or of a helper crate
/// can be.
#[derive(Debug, Default)]
pub struct Expansion {
    /// Each other crate that can name the macro: a crate that depends on the
    /// proc-macro crate, or on a crate that re-exports the macro.
    pub callers: Vec<CrateId>,
    /// The proc-macro crate, the callers and the crates that they can use.
    pub crates: Vec<CrateId>,
}

/// The names that can lead to a macro in a crate that exports the macro.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Names {
    /// Each name. A proc-macro crate exports only macros.
    All,
    /// The names of the re-exported macros, and the names of the modules
    /// and the crates that hold them.
    Some(FxHashSet<String>),
}

impl Names {
    fn covers(&self, name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Some(names) => names.contains(name),
        }
    }

    /// Adds `name`, and returns true when the set changed.
    fn insert(&mut self, name: &str) -> bool {
        match self {
            Self::All => false,
            Self::Some(names) => names.insert(name.to_owned()),
        }
    }

    /// Adds `other`, and returns true when the set changed.
    fn extend(&mut self, other: &Self) -> bool {
        match (&mut *self, other) {
            (Self::All, _) => false,
            (_, Self::All) => {
                *self = Self::All;
                true
            }
            (Self::Some(names), Self::Some(more)) => {
                let before = names.len();
                names.extend(more.iter().cloned());
                names.len() != before
            }
        }
    }
}

/// The names that an import binds from a crate or a module with the macro
/// names `source`, or `None` when the import binds no macro. `rest` is the
/// part of the path after the crate or the module of `source`.
///
/// An import binds a macro only when each name of `rest` can lead to a
/// macro. `crate`, `self` and `super` are no names of the source.
///
/// - `use source;`, `extern crate source;` and `use source::*;` bind each
///   name of the source.
/// - A glob import of a module with a macro binds the names of the source.
/// - A named import with a path binds its own name.
///
/// The names of the module of the import also lead to the macros.
fn imported(graph: &Graph, import: &Import, rest: &[String], source: &Names) -> Option<Names> {
    let leads = rest
        .iter()
        .filter(|segment| !matches!(segment.as_str(), "crate" | "self" | "super"))
        .all(|segment| source.covers(segment));
    if !leads {
        return None;
    }
    let mut names = match &import.binding {
        Binding::Underscore => return None,
        Binding::Name(name) if !rest.is_empty() => {
            Names::Some(FxHashSet::from_iter([name.clone()]))
        }
        Binding::Glob | Binding::Name(_) => source.clone(),
    };
    if let Binding::Name(name) = &import.binding {
        names.insert(name);
    }
    for segment in &graph.modules[import.module].path {
        names.insert(segment);
    }
    Some(names)
}

/// The names that an import binds from a crate that exports macros, or from
/// a module of its own crate that holds an imported macro.
fn import_names(
    graph: &Graph,
    import: &Import,
    sources: &FxHashMap<CrateId, Names>,
    inner: &FxHashMap<CrateId, Names>,
) -> Option<Names> {
    let head = import.path.first()?;
    let mut externs = graph.crates[import.krate]
        .externs
        .iter()
        .filter(|name| &name.name == head)
        .peekable();
    if externs.peek().is_some() {
        let source =
            externs.find_map(|name| name.target.and_then(|target| sources.get(&target)))?;
        return imported(
            graph,
            import,
            import.path.get(1..).unwrap_or_default(),
            source,
        );
    }
    // A path in the crate, for example `crate::inner::name`, binds a macro
    // when it leads to a macro that another import of the crate binds.
    let local = inner.get(&import.krate)?;
    imported(graph, import, &import.path, local)
}

/// Adds `found` to the names of `krate`, and returns true when they changed.
fn add_names(map: &mut FxHashMap<CrateId, Names>, krate: CrateId, found: &Names) -> bool {
    map.entry(krate)
        .or_insert_with(|| Names::Some(FxHashSet::default()))
        .extend(found)
}

/// The proc-macro crate and each crate that re-exports one of its macros,
/// with the names that lead to the macros. A re-export of a re-export also
/// counts. A crate can import a macro first and re-export it later with a
/// path in the crate, as in `use pm::gen;` and `pub use crate::gen;`.
fn macro_sources(graph: &Graph, proc_macro: CrateId) -> FxHashMap<CrateId, Names> {
    let mut sources = FxHashMap::default();
    sources.insert(proc_macro, Names::All);
    let mut inner: FxHashMap<CrateId, Names> = FxHashMap::default();
    let mut changed = true;
    while changed {
        changed = false;
        for import in &graph.imports {
            if import.krate == proc_macro {
                continue;
            }
            let Some(found) = import_names(graph, import, &sources, &inner) else {
                continue;
            };
            changed |= add_names(&mut inner, import.krate, &found);
            if import.vis == Vis::Public {
                changed |= add_names(&mut sources, import.krate, &found);
            }
        }
    }
    sources
}

/// The [`Expansion`] of each proc-macro crate and of each helper crate.
///
/// A helper crate is a workspace library that a proc-macro crate uses as a
/// normal dependency, directly or through other helper crates. The
/// proc-macro crate returns the tokens that its helpers write, so the output
/// of a helper has the expansion of each proc-macro crate that uses it. A
/// dev-dependency or a build-dependency writes no output of the macro.
pub fn expansions(graph: &Graph) -> FxHashMap<CrateId, Expansion> {
    let macros: Vec<CrateId> = (0..graph.crates.len())
        .filter(|&krate| graph.crates[krate].kind == CrateKind::ProcMacro)
        .collect();
    if macros.is_empty() {
        return FxHashMap::default();
    }
    let mut visible: FxHashMap<CrateId, Vec<CrateId>> = FxHashMap::default();
    let mut found: FxHashMap<CrateId, Expansion> = macros
        .iter()
        .map(|&proc_macro| {
            let expansion = macro_expansion(graph, proc_macro, &mut visible);
            (proc_macro, expansion)
        })
        .collect();
    let mut helpers: FxHashMap<CrateId, Expansion> = FxHashMap::default();
    for proc_macro in macros {
        let expansion = &found[&proc_macro];
        for helper in helper_crates(graph, proc_macro) {
            let entry = helpers.entry(helper).or_default();
            entry.callers.extend(expansion.callers.iter().copied());
            entry.crates.extend(expansion.crates.iter().copied());
        }
    }
    for (helper, mut expansion) in helpers {
        expansion.callers.sort_unstable();
        expansion.callers.dedup();
        expansion.crates.sort_unstable();
        expansion.crates.dedup();
        found.insert(helper, expansion);
    }
    found
}

/// The [`Expansion`] of one proc-macro crate. `visible` holds the crates
/// that each crate can use, for the next calls.
fn macro_expansion(
    graph: &Graph,
    proc_macro: CrateId,
    visible: &mut FxHashMap<CrateId, Vec<CrateId>>,
) -> Expansion {
    let sources = macro_sources(graph, proc_macro);
    let callers: Vec<CrateId> = (0..graph.crates.len())
        .filter(|&krate| {
            krate != proc_macro
                && graph.crates[krate].externs.iter().any(|name| {
                    name.target
                        .is_some_and(|target| sources.contains_key(&target))
                })
        })
        .collect();
    let mut crates = Vec::new();
    for &krate in callers.iter().chain([&proc_macro]) {
        let seen = visible
            .entry(krate)
            .or_insert_with(|| visible_crates(graph, krate, false));
        crates.extend(seen.iter().copied());
    }
    crates.sort_unstable();
    crates.dedup();
    Expansion { callers, crates }
}

/// The helper crates of a proc-macro crate: each workspace library in the
/// normal dependencies of the proc-macro crate or of another helper crate.
fn helper_crates(graph: &Graph, proc_macro: CrateId) -> Vec<CrateId> {
    let mut helpers: Vec<CrateId> = Vec::new();
    let mut index = 0;
    let mut next = Some(proc_macro);
    while let Some(krate) = next {
        for name in graph.crates[krate].externs.iter().filter(|name| !name.dev) {
            if let Some(target) = name.target
                && graph.crates[target].kind == CrateKind::Lib
                && !helpers.contains(&target)
            {
                helpers.push(target);
            }
        }
        next = helpers.get(index).copied();
        index += 1;
    }
    helpers
}
