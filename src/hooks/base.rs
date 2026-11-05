//! The findings of the base commit, so that the gate blocks only on new
//! findings.
//!
//! The gate writes the tree of the base commit to a temporary folder and runs
//! `check` there with `--save-baseline`. The work-tree run then reads that
//! file with `--baseline`, so each finding that the base commit already has
//! drops out before the gates count.
//!
//! The export starts at the git top level, so a workspace in a subfolder and
//! a path dependency outside it are in the export. It uses a temporary index
//! and `git checkout-index`, so `export-ignore` attributes cannot leave out a
//! file, and the repository does not change.
//!
//! The result is a cache in the git folder (`git rev-parse --git-path`),
//! keyed by the base commit, the tool version and the workspace folder. A
//! commit cannot add a file there. A retry after a block then runs `check`
//! one time. The cache keeps the newest files only.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::baseline::Loaded;
use crate::check;
use crate::diff::git;
use crate::report::TOOL_VERSION;

/// The folder of the cache in the git folder.
const CACHE_DIR: &str = "cargo-fallow-gate";

/// The number of cache files that stay. A commit and a push of one branch use
/// two base commits, and two workspaces double that.
const KEPT_CACHES: usize = 4;

/// The baseline file of the findings of `commit`, from the cache or from a
/// new run. `options` are the options of the work-tree run. The error says
/// why the file is not there.
pub fn baseline(root: &Path, commit: &str, options: &check::Options) -> Result<PathBuf, String> {
    let line = |args: &[&str]| {
        git(root, args)
            .map(|out| String::from_utf8_lossy(&out).trim().to_owned())
            .map_err(|detail| format!("git {} failed ({})", args[0], detail.trim()))
    };
    let top = PathBuf::from(line(&["rev-parse", "--show-toplevel"])?);
    let prefix = line(&["rev-parse", "--show-prefix"])?;
    let cache_dir = absolute(root, &line(&["rev-parse", "--git-path", CACHE_DIR])?);
    let key = xxhash_rust::xxh3::xxh3_64(prefix.as_bytes());
    let cache = cache_dir.join(format!("base-{commit}-{TOOL_VERSION}-{key:016x}.json"));
    if Loaded::read(&cache).is_ok() {
        return Ok(cache);
    }
    let export = tempfile::tempdir().map_err(|err| format!("cannot make a folder ({err})"))?;
    let tree = export.path().join("tree");
    export_commit(&top, commit, export.path(), &tree)?;
    let saved = export.path().join("baseline.json");
    let base_options = check::Options {
        manifest_path: Some(tree.join(&prefix).join("Cargo.toml")),
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
    match keep(&cache_dir, &cache, &text) {
        Ok(path) => Ok(path),
        // Without a cache, the file stays in the temporary folder.
        Err(_) => Ok(export.keep().join("baseline.json")),
    }
}

/// A path that git printed, relative to `root` when it is not absolute.
fn absolute(root: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

/// Writes the cache file and removes the oldest files beyond
/// [`KEPT_CACHES`].
fn keep(dir: &Path, cache: &Path, text: &str) -> std::io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(text.as_bytes())?;
    file.persist(cache).map_err(|err| err.error)?;
    let mut caches: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(dir)?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path != cache && path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| Some((fs::metadata(&path).ok()?.modified().ok()?, path)))
        .collect();
    caches.sort();
    let extra = caches.len().saturating_sub(KEPT_CACHES - 1);
    for (_, path) in caches.into_iter().take(extra) {
        // A file that another gate removed first is not an error.
        let _ = fs::remove_file(path);
    }
    Ok(cache.to_path_buf())
}

/// Writes the tree of `commit` to `tree`, with a temporary index in
/// `scratch`.
fn export_commit(top: &Path, commit: &str, scratch: &Path, tree: &Path) -> Result<(), String> {
    let index = scratch.join("index");
    let mut prefix = OsString::from("--prefix=");
    prefix.push(tree.as_os_str());
    prefix.push("/");
    let exported = run(top, &index, &["read-tree".as_ref(), commit.as_ref()])
        && run(
            top,
            &index,
            &["checkout-index".as_ref(), "-a".as_ref(), prefix.as_os_str()],
        );
    if exported {
        Ok(())
    } else {
        Err("cannot export the base commit".to_owned())
    }
}

/// Runs git in `top` with the index file `index`. True on success.
fn run(top: &Path, index: &Path, args: &[&OsStr]) -> bool {
    #[expect(
        clippy::disallowed_methods,
        reason = "the gate exports the base commit with its own index file"
    )]
    let mut command = Command::new("git");
    for name in crate::diff::SCRUBBED_VARIABLES {
        command.env_remove(name);
    }
    command
        .env("GIT_INDEX_FILE", index)
        .arg("-C")
        .arg(top)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}
