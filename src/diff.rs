//! The git layer: one way to run `git`, and the files that a branch changed.
//!
//! Each command runs `git` in the workspace root and only reads. The
//! environment loses each variable that can move git to another repository.
//! Each failure stops the run, because a run with the wrong scope looks like a
//! clean result.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde::Serialize;

use crate::error::{CliError, ErrorCode};
use crate::workspace::Workspace;

/// The variables that move git to another repository, index or object store.
/// A hook or a parent process can set them, so each command removes them.
const SCRUBBED_VARIABLES: [&str; 9] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
];

/// The name of the manifest file of a package.
const MANIFEST: &str = "Cargo.toml";

/// Runs git in `root` with a clean environment: no variable of
/// [`SCRUBBED_VARIABLES`], and no optional locks, so a read does not write
/// the index. The error is the spawn error.
pub fn run_git(root: &Path, args: &[&str]) -> Result<Output, String> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the git layer reads the changed files and the old blobs"
    )]
    let mut command = Command::new("git");
    for name in SCRUBBED_VARIABLES {
        command.env_remove(name);
    }
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("git does not run: {err}"))
}

/// Runs git in `root`. The error is the stderr text or the spawn error.
pub fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = run_git(root, args)?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(output.stdout)
}

/// True when the repository is a shallow clone.
pub fn is_shallow(root: &Path) -> bool {
    git(root, &["rev-parse", "--is-shallow-repository"])
        .is_ok_and(|output| output.trim_ascii() == b"true")
}

/// The files that the working tree changed since the fork point of a ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangedFiles {
    /// The ref of the flag.
    #[serde(rename = "ref")]
    pub reference: String,
    /// The fork point of the ref and `HEAD`: the commit that the diff starts
    /// from.
    pub base: String,
    /// The changed files, relative to the workspace root, sorted.
    #[serde(rename = "changed_files")]
    pub files: BTreeSet<String>,
    /// The manifests of the packages that own a changed file.
    #[serde(skip)]
    manifests: BTreeSet<String>,
}

impl ChangedFiles {
    /// Asks git for the files that changed since the fork point of
    /// `reference`. A committed, a staged, an unstaged and an untracked change
    /// count. `flag` names the flag in the messages.
    pub fn load(workspace: &Workspace, flag: &str, reference: &str) -> Result<Self, CliError> {
        let root = workspace.root.as_std_path();
        let base = fork_point(root, flag, reference)?;
        let tracked = git(
            root,
            &[
                "diff",
                "--name-only",
                "-z",
                "--no-renames",
                "--no-ext-diff",
                "--relative",
                base.as_str(),
                "--",
            ],
        )
        .map_err(|detail| cannot_read(flag, "the changed files", &detail))?;
        let untracked = git(root, &["ls-files", "--others", "--exclude-standard", "-z"])
            .map_err(|detail| cannot_read(flag, "the untracked files", &detail))?;
        let changed_files: BTreeSet<String> = paths(&tracked).chain(paths(&untracked)).collect();
        Ok(Self::new(workspace, reference, base, changed_files))
    }

    /// The changed files with the manifests that own them.
    pub fn new(
        workspace: &Workspace,
        reference: &str,
        base: String,
        changed_files: BTreeSet<String>,
    ) -> Self {
        let dirs: Vec<(&str, &str)> = workspace
            .members
            .iter()
            .map(|member| {
                let dir = member
                    .manifest_path
                    .strip_suffix(MANIFEST)
                    .unwrap_or_default();
                (dir, member.manifest_path.as_str())
            })
            .collect();
        let manifests = changed_files
            .iter()
            .map(|path| owner(&dirs, path))
            .collect();
        Self {
            reference: reference.to_owned(),
            base,
            files: changed_files,
            manifests,
        }
    }

    /// True when the scope keeps a finding at `path`. A finding in a
    /// `Cargo.toml` counts when a file of its package changed: the manifest, or
    /// a file under the package folder that no nested member owns.
    pub fn keeps(&self, path: &str) -> bool {
        self.files.contains(path) || (is_manifest(path) && self.manifests.contains(path))
    }

    /// True when the scope keeps a finding with one of these places.
    pub fn keeps_any<'a>(&self, mut paths: impl Iterator<Item = &'a str>) -> bool {
        paths.any(|path| self.keeps(path))
    }
}

/// True for the path of a `Cargo.toml`.
fn is_manifest(path: &str) -> bool {
    path == MANIFEST || path.ends_with("/Cargo.toml")
}

