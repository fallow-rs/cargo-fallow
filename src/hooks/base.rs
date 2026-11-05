//! The findings of the base commit, so that the gate blocks only on new
//! findings.
//!
//! The gate exports the base commit with `git archive` to a temporary folder
//! and runs `check` there with `--save-baseline`. The work-tree run then reads
//! that file with `--baseline`, so each finding that the base commit already
//! has drops out before the gates count. The file is a cache in `.fallow/`,
//! keyed by the base commit and the tool version: a retry after a block runs
//! `check` one time.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::report::write_text;
use crate::check;
use crate::diff::git;
use crate::report::TOOL_VERSION;

/// The prefix of the cache file name.
const CACHE_PREFIX: &str = "cargo-gate-base-";

/// The folder of the cache, relative to the workspace root.
const CACHE_DIR: &str = ".fallow";

/// The baseline file of the findings of `commit`, from the cache or from a
/// new run. `options` are the options of the work-tree run. The error says
/// why the file is not there.
pub fn baseline(root: &Path, commit: &str, options: &check::Options) -> Result<PathBuf, String> {
    let name = format!("{CACHE_PREFIX}{commit}-{TOOL_VERSION}.json");
    let cache = root.join(CACHE_DIR).join(&name);
    if fs::symlink_metadata(&cache).is_ok_and(|meta| meta.is_file()) {
        return Ok(cache);
    }
    let export = tempfile::tempdir().map_err(|err| format!("cannot make a folder ({err})"))?;
    let prefix = git(root, &["rev-parse", "--show-prefix"])
        .map(|out| String::from_utf8_lossy(&out).trim().to_owned())
        .map_err(|detail| format!("git rev-parse failed ({})", detail.trim()))?;
    export_commit(root, commit, export.path())?;
    let saved = export.path().join("baseline.json");
    let base_options = check::Options {
        manifest_path: Some(export.path().join(&prefix).join("Cargo.toml")),
        base: None,
        staged: false,
        scope: crate::scope::ScopeArgs {
            save_baseline: Some(saved.clone()),
            ..crate::scope::ScopeArgs::default()
        },
        ..options.clone()
    };
    check::execute(&base_options)
        .map_err(|err| format!("the check of the base commit stopped ({})", err.message))?;
    let text =
        fs::read_to_string(&saved).map_err(|err| format!("cannot read the base result ({err})"))?;
    remove_old_caches(root);
    match write_text(root, &Path::new(CACHE_DIR).join(&name), true, &text) {
        super::report::ReportTarget::Written(path)
        | super::report::ReportTarget::Fallback(path) => Ok(path),
        super::report::ReportTarget::Failed(detail) => {
            Err(format!("cannot keep the base result ({detail})"))
        }
    }
}

/// Removes the cache files of other commits.
fn remove_old_caches(root: &Path) {
    let Ok(entries) = fs::read_dir(root.join(CACHE_DIR)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_cache = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(CACHE_PREFIX))
            && path.extension().is_some_and(|ext| ext == "json");
        if is_cache && entry.file_type().is_ok_and(|kind| !kind.is_dir()) {
            // A file that another process removed first is not an error.
            let _ = fs::remove_file(path);
        }
    }
}

/// Writes the tree of `commit` to `dir` with `git archive` and `tar`.
fn export_commit(root: &Path, commit: &str, dir: &Path) -> Result<(), String> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the gate exports the base commit to compare the findings"
    )]
    let mut archive = Command::new("git");
    for name in crate::diff::SCRUBBED_VARIABLES {
        archive.env_remove(name);
    }
    let mut archive = archive
        .arg("-C")
        .arg(root)
        .args(["archive", "--format=tar", commit])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("git archive does not run ({err})"))?;
    let Some(stream) = archive.stdout.take() else {
        return Err("git archive gave no output".to_owned());
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the gate unpacks the export of the base commit"
    )]
    let unpacked = Command::new("tar")
        .arg("-x")
        .arg("-f")
        .arg("-")
        .arg("-C")
        .arg(dir)
        .stdin(stream)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|err| format!("tar does not run ({err})"))?;
    let archived = archive
        .wait()
        .map_err(|err| format!("git archive failed ({err})"))?;
    if !archived.success() || !unpacked.success() {
        return Err("cannot export the base commit".to_owned());
    }
    Ok(())
}
