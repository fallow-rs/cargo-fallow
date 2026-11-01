//! Finds the source files of a package and copies a source tree.
//!
//! The walk starts at each target root and follows `mod name;` declarations,
//! so a file that no module declares does not add mutants that no compile
//! checks. A declaration inside a macro call is not visible to the parser. The
//! report counts each file that the walk does not reach.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::mutation::schema::CrateRoot;
use crate::mutation::sites::{FileSites, ModuleDecl, find_sites};
use crate::parse::Edition;

/// A target root of a package.
#[derive(Debug, Clone)]
pub struct TargetRoot {
    /// The root file, relative to the workspace root, with `/` separators.
    pub path: String,
    /// The edition of the target.
    pub edition: Edition,
}

/// One source file of the package.
#[derive(Debug, Clone)]
pub struct CrateFile {
    /// The path relative to the workspace root, with `/` separators.
    pub path: String,
    /// The original text.
    pub original: String,
    /// The edition of the target.
    pub edition: Edition,
    /// The sites and the other parse results.
    pub found: FileSites,
    /// The helper position, when the file is a target root.
    pub root: Option<CrateRoot>,
    /// True when a `cfg` attribute is on a module declaration of the file.
    pub under_cfg: bool,
}

/// A file that the walk reached but cannot use.
#[derive(Debug, Clone)]
pub struct SkippedFile {
    /// The path relative to the workspace root.
    pub path: String,
    /// The reason.
    pub reason: String,
}

/// The result of the walk.
#[derive(Debug, Clone, Default)]
pub struct Collected {
    /// The files, sorted by path.
    pub files: Vec<CrateFile>,
    /// The files that a declaration reached, but that have no sites.
    pub skipped: Vec<SkippedFile>,
    /// The `.rs` files under the target root folders that the walk did not reach.
    pub unreached: u32,
}

/// Walks the module tree of each target.
pub fn collect(workspace: &Path, targets: &[TargetRoot]) -> Collected {
    let mut files: BTreeMap<String, CrateFile> = BTreeMap::new();
    let mut skipped = Vec::new();
    let mut seen = BTreeSet::new();
    let mut folders = BTreeSet::new();
    let mut queue: Vec<Pending> = targets
        .iter()
        .map(|target| {
            if let Some(parent) = Path::new(&target.path).parent() {
                folders.insert(parent.to_path_buf());
            }
            Pending {
                path: target.path.clone(),
                edition: target.edition,
                is_root: true,
                is_mod_rs: true,
                under_cfg: false,
            }
        })
        .collect();
    while let Some(next) = queue.pop() {
        if !seen.insert(next.path.clone()) {
            continue;
        }
        match load_file(workspace, &next) {
            Ok(file) => {
                let dir = child_dir(&next.path, next.is_mod_rs);
                for module in &file.found.modules {
                    let child = resolve_module(workspace, &next.path, &dir, module);
                    queue.extend(child.map(|(path, is_mod_rs)| Pending {
                        path,
                        edition: next.edition,
                        is_root: false,
                        is_mod_rs,
                        under_cfg: next.under_cfg || module.under_cfg,
                    }));
                }
                files.insert(file.path.clone(), file);
            }
            Err(reason) => skipped.push(SkippedFile {
                path: next.path,
                reason,
            }),
        }
    }
    let unreached = count_unreached(workspace, &folders, &seen);
    Collected {
        files: files.into_values().collect(),
        skipped,
        unreached,
    }
}

struct Pending {
    path: String,
    edition: Edition,
    is_root: bool,
    is_mod_rs: bool,
    under_cfg: bool,
}

fn load_file(workspace: &Path, next: &Pending) -> Result<CrateFile, String> {
    let original = fs::read_to_string(workspace.join(&next.path))
        .map_err(|err| format!("cannot read the file: {err}"))?;
    let found = find_sites(&original, next.edition, &next.path)
        .map_err(|outcome| format!("the parse failed: {outcome:?}"))?;
    let root = next.is_root.then_some(CrateRoot {
        after_inner_attrs: found.after_inner_attrs,
    });
    Ok(CrateFile {
        path: next.path.clone(),
        original,
        edition: next.edition,
        found,
        root,
        under_cfg: next.under_cfg,
    })
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
        let parent = Path::new(declaring_file).parent()?;
        let path = normalize(&parent.join(attr))?;
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

/// Copies a source tree. The copy leaves out each `.git` folder and the `target`
/// folder at the root, and it copies a symbolic link as a link.
pub fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    copy_entries(from, to, true)
}

fn copy_entries(from: &Path, to: &Path, at_root: bool) -> io::Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let kind = entry.file_type()?;
        let skip = kind.is_dir() && (name == ".git" || (at_root && name == "target"));
        if skip {
            continue;
        }
        let (source, destination) = (entry.path(), to.join(&name));
        if kind.is_dir() {
            fs::create_dir(&destination)?;
            copy_entries(&source, &destination, false)?;
        } else if kind.is_symlink() {
            copy_link(&source, &destination)?;
        } else {
            fs::copy(&source, &destination)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn copy_link(source: &Path, destination: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(fs::read_link(source)?, destination)
}

/// A link on Windows needs a privilege, so a link there becomes a copy. A link
/// to a folder or a broken link is left out.
#[cfg(not(unix))]
fn copy_link(source: &Path, destination: &Path) -> io::Result<()> {
    if source.is_file() {
        fs::copy(source, destination)?;
    }
    Ok(())
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

    #[test]
    fn copy_leaves_out_git_and_the_root_target() {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        let from = dir.path().join("from");
        for folder in [".git", "target", "src/target", "src/.git"] {
            let Ok(()) = fs::create_dir_all(from.join(folder)) else {
                panic!("create a folder");
            };
        }
        let Ok(()) = fs::write(from.join("src/target/keep.rs"), "x") else {
            panic!("write a file");
        };
        let to = dir.path().join("to");
        let Ok(()) = copy_tree(&from, &to) else {
            panic!("the copy works");
        };
        assert!(to.join("src/target/keep.rs").is_file());
        assert!(!to.join("target").exists());
        assert!(!to.join(".git").exists());
        assert!(!to.join("src/.git").exists());
    }
}
