//! The git reads of the size check: the changed files and their old versions.
//!
//! This module is the one place in the structure checks that spawns a
//! process. Each command runs `git` in the workspace root and only reads.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::{CliError, ErrorCode};

/// One changed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The path now, relative to the workspace root.
    pub path: String,
    /// The path in the old version. It differs for a rename.
    pub old_path: String,
    /// The blob of the new version, for `git cat-file`.
    pub new_spec: String,
    /// The blobs of the old versions. The largest one is the base.
    pub old_specs: Vec<String>,
}

/// The changes to compare and the name of the old version.
#[derive(Debug, Clone)]
pub struct Changes {
    /// The name of the old version for the messages.
    pub label: String,
    /// The changed files.
    pub files: Vec<Change>,
    /// The changed paths that are not UTF-8 text, with the bytes that are not
    /// valid replaced. The growth rule cannot check them.
    pub skipped: Vec<String>,
}

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

/// The arguments of the diff: added, changed and renamed files, with renames.
const DIFF_ARGS: [&str; 6] = [
    "diff",
    "--name-status",
    "-z",
    "-M",
    "--diff-filter=AMR",
    "--no-ext-diff",
];

/// The files that HEAD changes since the merge base of HEAD and `base`.
pub fn since_base(root: &Path, base: &str) -> Result<Changes, CliError> {
    let commit = verify_base(root, base)?;
    let merge_base = git(root, &["merge-base", "HEAD", commit.as_str()])
        .map_err(|detail| {
            // `git merge-base` exits with 1 and no message when it finds no
            // common commit.
            let detail = if detail.trim().is_empty() {
                "git found no common commit"
            } else {
                detail.as_str()
            };
            let error = cannot_read(&format!("the merge base with `{base}`"), detail);
            if is_shallow(root) {
                error.with_help(
                    "The clone is shallow, so the merge base can be missing. Fetch more history (for example `git fetch --unshallow`).",
                )
            } else {
                error
            }
        })
        .and_then(|output| text(output, "the merge base"))?;
    let merge_base = merge_base.trim().to_owned();
    let mut args = DIFF_ARGS.to_vec();
    args.extend(["--relative", merge_base.as_str(), "HEAD"]);
    let listed = git(root, &args)
        .map_err(|detail| cannot_read(&format!("the diff with `{base}`"), &detail))?;
    let short: String = merge_base.chars().take(12).collect();
    let (listed, skipped) = parse_name_status(&listed);
    Ok(Changes {
        label: format!("merge base {short}"),
        skipped,
        files: listed
            .into_iter()
            .map(|(path, old_path)| Change {
                new_spec: format!("HEAD:./{path}"),
                old_specs: vec![format!("{merge_base}:./{old_path}")],
                path,
                old_path,
            })
            .collect(),
    })
}

/// The staged files, compared with HEAD. During a merge, the old versions are
/// also in MERGE_HEAD.
pub fn staged(root: &Path) -> Result<Changes, CliError> {
    let has_head = has_commit(root, "HEAD");
    let (listed, skipped) = if has_head {
        let mut args = DIFF_ARGS.to_vec();
        args.extend(["--relative", "--cached", "HEAD"]);
        let output =
            git(root, &args).map_err(|detail| cannot_read("the staged changes", &detail))?;
        parse_name_status(&output)
    } else {
        let output = git(root, &["ls-files", "-z", "--cached"])
            .map_err(|detail| cannot_read("the staged changes", &detail))?;
        let mut listed = Vec::new();
        let mut skipped = Vec::new();
        for field in output
            .split(|&byte| byte == 0)
            .filter(|field| !field.is_empty())
        {
            match std::str::from_utf8(field) {
                Ok(path) => listed.push((path.to_owned(), path.to_owned())),
                Err(_) => skipped.push(String::from_utf8_lossy(field).into_owned()),
            }
        }
        (listed, skipped)
    };
    let merging = has_head && has_commit(root, "MERGE_HEAD");
    Ok(Changes {
        skipped,
        label: if merging {
            "HEAD or MERGE_HEAD"
        } else {
            "HEAD"
        }
        .to_owned(),
        files: listed
            .into_iter()
            .map(|(path, old_path)| {
                let mut old_specs = Vec::new();
                if has_head {
                    old_specs.push(format!("HEAD:./{old_path}"));
                }
                if merging {
                    old_specs.push(format!("MERGE_HEAD:./{old_path}"));
                    old_specs.push(format!("MERGE_HEAD:./{path}"));
                }
                Change {
                    new_spec: format!(":./{path}"),
                    old_specs,
                    path,
                    old_path,
                }
            })
            .collect(),
    })
}

