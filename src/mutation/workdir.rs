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
    /// True when the run created a temporary folder.
    temporary: bool,
}

impl WorkDirs {
    /// Creates the work folder. An existing folder with content is an error, so
    /// the command never writes over other files.
    pub fn create(work: Option<&Path>) -> io::Result<Self> {
        let temporary = work.is_none();
        let work = work.map_or_else(
            || {
                let name = format!("cargo-fallow-mutation-{}", std::process::id());
                std::env::temp_dir().join(name)
            },
            Path::to_path_buf,
        );
        if fs::read_dir(&work).is_ok_and(|mut entries| entries.next().is_some()) {
            return Err(io::Error::other(format!(
                "the work folder `{}` is not empty",
                work.display()
            )));
        }
        fs::create_dir_all(&work)?;
        let root = work.canonicalize()?;
        let dirs = Self {
            scratch: root.join("scratch"),
            schema: root.join("schema"),
            target_scratch: root.join("target-scratch"),
            target_schema: root.join("target-schema"),
            target_coverage: root.join("target-coverage"),
            logs: root.join("logs"),
            root,
            temporary,
        };
        fs::create_dir_all(&dirs.logs)?;
        Ok(dirs)
    }

    /// Removes a temporary folder. A failed removal only leaves files in the
    /// temporary directory of the system.
    pub fn finish(&self, keep: bool) {
        if self.temporary && !keep {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
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
