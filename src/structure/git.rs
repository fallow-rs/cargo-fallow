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
    pub changes: Vec<Change>,
}

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
    let merge_base = git(root, &["merge-base", "HEAD", base])
        .map_err(|detail| cannot_read(&format!("the merge base with `{base}`"), &detail))?;
    let merge_base = merge_base.trim().to_owned();
    let mut args = DIFF_ARGS.to_vec();
    args.extend(["--relative", merge_base.as_str(), "HEAD"]);
    let listed = git(root, &args)
        .map_err(|detail| cannot_read(&format!("the diff with `{base}`"), &detail))?;
    let short: String = merge_base.chars().take(12).collect();
    Ok(Changes {
        label: format!("merge base {short}"),
        changes: parse_name_status(&listed)
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
    let listed = if has_head {
        let mut args = DIFF_ARGS.to_vec();
        args.extend(["--relative", "--cached", "HEAD"]);
        let output =
            git(root, &args).map_err(|detail| cannot_read("the staged changes", &detail))?;
        parse_name_status(&output)
    } else {
        let output = git(root, &["ls-files", "-z", "--cached"])
            .map_err(|detail| cannot_read("the staged changes", &detail))?;
        output
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(|path| (path.to_owned(), path.to_owned()))
            .collect()
    };
    let merging = has_head && has_commit(root, "MERGE_HEAD");
    Ok(Changes {
        label: if merging {
            "HEAD or MERGE_HEAD"
        } else {
            "HEAD"
        }
        .to_owned(),
        changes: listed
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

/// The text of a blob, or `None` when the blob does not exist.
pub fn blob(root: &Path, spec: &str) -> Option<String> {
    git(root, &["cat-file", "blob", spec]).ok()
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

/// Reads `git diff --name-status -z` output as `(path, old path)` pairs.
pub fn parse_name_status(output: &str) -> Vec<(String, String)> {
    let fields: Vec<&str> = output.split('\0').collect();
    let mut found = Vec::new();
    let mut index = 0;
    while let Some(status) = fields.get(index).filter(|field| !field.is_empty()) {
        if status.starts_with('R') || status.starts_with('C') {
            let (Some(old), Some(new)) = (fields.get(index + 1), fields.get(index + 2)) else {
                break;
            };
            found.push(((*new).to_owned(), (*old).to_owned()));
            index += 3;
        } else {
            let Some(path) = fields.get(index + 1) else {
                break;
            };
            found.push(((*path).to_owned(), (*path).to_owned()));
            index += 2;
        }
    }
    found
}

fn cannot_read(subject: &str, detail: &str) -> CliError {
    CliError::new(
        ErrorCode::InvalidArgument,
        format!("cannot read {subject}: {}", detail.trim()),
    )
    .with_help("Run the command in a git work tree, and give a ref that git knows.")
}

/// Runs git in `root`. The error is the stderr text or the spawn error.
fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the size check reads the changed files and the old blobs from git"
    )]
    let mut command = Command::new("git");
    let output = command
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("git does not run: {err}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    String::from_utf8(output.stdout).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_status_reads_plain_and_renamed_entries() {
        let output = "M\0a.rs\0R087\0old.rs\0new.rs\0A\0b.rs\0";
        assert_eq!(
            parse_name_status(output),
            vec![
                ("a.rs".to_owned(), "a.rs".to_owned()),
                ("new.rs".to_owned(), "old.rs".to_owned()),
                ("b.rs".to_owned(), "b.rs".to_owned()),
            ]
        );
        assert!(parse_name_status("").is_empty());
    }
}
