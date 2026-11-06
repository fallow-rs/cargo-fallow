//! The crates where the output of a proc-macro crate can be.
//!
//! A proc-macro writes its output tokens into the crate that calls the macro.
//! A crate can call the macro when it can name the macro: it depends on the
//! proc-macro crate, or on a crate that re-exports the macro. A crate that
//! only depends on a caller cannot name the macro.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::graph::extract::{Binding, Vis};
use crate::graph::resolve::visible_crates;
use crate::graph::{CrateId, CrateKind, Graph, Import};

/// The crates where the output of a proc-macro crate can be.
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

/// The names that a `pub use` exports from a crate that exports macros with
/// the names `source`, or `None` when the import exports no macro.
///
/// - `pub use source;` and `pub extern crate source;` export each name of
///   the source under the binding.
/// - A glob import can export a macro, so it exports the names of the
///   source.
/// - A named import exports a macro when a name of its path after the crate
///   can lead to a macro.
///
/// The names of the module of the import also lead to the macros.
fn exported(graph: &Graph, import: &Import, source: &Names) -> Option<Names> {
    let rest = import.path.get(1..).unwrap_or_default();
    let mut names = match &import.binding {
        Binding::Underscore => return None,
        Binding::Glob => source.clone(),
        Binding::Name(name) => {
            let mut names = if rest.is_empty() {
                source.clone()
            } else if rest.iter().any(|segment| source.covers(segment)) {
                Names::Some(FxHashSet::default())
            } else {
                return None;
            };
            names.insert(name);
            names
        }
    };
    for segment in &graph.modules[import.module].path {
        names.insert(segment);
    }
    Some(names)
}

/// The proc-macro crate and each crate that re-exports one of its macros,
/// with the names that lead to the macros. A re-export of a re-export also
/// counts.
fn macro_sources(
    graph: &Graph,
    imports: &[&Import],
    proc_macro: CrateId,
) -> FxHashMap<CrateId, Names> {
    let mut sources = FxHashMap::default();
    sources.insert(proc_macro, Names::All);
    let mut changed = true;
    while changed {
        changed = false;
        for &import in imports {
            let Some(head) = import.path.first() else {
                continue;
            };
            let source = graph.crates[import.krate]
                .externs
                .iter()
                .filter(|name| &name.name == head)
                .find_map(|name| name.target.and_then(|target| sources.get(&target)));
            let Some(found) = source.and_then(|source| exported(graph, import, source)) else {
                continue;
            };
            changed |= sources
                .entry(import.krate)
                .or_insert_with(|| Names::Some(FxHashSet::default()))
                .extend(&found);
        }
    }
    sources
}

/// The [`Expansion`] of each proc-macro crate.
pub fn expansions(graph: &Graph) -> FxHashMap<CrateId, Expansion> {
    let macros: Vec<CrateId> = (0..graph.crates.len())
        .filter(|&krate| graph.crates[krate].kind == CrateKind::ProcMacro)
        .collect();
    if macros.is_empty() {
        return FxHashMap::default();
    }
    let imports: Vec<&Import> = graph
        .imports
        .iter()
        .filter(|import| import.vis == Vis::Public)
        .collect();
    let mut visible: FxHashMap<CrateId, Vec<CrateId>> = FxHashMap::default();
    macros
        .into_iter()
        .map(|proc_macro| {
            let sources = macro_sources(graph, &imports, proc_macro);
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
            (proc_macro, Expansion { callers, crates })
        })
        .collect()
}
