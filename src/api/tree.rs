//! The source tree of the merge base, in a temporary folder.
//!
//! `git archive` writes the `.rs` files and the `Cargo.toml` files of the
//! merge base as a tar stream, and this module writes them to a temporary
//! folder. git only reads: the command changes no index, no ref and no work
//! tree.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::diff::git;
use crate::error::{CliError, ErrorCode};

/// The size of one tar block.
const BLOCK: usize = 512;
/// The pathspecs of the files that `cargo metadata` and the module graph read.
const PATHSPECS: [&str; 2] = [":(glob)**/*.rs", ":(glob)**/Cargo.toml"];

/// The tree of the merge base.
pub struct BaseTree {
    /// The folder. It goes away when the value drops.
    _dir: TempDir,
    /// The workspace root in the folder.
    pub root: PathBuf,
}

/// Writes the tree of `commit` to a temporary folder. `workspace` is the
/// workspace root in the working tree.
pub fn checkout(workspace: &Path, commit: &str) -> Result<BaseTree, CliError> {
    let top = text(
        git(workspace, &["rev-parse", "--show-toplevel"]),
        "the repository root",
    )?;
    let prefix = text(
        git(workspace, &["rev-parse", "--show-prefix"]),
        "the workspace folder",
    )?;
    let top = PathBuf::from(top.trim());
    let mut args = vec!["archive", "--format=tar", commit, "--"];
    args.extend(PATHSPECS);
    let tar =
        git(&top, &args).map_err(|detail| fail(&format!("git archive {commit}: {detail}")))?;
    let dir = tempfile::Builder::new()
        .prefix("cargo-fallow-api-")
        .tempdir()
        .map_err(|err| fail(&format!("cannot make a temporary folder: {err}")))?;
    untar(&tar, dir.path()).map_err(|detail| fail(&detail))?;
    let root = dir.path().join(prefix.trim());
    Ok(BaseTree { _dir: dir, root })
}

fn text(output: Result<Vec<u8>, String>, subject: &str) -> Result<String, CliError> {
    let bytes = output.map_err(|detail| fail(&format!("cannot read {subject}: {detail}")))?;
    String::from_utf8(bytes).map_err(|_| fail(&format!("{subject} is not UTF-8 text")))
}

fn fail(detail: &str) -> CliError {
    CliError::new(
        ErrorCode::InvalidArgument,
        format!("cannot read the tree of the merge base: {detail}"),
    )
}

/// Writes the regular files of a tar stream below `dest`. A path that leaves
/// `dest` is an error. Links and other entries do not count.
fn untar(bytes: &[u8], dest: &Path) -> Result<(), String> {
    let mut position = 0;
    let mut long_name: Option<String> = None;
    while position + BLOCK <= bytes.len() {
        let header = &bytes[position..position + BLOCK];
        if header.iter().all(|byte| *byte == 0) {
            break;
        }
        let size = octal(&header[124..136]).ok_or("the tar stream has a bad size field")?;
        let start = position + BLOCK;
        let end = start
            .checked_add(size)
            .filter(|end| *end <= bytes.len())
            .ok_or("the tar stream ends inside an entry")?;
        let data = &bytes[start..end];
        let name = long_name.take().unwrap_or_else(|| {
            let prefix = field(&header[345..500]);
            let name = field(&header[..100]);
            if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            }
        });
        match header[156] {
            b'x' => long_name = pax_path(data),
            b'0' | 0 => write_entry(dest, &name, data)?,
            _ => {}
        }
        position = start + size.div_ceil(BLOCK) * BLOCK;
    }
    Ok(())
}

fn write_entry(dest: &Path, name: &str, data: &[u8]) -> Result<(), String> {
    let relative = crate::modules::normalize(Path::new(name))
        .filter(|path| !path.is_empty() && !Path::new(name).is_absolute())
        .ok_or_else(|| format!("the tar stream has the path `{name}` outside the tree"))?;
    let path = dest.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("cannot write `{name}`: {err}"))?;
    }
    fs::write(&path, data).map_err(|err| format!("cannot write `{name}`: {err}"))
}

/// The text of a tar field, up to the first zero byte.
fn field(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn octal(bytes: &[u8]) -> Option<usize> {
    let text = field(bytes);
    let text = text.trim_matches(|c: char| c == ' ' || c == '\0');
    if text.is_empty() {
        return Some(0);
    }
    usize::from_str_radix(text, 8).ok()
}

/// The `path` record of a pax header: lines of `<length> <key>=<value>`.
fn pax_path(data: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(data);
    text.lines().find_map(|line| {
        let (_, record) = line.split_once(' ')?;
        record.strip_prefix("path=").map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &str, size: usize, kind: u8) -> Vec<u8> {
        let mut block = vec![0_u8; BLOCK];
        block[..name.len()].copy_from_slice(name.as_bytes());
        let size = format!("{size:011o}\0");
        block[124..136].copy_from_slice(size.as_bytes());
        block[156] = kind;
        block
    }

    fn entry(name: &str, data: &[u8], kind: u8) -> Vec<u8> {
        let mut bytes = header(name, data.len(), kind);
        bytes.extend_from_slice(data);
        bytes.resize(bytes.len().div_ceil(BLOCK) * BLOCK, 0);
        bytes
    }

    #[test]
    fn untar_writes_files_and_reads_a_long_pax_name() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|err| err.to_string())?;
        let long = format!("{}/lib.rs", "deep".repeat(30));
        let mut tar = entry("pax_global_header", b"52 comment=abc\n", b'g');
        tar.extend(entry("src/", b"", b'5'));
        tar.extend(entry("src/main.rs", b"fn main() {}\n", b'0'));
        let record = format!("{} path={long}\n", long.len() + 12);
        tar.extend(entry("PaxHeader", record.as_bytes(), b'x'));
        tar.extend(entry("ignored-name", b"pub fn f() {}\n", b'0'));
        tar.extend(vec![0; BLOCK * 2]);
        untar(&tar, dir.path())?;
        let main =
            fs::read_to_string(dir.path().join("src/main.rs")).map_err(|err| err.to_string())?;
        assert_eq!(main, "fn main() {}\n");
        assert!(dir.path().join(long).is_file());
        Ok(())
    }

    #[test]
    fn untar_refuses_a_path_outside_the_tree() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|err| err.to_string())?;
        let mut tar = entry("../evil.rs", b"x", b'0');
        tar.extend(vec![0; BLOCK * 2]);
        assert!(untar(&tar, dir.path()).is_err());
        Ok(())
    }
}
