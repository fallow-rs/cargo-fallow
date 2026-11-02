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
    /// Creates the work folder. An existing folder with content is an error, so
    /// the command never writes over other files. With no `work`, the run gets a new
    /// temporary folder that only this user can read, and it stays when `keep` is true.
    pub fn create(work: Option<&Path>, keep: bool, workspace: &Path) -> io::Result<Self> {
        let remove = work.is_none() && !keep;
        let work = match work {
            Some(work) => {
                if fs::read_dir(work).is_ok_and(|mut entries| entries.next().is_some()) {
                    return Err(io::Error::other(format!(
                        "the work folder `{}` is not empty",
                        work.display()
                    )));
                }
                fs::create_dir_all(work)?;
                work.to_path_buf()
            }
            None => tempfile::Builder::new()
                .prefix("cargo-fallow-mutation-")
                .tempdir()?
                .keep(),
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