/// The manifest of the member with the deepest folder that holds `path`, or the
/// root `Cargo.toml` when no member folder holds it.
fn owner(dirs: &[(&str, &str)], path: &str) -> String {
    dirs.iter()
        .filter(|(dir, _)| path.starts_with(dir))
        .max_by_key(|(dir, _)| dir.len())
        .map_or_else(
            || MANIFEST.to_owned(),
            |(_, manifest)| (*manifest).to_owned(),
        )
}

/// The paths of NUL-separated git output. A path that is not UTF-8 text gets a
/// replacement character, so it matches no finding.
fn paths(output: &[u8]) -> impl Iterator<Item = String> + '_ {
    output
        .split(|&byte| byte == 0)
        .filter(|field| !field.is_empty())
        .map(|field| String::from_utf8_lossy(field).into_owned())
}

/// The fork point of `reference` and `HEAD`. A ref that starts with `-` is an
/// error, because git would read it as an option.
fn fork_point(root: &Path, flag: &str, reference: &str) -> Result<String, CliError> {
    if reference.is_empty() || reference.starts_with('-') {
        return Err(CliError::new(
            ErrorCode::InvalidArgument,
            format!("`{flag}` must name a git ref, not `{reference}`"),
        )
        .with_help("Give a branch, a tag or a commit, for example `origin/main`."));
    }
    let spec = format!("{reference}^{{commit}}");
    let output = run_git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &spec,
        ],
    )
    .map_err(|detail| cannot_read(flag, "the ref", &detail))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let error = if detail.trim().is_empty() {
            CliError::new(
                ErrorCode::InvalidArgument,
                format!("`{flag} {reference}`: git does not know this ref"),
            )
            .with_help("Give a ref that git knows. Run `git fetch` when the ref is on a remote.")
        } else {
            cannot_read(flag, "the ref", &detail)
        };
        return Err(error);
    }
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let base = git(root, &["merge-base", commit.as_str(), "HEAD"]).map_err(|detail| {
        // `git merge-base` exits with 1 and no message when it finds no
        // common commit.
        let detail = if detail.trim().is_empty() {
            "git found no common commit with `HEAD`"
        } else {
            detail.as_str()
        };
        let error = cannot_read(flag, &format!("the fork point of `{reference}`"), detail);
        if is_shallow(root) {
            error.with_help(
                "The clone is shallow, so the fork point can be missing. Fetch more history (for example `git fetch --unshallow`).",
            )
        } else {
            error
        }
    })?;
    Ok(String::from_utf8_lossy(&base).trim().to_owned())
}

fn cannot_read(flag: &str, subject: &str, detail: &str) -> CliError {
    CliError::new(
        ErrorCode::InvalidArgument,
        format!("`{flag}`: cannot read {subject}: {}", detail.trim()),
    )
    .with_help("Run the command in a git work tree, and give a ref that git knows.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(files: &[&str]) -> ChangedFiles {
        let dirs = [
            ("", "Cargo.toml"),
            ("crates/a/", "crates/a/Cargo.toml"),
            ("crates/a/nested/", "crates/a/nested/Cargo.toml"),
        ];
        let changed_files: BTreeSet<String> = files.iter().map(|&path| path.to_owned()).collect();
        ChangedFiles {
            reference: "main".to_owned(),
            base: "c".to_owned(),
            manifests: changed_files
                .iter()
                .map(|path| owner(&dirs, path))
                .collect(),
            files: changed_files,
        }
    }

    #[test]
    fn a_manifest_finding_counts_when_a_file_of_its_package_changed() {
        let scope = changed(&["crates/a/src/lib.rs"]);
        assert!(scope.keeps("crates/a/src/lib.rs"));
        assert!(!scope.keeps("crates/a/src/other.rs"));
        assert!(scope.keeps("crates/a/Cargo.toml"));
        assert!(!scope.keeps("crates/a/nested/Cargo.toml"));
        assert!(!scope.keeps("Cargo.toml"));
    }

    #[test]
    fn a_nested_member_owns_its_own_files() {
        let scope = changed(&["crates/a/nested/src/lib.rs"]);
        assert!(scope.keeps("crates/a/nested/Cargo.toml"));
        assert!(!scope.keeps("crates/a/Cargo.toml"));
        let root = changed(&["build.rs"]);
        assert!(root.keeps("Cargo.toml"));
        assert!(!root.keeps("crates/a/Cargo.toml"));
    }

    #[test]
    fn nul_separated_output_gives_each_path() {
        let found: Vec<String> = paths(b"a.rs\0b c.rs\0\0bad\xff.rs\0").collect();
        assert_eq!(found, ["a.rs", "b c.rs", "bad\u{fffd}.rs"]);
    }
}
