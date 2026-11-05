//! The module walk of a target.
//!
//! The walk starts at each target root and follows `mod name;` declarations,
//! with `#[path]` attributes. The caller loads each file and returns the module
//! declarations of that file, so the mutation command and the module graph use
//! one walk. A declaration inside a macro call is not visible to the parser.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::parse::Edition;

/// A `mod name;` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleDecl {
    /// The module name, without a `r#` prefix.
    pub name: String,
    /// The names of the enclosing inline modules, outermost first.
    pub inline_path: Vec<String>,
    /// The value of a `#[path]` attribute.
    pub path_attr: Option<String>,
    /// True when a `cfg` attribute other than `cfg(test)` is on the module.
    pub under_cfg: bool,
    /// True when a `cfg` that can be off in a build with all features and
    /// tests is on the module.
    pub cfg_off: bool,
    /// True when the module exists only in a test build, for example with
    /// `#[cfg(test)]`. Only the source walk of the structure checks sets it.
    pub test_only: bool,
}

/// A target root of a package.
#[derive(Debug, Clone)]
pub struct TargetRoot {
    /// The root file, relative to the workspace root, with `/` separators.
    pub path: String,
    /// The edition of the target.
    pub edition: Edition,
    /// True for a test target, for example a file in `tests/`.
    pub test_only: bool,
}

/// A file that the walk reaches.
#[derive(Debug, Clone)]
pub struct Visit {
    /// The path relative to the workspace root, with `/` separators.
    pub path: String,
    /// The edition of the target.
    pub edition: Edition,
    /// True when the file is a target root.
    pub is_root: bool,
    /// True when the file declares its child modules in its own folder: a root
    /// file, a `mod.rs` file or a file that a `#[path]` attribute names.
    pub is_mod_rs: bool,
    /// True when a `cfg` attribute is on a module declaration on the way to the file.
    pub under_cfg: bool,
    /// True when a `cfg` that can be off in a build with all features and tests
    /// is on a module declaration on the way to the file.
    pub cfg_off: bool,
    /// The module path of the file, from the crate root.
    pub module_path: Vec<String>,
    /// True when the file is part of a test target, or a declaration on the
    /// way to the file sets [`ModuleDecl::test_only`].
    pub test_only: bool,
}

/// A file that the walk reached but cannot use.
#[derive(Debug, Clone)]
pub struct SkippedFile {
    /// The path relative to the workspace root.
    pub path: String,
    /// The reason.
    pub reason: String,
}

/// The result of a walk.
#[derive(Debug, Clone)]
pub struct Walk<T> {
    /// The loaded files, sorted by path.
    pub files: Vec<T>,
    /// The files that a declaration reached, but that the loader refused.
    pub skipped: Vec<SkippedFile>,
    /// The `.rs` files under the target root folders that the walk did not reach.
    pub unreached: u32,
}

/// Walks the module tree of each target root.
///
/// `load` reads one file. It returns the loaded value and the module
/// declarations of the file, or the reason why it cannot use the file. A file
/// that two roots reach is loaded once.
pub fn walk<T>(
    workspace: &Path,
    roots: &[TargetRoot],
    mut load: impl FnMut(&Visit) -> Result<(T, Vec<ModuleDecl>), String>,
) -> Walk<T> {
    let mut files: BTreeMap<String, T> = BTreeMap::new();
    let mut skipped = Vec::new();
    let mut seen = BTreeSet::new();
    let mut folders = BTreeSet::new();
    let (mut tests, mut queue): (Vec<Visit>, Vec<Visit>) = roots
        .iter()
        .map(|root| {
            if let Some(parent) = Path::new(&root.path).parent() {
                folders.insert(parent.to_path_buf());
            }
            Visit {
                path: root.path.clone(),
                edition: root.edition,
                is_root: true,
                is_mod_rs: true,
                under_cfg: false,
                cfg_off: false,
                module_path: Vec::new(),
                test_only: root.test_only,
            }
        })
        .partition(|visit| visit.test_only);
    // The walk visits test code only after all other code. A file that test
    // code and other code both reach then does not count as test code.
    while let Some(next) = queue.pop().or_else(|| tests.pop()) {
        if !seen.insert(next.path.clone()) {
            continue;
        }
        match load(&next) {
            Ok((file, modules)) => {
                let dir = child_dir(&next.path, next.is_mod_rs);
                for module in &modules {
                    let child = resolve_module(workspace, &next.path, &dir, module);
                    let test_only = next.test_only || module.test_only;
                    let stack = if test_only { &mut tests } else { &mut queue };
                    stack.extend(child.map(|(path, is_mod_rs)| Visit {
                        path,
                        edition: next.edition,
                        is_root: false,
                        is_mod_rs,
                        under_cfg: next.under_cfg || module.under_cfg,
                        cfg_off: next.cfg_off || module.cfg_off,
                        module_path: child_module_path(&next.module_path, module),
                        test_only,
                    }));
                }
                files.insert(next.path.clone(), file);
            }
            Err(reason) => skipped.push(SkippedFile {
                path: next.path,
                reason,
            }),
        }
    }
    let unreached = count_unreached(workspace, &folders, &seen);
    Walk {
        files: files.into_values().collect(),
        skipped,
        unreached,
    }
}

fn child_module_path(parent: &[String], module: &ModuleDecl) -> Vec<String> {
    let mut path = parent.to_vec();
    path.extend(module.inline_path.iter().cloned());
    path.push(module.name.clone());
    path
}

/// The folder of the modules that a file declares: the folder of the file for
/// a root or a `mod.rs` file, else a subfolder with the file stem.
fn child_dir(path: &str, is_mod_rs: bool) -> PathBuf {
    let path = Path::new(path);
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    match (is_mod_rs, path.file_stem()) {
        (false, Some(stem)) => parent.join(stem),
        _ => parent.to_path_buf(),
    }
}

