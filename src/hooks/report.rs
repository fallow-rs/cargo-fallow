//! The JSON report of a block, at `.fallow/cargo-gate-report.json` in the
//! workspace root.
//!
//! The workspace can hold symbolic links, for example a committed `.fallow`
//! link to a folder outside the tree. The gate never writes through a link.
//! When the path or one of its folders is a link, the gate writes the report
//! to a new temporary file and the block message names that file.

use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use crate::check::report::CheckReport;
use crate::section::pretty;

/// The variable that moves the report. A relative path is relative to the
/// workspace root.
pub const REPORT_VARIABLE: &str = "CARGO_FALLOW_GATE_REPORT";

/// The default path of the report, relative to the workspace root.
pub const DEFAULT_REPORT: &str = ".fallow/cargo-gate-report.json";

/// The folder of the default report. It ignores itself in git.
const REPORT_DIR: &str = ".fallow";

/// Where the report went.
#[derive(Debug, PartialEq, Eq)]
pub enum ReportTarget {
    /// The report is at the configured path.
    Written(PathBuf),
    /// The configured path was a link or could not be written. The report is
    /// in this temporary file.
    Fallback(PathBuf),
    /// No file holds the report. The text says why.
    Failed(String),
}

/// Writes the report for a workspace root.
pub fn write(root: &Path, report: &CheckReport) -> ReportTarget {
    let text = match pretty(report) {
        Ok(mut text) => {
            text.push('\n');
            text
        }
        Err(err) => return ReportTarget::Failed(err.message),
    };
    let configured = std::env::var_os(REPORT_VARIABLE).filter(|value| !value.is_empty());
    let default = configured.is_none();
    let relative = configured.map_or_else(|| PathBuf::from(DEFAULT_REPORT), PathBuf::from);
    write_text(root, &relative, default, &text)
}

/// Writes `text` to `relative` under `root`, or to a temporary file when the
/// path is not safe. `default` is true for the default path: the gate then
/// also writes `.fallow/.gitignore`.
pub fn write_text(root: &Path, relative: &Path, default: bool, text: &str) -> ReportTarget {
    match write_no_follow(root, relative, text) {
        Ok(path) => {
            if default {
                ignore_report_dir(root);
            }
            ReportTarget::Written(path)
        }
        Err(_) => match fallback(text) {
            Ok(path) => ReportTarget::Fallback(path),
            Err(err) => ReportTarget::Failed(err.to_string()),
        },
    }
}

/// Writes the file when no existing part of the path is a link. The data goes
/// to a temporary file in the target folder, and a rename then replaces the
/// target. A link that appears at the target after the check is replaced, not
/// followed.
fn write_no_follow(root: &Path, relative: &Path, text: &str) -> io::Result<PathBuf> {
    let path = if relative.is_absolute() {
        relative.to_path_buf()
    } else {
        refuse_links(root, relative)?;
        root.join(relative)
    };
    let refused = || io::Error::other("the report path is a link or a folder");
    if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink() || meta.is_dir())
    {
        return Err(refused());
    }
    let parent = path.parent().ok_or_else(refused)?;
    fs::create_dir_all(parent)?;
    // A link that appeared during the creation of the folders.
    if !relative.is_absolute() {
        refuse_links(root, relative)?;
    }
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(text.as_bytes())?;
    file.persist(&path).map_err(|err| err.error)?;
    Ok(path)
}

/// Fails when an existing part of `relative` under `root` is a link, or when
/// the path leaves the root.
pub fn refuse_links(root: &Path, relative: &Path) -> io::Result<()> {
    let mut prefix = root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(part) => prefix.push(part),
            Component::CurDir => continue,
            _ => return Err(io::Error::other("the report path leaves the workspace")),
        }
        if fs::symlink_metadata(&prefix).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(io::Error::other("the report path holds a link"));
        }
    }
    Ok(())
}

/// Writes `.fallow/.gitignore` with `*` when it does not exist, so that the
/// report never makes the work tree dirty. A link is never written.
fn ignore_report_dir(root: &Path) {
    let ignore = Path::new(REPORT_DIR).join(".gitignore");
    if fs::symlink_metadata(root.join(&ignore)).is_ok() {
        return;
    }
    // A failed write leaves the report in place: the report is the result.
    let _ = write_no_follow(root, &ignore, "*\n");
}

/// Writes the report to a new temporary file that the gate keeps.
fn fallback(text: &str) -> io::Result<PathBuf> {
    let mut file = tempfile::Builder::new()
        .prefix("cargo-fallow-gate-report-")
        .suffix(".json")
        .tempfile()?;
    file.write_all(text.as_bytes())?;
    let (_, path) = file.keep().map_err(|err| err.error)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap_or_else(|err| panic!("{err}"))
    }

    #[test]
    fn the_default_path_is_written_and_ignored() {
        let root = temp();
        let target = write_text(root.path(), Path::new(DEFAULT_REPORT), true, "{}\n");
        assert_eq!(
            target,
            ReportTarget::Written(root.path().join(DEFAULT_REPORT))
        );
        let ignore = fs::read_to_string(root.path().join(".fallow/.gitignore"));
        assert_eq!(ignore.ok().as_deref(), Some("*\n"));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_the_path_is_not_written_through() {
        let root = temp();
        let outside = temp();
        std::os::unix::fs::symlink(outside.path(), root.path().join(".fallow"))
            .unwrap_or_else(|err| panic!("{err}"));
        let target = write_text(root.path(), Path::new(DEFAULT_REPORT), true, "{}\n");
        let ReportTarget::Fallback(path) = target else {
            panic!("the gate must not follow the link: {target:?}");
        };
        assert!(!outside.path().join("cargo-gate-report.json").exists());
        assert!(!outside.path().join(".gitignore").exists());
        assert_eq!(fs::read_to_string(&path).ok().as_deref(), Some("{}\n"));
        let _ = fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_at_the_report_path_is_not_written_through() {
        let root = temp();
        let outside = temp();
        fs::create_dir(root.path().join(".fallow")).unwrap_or_else(|err| panic!("{err}"));
        let victim = outside.path().join("victim");
        fs::write(&victim, "keep").unwrap_or_else(|err| panic!("{err}"));
        std::os::unix::fs::symlink(&victim, root.path().join(DEFAULT_REPORT))
            .unwrap_or_else(|err| panic!("{err}"));
        let target = write_text(root.path(), Path::new(DEFAULT_REPORT), true, "{}\n");
        assert!(matches!(target, ReportTarget::Fallback(_)), "{target:?}");
        assert_eq!(fs::read_to_string(&victim).ok().as_deref(), Some("keep"));
        if let ReportTarget::Fallback(path) = target {
            let _ = fs::remove_file(path);
        }
    }

    #[test]
    fn a_path_that_leaves_the_root_is_refused() {
        let root = temp();
        let target = write_text(root.path(), Path::new("../x.json"), false, "{}\n");
        assert!(matches!(target, ReportTarget::Fallback(_)), "{target:?}");
        if let ReportTarget::Fallback(path) = target {
            let _ = fs::remove_file(path);
        }
    }
}
