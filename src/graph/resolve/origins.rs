//! The crates outside the workspace that can give a name in a scope.
//!
//! A one-name attribute or macro call, for example `#[name]` or `name!(..);`,
//! can be a macro that an import brings from another crate. The walk follows
//! named imports, renames, glob imports and re-exports, also through other
//! workspace crates. When the syntax cannot decide, the walk gives the crate:
//! a glob import counts also when a named import can shadow it.

use super::{Resolver, Target};
use crate::graph::{CrateId, ModuleId};

impl Resolver<'_> {
    /// The sorted names of the crates outside the workspace that can give
    /// `name` in the scope of `module`. The names are the names in code.
    pub fn outside_origins(&self, module: ModuleId, name: &str) -> Vec<String> {
        let key = (module, name.to_owned());
        if let Some(found) = self.origin_memo.borrow().get(&key) {
            return found.clone();
        }
        if !self.origin_active.borrow_mut().insert(key.clone()) {
            // A cycle of imports. The first call of the cycle adds the rest.
            self.origin_cut.set(true);
            return Vec::new();
        }
        let outer_cut = self.origin_cut.replace(false);
        let mut found = Vec::new();
        for &index in self.named.get(&(module, name)).into_iter().flatten() {
            found.extend(self.import_origins(index, None));
        }
        if !self.declared[module].contains_key(name) {
            for &index in &self.globs[module] {
                found.extend(self.import_origins(index, Some(name)));
            }
        }
        found.sort_unstable();
        found.dedup();
        self.origin_active.borrow_mut().remove(&key);
        let cut = self.origin_cut.replace(outer_cut);
        // A result that a cycle cut short is not complete for other queries.
        if cut {
            self.origin_cut.set(true);
        } else {
            self.origin_memo.borrow_mut().insert(key, found.clone());
        }
        found
    }

    /// The outside crates of the import at `index`. For a glob import, `name`
    /// is the name after the path.
    fn import_origins(&self, index: usize, name: Option<&str>) -> Vec<String> {
        let import = &self.graph.imports[index];
        let mut path = self.edition_path(import.krate, import.global, &import.path);
        path.extend(name.map(str::to_owned));
        self.path_origins(import.krate, import.module, &path, import.global)
    }

    /// The outside crates that the last segment of `path` can come from. The
    /// path is an import path of `module` in `krate`.
    fn path_origins(
        &self,
        krate: CrateId,
        module: ModuleId,
        path: &[String],
        global: bool,
    ) -> Vec<String> {
        let Some((last, prefix)) = path.split_last() else {
            return Vec::new();
        };
        // A path of one segment names a crate or a module, not a macro.
        let Some(first) = prefix.first() else {
            return Vec::new();
        };
        let mut found: Vec<String> = self
            .externs(krate, false)
            .iter()
            .filter(|entry| entry.target.is_none() && entry.name == *first)
            .map(|entry| entry.name.clone())
            .collect();
        let (targets, _, _) = self.resolve_path(krate, module, prefix, global, false);
        for target in targets {
            if let Some(next) = self.module_of(target) {
                found.extend(self.outside_origins(next, last));
            } else if target == Target::Outside {
                // The prefix leaves the workspace through an import: the crate
                // of the prefix gives the last segment.
                found.extend(match prefix {
                    [one] if !global => self.outside_origins(module, one),
                    [_] => Vec::new(),
                    _ => self.path_origins(krate, module, prefix, global),
                });
            }
        }
        found
    }
}
