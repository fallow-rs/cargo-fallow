//! The work folder of one run.
//!
//! The run copies the workspace, so no mutant touches the live checkout. The
//! scratch copy holds the clean source and each rebuild mutant, and the schema
//! copy holds the schema build. Each copy has its own target folder.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::mutation::files::copy_tree;
use crate::mutation::guard::{Guard, Scope};
use crate::mutation::process::{cargo, run_to_files};

/// The time limit of the lock file generation.
const LOCK_LIMIT: Duration = Duration::from_mins(10);

/// The folders of one run.
pub struct WorkDirs {
    /// The work folder.
    pub root: PathBuf,
    /// The clean copy, also the copy for each rebuild.
    pub scratch: PathBuf,
    /// The copy of the schema build.
    pub schema: PathBuf,
    /// The target folder of the scratch copy.
    pub target_scratch: PathBuf,
    /// The target folder of the schema copy.
    pub target_schema: PathBuf,
    /// The target folder of the coverage build of the scratch copy.
    pub target_coverage: PathBuf,
    /// The folder for the output files.
    pub logs: PathBuf,
    /// The files of `.cargo/config.toml` above the workspace, joined in a text.
    pub parent_configs: String,
    /// True when the end of this value removes the work folder.
    remove: bool,
}

impl WorkDirs {
    /// Creates the work folder that only this user can read. The run builds and runs
    /// code from this folder, so `work` must be a new path: an existing folder or a
    /// symlink is an error. With no `work`, the run gets a new temporary folder with a
    /// random name, and it stays when `keep` is true.
    pub fn create(work: Option<&Path>, keep: bool, workspace: &Path) -> io::Result<Self> {
        let remove = work.is_none() && !keep;
        let work = match work {
            Some(work) => {
                create_new_folder(work)?;
                work.to_path_buf()
            }
            None => temporary_folder()?,
        };
        let root = work.canonicalize()?;
        let configs = cargo_configs_above(workspace);
        let parent = copy_parent_configs(&root, &configs)?;
        let parent_configs = configs
            .iter()
            .flat_map(|levels| levels.iter().map(|(_, text)| text.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
        let dirs = Self {
            scratch: parent.join("scratch"),
            schema: parent.join("schema"),
            target_scratch: root.join("target-scratch"),
            target_schema: root.join("target-schema"),
            target_coverage: root.join("target-coverage"),
            logs: root.join("logs"),
            parent_configs,
            root,
            remove,
        };
        fs::create_dir_all(&dirs.logs)?;
        Ok(dirs)
    }

    /// True when the end of the run removes the work folder.
    pub const fn removes_folder(&self) -> bool {
        self.remove
    }
}

impl Drop for WorkDirs {
    /// Removes a temporary folder, also after an error or a panic. A failed removal
    /// only leaves files in the temporary directory of the system.
    fn drop(&mut self) {
        if self.remove {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

/// The mode of a work folder: only the owner can read, write, or enter it.
#[cfg(unix)]
const OWNER_ONLY: u32 = 0o700;

/// Creates `work` and each missing parent. The last step fails when `work` exists,
/// also as a symlink, so no other user can prepare the folder.
fn create_new_folder(work: &Path) -> io::Result<()> {
    if let Some(parent) = work
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    let builder = {
        let mut builder = fs::DirBuilder::new();
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, OWNER_ONLY);
        builder
    };
    #[cfg(not(unix))]
    let builder = fs::DirBuilder::new();
    builder.create(work).map_err(|err| {
        if err.kind() == io::ErrorKind::AlreadyExists {
            io::Error::new(
                err.kind(),
                format!(
                    "the work folder `{}` exists: give a path that does not exist",
                    work.display()
                ),
            )
        } else {
            err
        }
    })
}

/// Creates a new temporary folder with a random name.
fn temporary_folder() -> io::Result<PathBuf> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("cargo-fallow-mutation-");
    #[cfg(unix)]
    builder.permissions(std::os::unix::fs::PermissionsExt::from_mode(OWNER_ONLY));
    Ok(builder.tempdir()?.keep())
}

/// The names of a Cargo config file in a `.cargo` folder.
const CONFIG_NAMES: [&str; 2] = ["config.toml", "config"];

/// The config files of one folder, as the file name and the text.
type ConfigLevel = Vec<(&'static str, String)>;

/// Reads each Cargo config file above the workspace root, outermost first. cargo reads
/// these files, and the copy of the workspace lies elsewhere. The config of the Cargo
/// home stays out, because cargo also reads that file for the copy.
fn cargo_configs_above(workspace: &Path) -> Vec<ConfigLevel> {
    let home = cargo_home();
    let mut levels: Vec<ConfigLevel> = workspace
        .ancestors()
        .skip(1)
        .map(|folder| folder.join(".cargo"))
        .filter(|folder| home.as_ref() != folder.canonicalize().ok().as_ref())
        .map(|folder| {
            CONFIG_NAMES
                .iter()
                .filter_map(|name| Some((*name, fs::read_to_string(folder.join(name)).ok()?)))
                .collect()
        })
        .filter(|level: &ConfigLevel| !level.is_empty())
        .collect();
    levels.reverse();
    levels
}

fn cargo_home() -> Option<PathBuf> {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))?;
    home.canonicalize().ok()
}

/// Writes each config level into its own folder above the copies, in the same order,
/// so that cargo joins them in the same way. Returns the folder of the copies. A
/// relative path in such a file now starts at the new folder.
fn copy_parent_configs(root: &Path, levels: &[ConfigLevel]) -> io::Result<PathBuf> {
    let mut folder = root.to_path_buf();
    for (depth, level) in levels.iter().enumerate() {
        folder = folder.join(format!("up{depth}"));
        let cargo = folder.join(".cargo");
        fs::create_dir_all(&cargo)?;
        for (name, text) in level {
            fs::write(cargo.join(name), text)?;
        }
    }
    Ok(folder)
}

/// Writes the scratch copy with a lock file, then the schema copy from it, so
/// both builds use the same dependency versions.
pub fn prepare_copies(
    root: &Path,
    target_dir: &Path,
    dirs: &WorkDirs,
    guard: &Guard,
) -> io::Result<()> {
    copy_tree(root, &dirs.scratch, Some(target_dir))?;
    if !dirs.scratch.join("Cargo.lock").exists() {
        let mut command = cargo(&dirs.scratch, &dirs.target_scratch, 1);
        command.arg("generate-lockfile");
        let out = dirs.logs.join("lockfile.out");
        let err = dirs.logs.join("lockfile.err");
        run_to_files(command, &out, &err, LOCK_LIMIT, guard, Scope::Build)?;
    }
    copy_tree(&dirs.scratch, &dirs.schema, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary() -> tempfile::TempDir {
        let Ok(dir) = tempfile::tempdir() else {
            panic!("a temporary directory");
        };
        dir
    }

    fn refusal(work: &Path) -> String {
        let workspace = temporary();
        match WorkDirs::create(Some(work), true, workspace.path()) {
            Ok(_) => panic!("`{}` must be refused", work.display()),
            Err(err) => err.to_string(),
        }
    }

    #[test]
    fn an_existing_empty_work_folder_is_refused() {
        let base = temporary();
        let work = base.path().join("run");
        let Ok(()) = fs::create_dir(&work) else {
            panic!("create the folder");
        };
        assert!(refusal(&work).contains("exists"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_as_work_folder_is_refused() {
        let base = temporary();
        let target = base.path().join("target");
        let work = base.path().join("run");
        let Ok(()) = fs::create_dir(&target) else {
            panic!("create the folder");
        };
        let Ok(()) = std::os::unix::fs::symlink(&target, &work) else {
            panic!("create the symlink");
        };
        assert!(refusal(&work).contains("exists"));
        assert!(fs::read_dir(&target).is_ok_and(|mut entries| entries.next().is_none()));
    }

    #[cfg(unix)]
    #[test]
    fn a_new_work_folder_is_for_the_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let base = temporary();
        let workspace = temporary();
        let work = base.path().join("parent").join("run");
        let Ok(dirs) = WorkDirs::create(Some(&work), true, workspace.path()) else {
            panic!("create the work folder");
        };
        let Ok(meta) = fs::metadata(&dirs.root) else {
            panic!("read the work folder");
        };
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn a_temporary_work_folder_is_for_the_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let workspace = temporary();
        let Ok(dirs) = WorkDirs::create(None, false, workspace.path()) else {
            panic!("create the work folder");
        };
        let Ok(meta) = fs::metadata(&dirs.root) else {
            panic!("read the work folder");
        };
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    }
}
