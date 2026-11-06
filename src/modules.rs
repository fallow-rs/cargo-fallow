//! The module walk of a target.
//!
//! The walk starts at each target root and follows `mod name;` declarations,
//! with `#[path]` attributes. A declaration with `path` values in `cfg_attr`
//! attributes gives one [`ModuleDecl`] for each file, so the walk reaches each
//! platform file. The caller loads each file and returns the module
//! declarations of that file, so the mutation command and the module graph use
//! one walk. A declaration inside a macro call is not visible to the parser.
//!
//! A `path` attribute on an inline module sets the folder of the modules in
//! it, like rustc does: `#[path = "imp"] mod outer { mod inner; }` loads
//! `imp/inner.rs`. [`ModuleDecl::for_module`] gives one declaration for each
//! combination of folder and file.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use ra_ap_syntax::AstNode;
use ra_ap_syntax::ast::{self, HasName};

use crate::attrs::{ModuleFile, module_files};
use crate::parse::Edition;

/// The folder of the nested modules of an inline module, when a `path`
/// attribute on an enclosing inline module sets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineFolder {
    /// True when the folder starts at the folder of the declaring file. That
    /// is the case for a `path` attribute on an inline module at the top level
    /// of the file. Else the folder starts at the module folder of the file.
    pub from_file_folder: bool,
    /// The folder below the start folder, as the attributes give it.
    pub path: PathBuf,
}

/// A `mod name;` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleDecl {
    /// The module name, without a `r#` prefix.
    pub name: String,
    /// The names of the enclosing inline modules, outermost first.
    pub inline_path: Vec<String>,
    /// The folder of the enclosing inline modules when a `path` attribute
    /// sets it, or `None` for the folder of the names in `inline_path`.
    pub inline_folder: Option<InlineFolder>,
    /// The value of a `#[path]` attribute.
    pub path_attr: Option<String>,
    /// True when a `cfg` attribute other than `cfg(test)` is on the module.
    pub under_cfg: bool,
    /// True when a `cfg` that can be off in a build with all features and
    /// tests is on the module.
    pub cfg_off: bool,
    /// True when the module exists only in a test build, for example with
    /// `#[cfg(test)]`. The source walk of the structure checks sets it for
    /// `#[cfg(test)]`. Each walk sets it for a `cfg_attr(test, path = "..")` file.
    pub test_only: bool,
}

impl ModuleDecl {
    /// One declaration for each file that the `mod name;` item `module` can
    /// load. The `path` attributes of the enclosing inline modules and of
    /// `module` give the candidates. The `cfg` and test flags of each
    /// attribute add to the flags of the declaration.
    pub fn for_module(self, module: &ast::Module) -> Vec<Self> {
        let mut outer: Vec<ast::Module> = module
            .syntax()
            .ancestors()
            .skip(1)
            .filter_map(ast::Module::cast)
            .collect();
        outer.reverse();
        let mut folders = vec![(Folder::default(), ModuleFile::plain())];
        for (depth, inline) in outer.iter().enumerate() {
            let name = inline
                .name()
                .map(|name| crate::attrs::plain_name(&name))
                .unwrap_or_default();
            let choices = module_files(inline);
            let mut next = Vec::with_capacity(folders.len() * choices.len());
            for (folder, flags) in &folders {
                for choice in &choices {
                    next.push((folder.nested(depth, &name, choice), flags.join(choice)));
                }
            }
            folders = next;
        }
        let files = module_files(module);
        let mut decls = Vec::with_capacity(folders.len() * files.len());
        for (folder, flags) in folders {
            let inline_folder = folder.finish();
            for file in &files {
                let file = flags.join(file);
                decls.push(Self {
                    path_attr: file.path,
                    inline_folder: inline_folder.clone(),
                    under_cfg: self.under_cfg || file.under_cfg,
                    cfg_off: self.cfg_off || file.cfg_off,
                    test_only: self.test_only || file.test_only,
                    ..self.clone()
                });
            }
        }
        decls
    }
}