/// The text of a blob, or `None` when the blob does not exist: the file is
/// new, or the old version did not have it. A byte that is not valid UTF-8
/// becomes a replacement character, so the line count stays the same. A git
/// failure is an error.
pub fn blob(root: &Path, spec: &str) -> Result<Option<String>, CliError> {
    let Some(object) = object_id(root, spec)? else {
        return Ok(None);
    };
    let bytes = git(root, &["cat-file", "blob", object.as_str()])
        .map_err(|detail| cannot_read(&format!("`{spec}`"), &detail))?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// The object id of `spec`, or `None` when git reports no such object.
fn object_id(root: &Path, spec: &str) -> Result<Option<String>, CliError> {
    let output = run_git(
        root,
        &["rev-parse", "--verify", "--quiet", "--end-of-options", spec],
    )
    .map_err(|detail| cannot_read(&format!("`{spec}`"), &detail))?;
    if output.status.success() {
        let id = text(output.stdout, "an object id")?;
        return Ok(Some(id.trim().to_owned()));
    }
    // `--verify --quiet` exits with 1 and no message for a missing object.
    if output.status.code() == Some(1) && output.stderr.is_empty() {
        return Ok(None);
    }
    Err(cannot_read(
        &format!("`{spec}`"),
        &String::from_utf8_lossy(&output.stderr),
    ))
}

/// The commit of `base`. A value that starts with `-` is an error, because git
/// would read it as an option.
fn verify_base(root: &Path, base: &str) -> Result<String, CliError> {
    if base.starts_with('-') || base.is_empty() {
        return Err(CliError::new(
            ErrorCode::InvalidArgument,
            format!("`--base` must name a git ref, not `{base}`"),
        )
        .with_help("Give a branch, a tag or a commit, for example `--base origin/main`."));
    }
    let spec = format!("{base}^{{commit}}");
    match object_id(root, &spec)? {
        Some(commit) => Ok(commit),
        None => Err(CliError::new(
            ErrorCode::InvalidArgument,
            format!("`{base}` is not a commit that git knows"),
        )
        .with_help("Give a ref that git knows. Run `git fetch` when the ref is on a remote.")),
    }
}

/// True when the repository is a shallow clone.
fn is_shallow(root: &Path) -> bool {
    git(root, &["rev-parse", "--is-shallow-repository"])
        .is_ok_and(|output| output.trim_ascii() == b"true")
}

/// The output as text, or an error that names `subject`.
fn text(output: Vec<u8>, subject: &str) -> Result<String, CliError> {
    String::from_utf8(output).map_err(|_| {
        CliError::new(
            ErrorCode::InvalidArgument,
            format!("cannot read {subject}: the git output is not UTF-8 text"),
        )
    })
}

fn has_commit(root: &Path, name: &str) -> bool {
    git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{name}^{{commit}}"),
        ],
    )
    .is_ok()
}

/// Reads `git diff --name-status -z` output as `(path, old path)` pairs. The
/// second list holds each entry with a path that is not UTF-8 text.
pub fn parse_name_status(output: &[u8]) -> (Vec<(String, String)>, Vec<String>) {
    let fields: Vec<&[u8]> = output.split(|&byte| byte == 0).collect();
    let mut found = Vec::new();
    let mut skipped = Vec::new();
    let mut index = 0;
    let mut add =
        |new: &[u8], old: &[u8]| match (std::str::from_utf8(new), std::str::from_utf8(old)) {
            (Ok(new), Ok(old)) => found.push((new.to_owned(), old.to_owned())),
            _ => skipped.push(String::from_utf8_lossy(new).into_owned()),
        };
    while let Some(status) = fields.get(index).filter(|field| !field.is_empty()) {
        if status.starts_with(b"R") || status.starts_with(b"C") {
            let (Some(old), Some(new)) = (fields.get(index + 1), fields.get(index + 2)) else {
                break;
            };
            add(new, old);
            index += 3;
        } else {
            let Some(path) = fields.get(index + 1) else {
                break;
            };
            add(path, path);
            index += 2;
        }
    }
    (found, skipped)
}

fn cannot_read(subject: &str, detail: &str) -> CliError {
    CliError::new(
        ErrorCode::InvalidArgument,
        format!("cannot read {subject}: {}", detail.trim()),
    )
    .with_help("Run the command in a git work tree, and give a ref that git knows.")
}

/// Runs git in `root`. The error is the stderr text or the spawn error.
fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = run_git(root, args)?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(output.stdout)
}

/// Runs git in `root` with a clean environment: no variable of
/// [`SCRUBBED_VARIABLES`], and no optional locks, so a read does not write
/// the index.
fn run_git(root: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the size check reads the changed files and the old blobs from git"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_status_reads_plain_and_renamed_entries() {
        let output = b"M\0a.rs\0R087\0old.rs\0new.rs\0A\0b.rs\0";
        assert_eq!(
            parse_name_status(output).0,
            vec![
                ("a.rs".to_owned(), "a.rs".to_owned()),
                ("new.rs".to_owned(), "old.rs".to_owned()),
                ("b.rs".to_owned(), "b.rs".to_owned()),
            ]
        );
        assert!(parse_name_status(b"").0.is_empty());
    }

    #[test]
    fn name_status_skips_an_entry_with_a_path_that_is_not_utf8() {
        let output = b"M\0a.rs\0A\0bad\xff.rs\0R100\0old\xfe.rs\0new.rs\0A\0b.rs\0";
        let (found, skipped) = parse_name_status(output);
        assert_eq!(
            found,
            vec![
                ("a.rs".to_owned(), "a.rs".to_owned()),
                ("b.rs".to_owned(), "b.rs".to_owned()),
            ]
        );
        assert_eq!(
            skipped,
            vec!["bad\u{fffd}.rs".to_owned(), "new.rs".to_owned()]
        );
    }
}
