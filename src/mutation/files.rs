//! Finds the source files of a package and copies a source tree.
//!
//! The walk in [`crate::modules`] starts at each target root and follows
//! `mod name;` declarations, so a file that no module declares does not add
//! mutants that no compile checks. The report counts each file that the walk
//! does not reach.

use std::fs;
use std::io;
use std::path::Path;

pub use crate::modules::{SkippedFile, TargetRoot, normalize};
use crate::modules::{Visit, walk};
use crate::mutation::sites::{FileSites, find_sites};
use crate::parse::Edition;

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
    /// True when the file is a target root, which holds the helper.
    pub is_root: bool,
    /// True when a `cfg` attribute is on a module declaration of the file.
    pub under_cfg: bool,
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
    let walked = walk(workspace, targets, |visit| {
        let file = load_file(workspace, visit)?;
        let modules = file.found.modules.clone();
        Ok((file, modules))
    });
    Collected {
        files: walked.files,
        skipped: walked.skipped,
        unreached: walked.unreached,
    }
}

fn load_file(workspace: &Path, visit: &Visit) -> Result<CrateFile, String> {
    let original = fs::read_to_string(workspace.join(&visit.path))
        .map_err(|err| format!("cannot read the file: {err}"))?;
    let found = find_sites(&original, visit.edition, &visit.path)
        .map_err(|outcome| format!("the parse failed: {outcome:?}"))?;
    Ok(CrateFile {
        path: visit.path.clone(),
        original,
        edition: visit.edition,
        found,
        is_root: visit.is_root,
        under_cfg: visit.under_cfg,
    })
}

/// Copies a source tree. The copy leaves out each `.git` folder, the `target`
/// folder at the root and the folder `skip`, and it copies a symbolic link as a
/// link. `skip` is the real target folder, which can have another name.
pub fn copy_tree(from: &Path, to: &Path, skip: Option<&Path>) -> io::Result<()> {
    fs::create_dir_all(to)?;
    copy_entries(from, to, true, skip)
}

fn copy_entries(from: &Path, to: &Path, at_root: bool, skip: Option<&Path>) -> io::Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let kind = entry.file_type()?;
        let (source, destination) = (entry.path(), to.join(&name));
        let left_out = kind.is_dir()
            && (name == ".git"
                || (at_root && name == "target")
                || skip.is_some_and(|skip| skip == source));
        if left_out {
            continue;
        }
        if kind.is_dir() {
            fs::create_dir(&destination)?;
            copy_entries(&source, &destination, false, skip)?;
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
        let Ok(()) = copy_tree(&from, &to, None) else {
            panic!("the copy works");
        };
        assert!(to.join("src/target/keep.rs").is_file());
        assert!(!to.join("target").exists());
        assert!(!to.join(".git").exists());
        assert!(!to.join("src/.git").exists());
    }
}