/// The folder of the inline modules so far, relative to the module folder of
/// the file or, after a `path` attribute at the top level, to the folder of
/// the file.
#[derive(Debug, Clone, Default)]
struct Folder {
    /// `None` before the first `path` attribute, else
    /// [`InlineFolder::from_file_folder`].
    moved: Option<bool>,
    path: PathBuf,
}

impl Folder {
    /// The folder of the inline module `name` at `depth` in this folder, with
    /// the `path` value of `choice`. Like rustc, a `path` value replaces the
    /// name, and at the top level it also drops the module folder of the file.
    fn nested(&self, depth: usize, name: &str, choice: &ModuleFile) -> Self {
        match &choice.path {
            None => Self {
                moved: self.moved,
                path: self.path.join(name),
            },
            Some(value) => Self {
                moved: self.moved.or(Some(depth == 0)),
                path: self.path.join(value),
            },
        }
    }

    fn finish(self) -> Option<InlineFolder> {
        self.moved.map(|from_file_folder| InlineFolder {
            from_file_folder,
            path: self.path,
        })
    }
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
///
/// rustc on Windows reads `\` in a `path` value as a folder separator, and
/// rustc on other hosts reads it as part of a name. The walk tries the value as
/// it is, then with `/` for each `\`. A crate for Windows then has the same
/// files on each host.
fn resolve_module(
    workspace: &Path,
    declaring_file: &str,
    dir: &Path,
    module: &ModuleDecl,
) -> Option<(String, bool)> {
    let found = resolve_in(workspace, declaring_file, dir, module, false);
    let has_backslash = module.path_attr.iter().any(|path| path.contains('\\'))
        || module
            .inline_folder
            .as_ref()
            .is_some_and(|folder| folder.path.to_string_lossy().contains('\\'));
    if found.is_some() || !has_backslash {
        return found;
    }
    resolve_in(workspace, declaring_file, dir, module, true)
}

/// [`resolve_module`] with each `\` of the `path` values as `/` when
/// `slashes` is true.
fn resolve_in(
    workspace: &Path,
    declaring_file: &str,
    dir: &Path,
    module: &ModuleDecl,
    slashes: bool,
) -> Option<(String, bool)> {
    let value = |path: &Path| -> PathBuf {
        if slashes {
            PathBuf::from(path.to_string_lossy().replace('\\', "/"))
        } else {
            path.to_path_buf()
        }
    };
    // The folder of the enclosing inline modules: the module folder of the
    // file plus their names, or the folder that a `path` attribute sets.
    let folder = match &module.inline_folder {
        None => {
            let mut base = dir.to_path_buf();
            base.extend(&module.inline_path);
            base
        }
        Some(moved) if moved.from_file_folder => {
            Path::new(declaring_file).parent()?.join(value(&moved.path))
        }
        Some(moved) => dir.join(value(&moved.path)),
    };
    if let Some(attr) = &module.path_attr {
        // Outside an inline module, the path starts at the folder of the
        // declaring file. Inside one, it starts at the folder of the inline
        // module.
        let base = if module.inline_path.is_empty() {
            Path::new(declaring_file).parent()?.to_path_buf()
        } else {
            folder
        };
        let path = normalize(&base.join(value(Path::new(attr))))?;
        return workspace.join(&path).is_file().then_some((path, true));
    }
    let flat = normalize(&folder.join(format!("{}.rs", module.name)))?;
    if workspace.join(&flat).is_file() {
        return Some((flat, false));
    }
    let nested = normalize(&folder.join(&module.name).join("mod.rs"))?;
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
            inline_folder: None,
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

    /// An inline folder as `(from_file_folder, path)`, the `path` value and
    /// `cfg_off` of one declaration.
    type Found = (Option<(bool, String)>, Option<String>, bool);

    /// The declarations of the last `mod name;` item in `text`.
    fn folders_of(text: &str) -> Vec<Found> {
        let file = ast::SourceFile::parse(text, ra_ap_syntax::Edition::Edition2024).tree();
        let Some(module) = file
            .syntax()
            .descendants()
            .filter_map(ast::Module::cast)
            .filter(|module| module.item_list().is_none())
            .last()
        else {
            panic!("a module declaration");
        };
        decl("leaf", &[], None)
            .for_module(&module)
            .into_iter()
            .map(|decl| {
                (
                    decl.inline_folder.map(|folder| {
                        (
                            folder.from_file_folder,
                            folder.path.to_string_lossy().into_owned(),
                        )
                    }),
                    decl.path_attr,
                    decl.cfg_off,
                )
            })
            .collect()
    }

    #[test]
    fn a_path_on_an_inline_module_sets_the_folder_like_rustc() {
        assert_eq!(folders_of("mod a { mod leaf; }"), [(None, None, false)]);
        assert_eq!(
            folders_of("#[path = \"imp\"] mod a { mod leaf; }"),
            [(Some((true, "imp".to_owned())), None, false)]
        );
        // A deeper `path` starts at the folder of the names before it.
        assert_eq!(
            folders_of("mod m { #[path = \"imp\"] mod a { mod b { mod leaf; } } }"),
            [(Some((false, "m/imp/b".to_owned())), None, false)]
        );
        assert_eq!(
            folders_of("#[path = \"x\"] mod a { #[path = \"y\"] mod b { mod leaf; } }"),
            [(Some((true, "x/y".to_owned())), None, false)]
        );
        // Each `cfg_attr` folder is a candidate, next to the default folder.
        assert_eq!(
            folders_of("#[cfg_attr(unix, path = \"u\")] mod a { #[path = \"p.rs\"] mod leaf; }"),
            [
                (None, Some("p.rs".to_owned()), true),
                (Some((true, "u".to_owned())), Some("p.rs".to_owned()), true),
            ]
        );
        assert_eq!(
            folders_of("#[cfg_attr(any(), path = \"u\")] mod a { mod leaf; }"),
            [(None, None, false)]
        );
    }

    fn write_files(root: &Path, paths: &[&str]) {
        for path in paths {
            let full = root.join(path);
            let Some(parent) = full.parent() else {
                panic!("a parent folder");
            };
            let (Ok(()), Ok(())) = (fs::create_dir_all(parent), fs::write(&full, "")) else {
                panic!("write a fixture file");
            };
        }
    }

    #[test]
    fn an_inline_folder_starts_at_the_file_folder_or_the_module_folder() {
        let Ok(tmp) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        write_files(
            tmp.path(),
            &["src/imp/x.rs", "src/a/m/imp/y.rs", "src/imp/z.rs"],
        );
        let dir = Path::new("src/a");
        let moved = |from_file_folder: bool, path: &str, name: &str, attr: Option<&str>| {
            let mut module = decl(name, &["o"], attr);
            module.inline_folder = Some(InlineFolder {
                from_file_folder,
                path: PathBuf::from(path),
            });
            resolve_module(tmp.path(), "src/a.rs", dir, &module)
        };
        assert_eq!(
            moved(true, "imp", "x", None),
            Some(("src/imp/x.rs".to_owned(), false))
        );
        assert_eq!(
            moved(false, "m/imp", "y", None),
            Some(("src/a/m/imp/y.rs".to_owned(), false))
        );
        assert_eq!(
            moved(true, "imp", "other", Some("z.rs")),
            Some(("src/imp/z.rs".to_owned(), true))
        );
    }

    #[test]
    fn a_backslash_path_also_resolves_with_slashes() {
        let Ok(tmp) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        write_files(tmp.path(), &["src/plat/win.rs", "src/win/inner.rs"]);
        let dir = Path::new("src");
        let found = resolve_module(
            tmp.path(),
            "src/lib.rs",
            dir,
            &decl("w", &[], Some("plat\\win.rs")),
        );
        assert_eq!(found, Some(("src/plat/win.rs".to_owned(), true)));
        let mut nested = decl("inner", &["o"], None);
        nested.inline_folder = Some(InlineFolder {
            from_file_folder: true,
            path: PathBuf::from(".\\win"),
        });
        assert_eq!(
            resolve_module(tmp.path(), "src/lib.rs", dir, &nested),
            Some(("src/win/inner.rs".to_owned(), false))
        );
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