/// The file of a module declaration, and whether it counts as a `mod.rs` file.
fn resolve_module(
    workspace: &Path,
    declaring_file: &str,
    dir: &Path,
    module: &ModuleDecl,
) -> Option<(String, bool)> {
    if let Some(attr) = &module.path_attr {
        // Outside an inline module, the path starts at the folder of the
        // declaring file. Inside one, it starts at the module folder of the
        // file plus the inline module names.
        let base = if module.inline_path.is_empty() {
            Path::new(declaring_file).parent()?.to_path_buf()
        } else {
            let mut base = dir.to_path_buf();
            base.extend(&module.inline_path);
            base
        };
        let path = normalize(&base.join(attr))?;
        return workspace.join(&path).is_file().then_some((path, true));
    }
    let mut base = dir.to_path_buf();
    base.extend(&module.inline_path);
    let flat = normalize(&base.join(format!("{}.rs", module.name)))?;
    if workspace.join(&flat).is_file() {
        return Some((flat, false));
    }
    let nested = normalize(&base.join(&module.name).join("mod.rs"))?;
    workspace.join(&nested).is_file().then_some((nested, true))
}

/// Removes `.` and `..` from a relative path and joins it with `/`. Returns
/// `None` for a path that leaves the workspace root.
pub fn normalize(path: &Path) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
}

fn count_unreached(workspace: &Path, folders: &BTreeSet<PathBuf>, seen: &BTreeSet<String>) -> u32 {
    let mut all = BTreeSet::new();
    for folder in folders {
        list_rust_files(workspace, folder, &mut all);
    }
    let unreached = all.iter().filter(|path| !seen.contains(*path)).count();
    u32::try_from(unreached).unwrap_or(u32::MAX)
}

fn list_rust_files(workspace: &Path, folder: &Path, into: &mut BTreeSet<String>) {
    let Ok(entries) = fs::read_dir(workspace.join(folder)) else {
        return;
    };
    for entry in entries.flatten() {
        let relative = folder.join(entry.file_name());
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            list_rust_files(workspace, &relative, into);
        } else if relative.extension().is_some_and(|ext| ext == "rs") {
            into.extend(normalize(&relative));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_removes_dots_and_refuses_an_escape() {
        assert_eq!(
            normalize(Path::new("a/./b/../c.rs")).as_deref(),
            Some("a/c.rs")
        );
        assert_eq!(normalize(Path::new("../c.rs")), None);
        assert_eq!(normalize(Path::new("/abs/c.rs")), None);
    }

    #[test]
    fn child_folder_follows_the_file_kind() {
        assert_eq!(child_dir("src/lib.rs", true), Path::new("src"));
        assert_eq!(child_dir("src/a.rs", false), Path::new("src/a"));
        assert_eq!(child_dir("src/b/mod.rs", true), Path::new("src/b"));
    }

    fn decl(name: &str, inline: &[&str], path_attr: Option<&str>) -> ModuleDecl {
        ModuleDecl {
            name: name.to_owned(),
            inline_path: inline.iter().map(|part| (*part).to_owned()).collect(),
            path_attr: path_attr.map(str::to_owned),
            under_cfg: false,
            cfg_off: false,
            test_only: false,
        }
    }

    #[test]
    fn path_in_an_inline_module_of_a_plain_file_starts_at_the_module_folder() {
        let dir = std::path::PathBuf::from("src/a");
        let Ok(tmp) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let full = tmp.path().join("src/a/m/x.rs");
        let Some(parent) = full.parent() else {
            panic!("a parent folder");
        };
        let (Ok(()), Ok(())) = (fs::create_dir_all(parent), fs::write(&full, "")) else {
            panic!("write a fixture file");
        };
        let found = resolve_module(
            tmp.path(),
            "src/a.rs",
            &dir,
            &decl("y", &["m"], Some("x.rs")),
        );
        assert_eq!(found, Some(("src/a/m/x.rs".to_owned(), true)));
    }

    #[test]
    fn walk_gives_each_file_its_module_path() {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        for (path, text) in [
            ("src/lib.rs", ""),
            ("src/a.rs", ""),
            ("src/a/b.rs", ""),
            ("src/outer/c.rs", ""),
            ("src/other/place.rs", ""),
            ("src/stray.rs", ""),
        ] {
            let full = dir.path().join(path);
            let Some(parent) = full.parent() else {
                panic!("a parent folder");
            };
            let (Ok(()), Ok(())) = (fs::create_dir_all(parent), fs::write(&full, text)) else {
                panic!("write a fixture file");
            };
        }
        let roots = [TargetRoot {
            path: "src/lib.rs".to_owned(),
            edition: Edition::Edition2021,
            test_only: false,
        }];
        let walked = walk(dir.path(), &roots, |visit| {
            let modules = match visit.path.as_str() {
                "src/lib.rs" => vec![
                    decl("a", &[], None),
                    decl("c", &["outer"], None),
                    decl("moved", &[], Some("other/place.rs")),
                ],
                "src/a.rs" => vec![decl("b", &[], None)],
                _ => Vec::new(),
            };
            Ok(((visit.path.clone(), visit.module_path.join("::")), modules))
        });
        assert_eq!(
            walked.files,
            [
                ("src/a.rs".to_owned(), "a".to_owned()),
                ("src/a/b.rs".to_owned(), "a::b".to_owned()),
                ("src/lib.rs".to_owned(), String::new()),
                ("src/other/place.rs".to_owned(), "moved".to_owned()),
                ("src/outer/c.rs".to_owned(), "outer::c".to_owned()),
            ]
        );
        assert_eq!(walked.unreached, 1);
    }
}
