//! The files that the hooks commands write: generated scripts with a
//! checksum, and the outcome of each file.
//!
//! Each generated script holds a marker line and a checksum line. The
//! checksum covers the script without the checksum line. A script whose
//! checksum matches is a generated script that nobody edited, so a new
//! version can replace it. Any other file at the path is a user file: install
//! refuses it and uninstall keeps it, unless the user gives `--force`.

use std::fs;
use std::io;
use std::path::Path;

use serde::Serialize;

/// The prefix of the checksum line of a script.
const CHECKSUM_PREFIX: &str = "# Checksum: ";

/// The prefix and the suffix of the checksum line of a Markdown file.
const MARKDOWN_CHECKSUM: (&str, &str) = ("<!-- Checksum: ", " -->");

/// The outcome for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// The file did not exist and the command wrote it.
    Created,
    /// The command replaced an older version of the file.
    Updated,
    /// The file already had the content.
    Unchanged,
    /// The command removed the file or the handler.
    Removed,
    /// The file is a user file, and the command kept it.
    Kept,
    /// The file does not exist, and there was nothing to remove.
    NotPresent,
}

impl Outcome {
    /// The verb for the human output.
    pub const fn verb(self, dry_run: bool) -> &'static str {
        match (self, dry_run) {
            (Self::Created, false) => "Created",
            (Self::Created, true) => "Would create",
            (Self::Updated, false) => "Updated",
            (Self::Updated, true) => "Would update",
            (Self::Unchanged, _) => "Unchanged",
            (Self::Removed, false) => "Removed",
            (Self::Removed, true) => "Would remove",
            (Self::Kept, false) => "Kept (edited, use --force)",
            (Self::Kept, true) => "Would keep (edited, use --force)",
            (Self::NotPresent, _) => "Absent",
        }
    }
}

/// The state of a script on disk.
#[derive(Debug, PartialEq, Eq)]
pub enum ScriptState {
    /// No file.
    Missing,
    /// A generated script with a matching checksum. The text is the
    /// installer version line, when the script has one.
    Generated(Option<String>),
    /// Any other file.
    User,
}

/// Adds the checksum line after the marker line of `script`. A marker in
/// an HTML comment gets the checksum in an HTML comment, for Markdown.
pub fn with_checksum(script: &str, marker: &str) -> String {
    let sum = checksum(script);
    let line = if marker.starts_with("<!--") {
        format!("{}{sum}{}", MARKDOWN_CHECKSUM.0, MARKDOWN_CHECKSUM.1)
    } else {
        format!("{CHECKSUM_PREFIX}{sum}")
    };
    script.replacen(&format!("{marker}\n"), &format!("{marker}\n{line}\n"), 1)
}

/// The checksum of a checksum line, or `None` for another line.
fn checksum_of(line: &str) -> Option<&str> {
    let line = line.trim_end();
    line.strip_prefix(CHECKSUM_PREFIX).or_else(|| {
        line.strip_prefix(MARKDOWN_CHECKSUM.0)?
            .strip_suffix(MARKDOWN_CHECKSUM.1)
    })
}

/// The checksum of a text: 16 hex digits of the XXH3 hash.
fn checksum(text: &str) -> String {
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(text.as_bytes()))
}

/// Reads the state of the script at `path`. A link is a user file.
pub fn script_state(path: &Path, marker: &str) -> io::Result<ScriptState> {
    match fs::symlink_metadata(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(ScriptState::Missing),
        Err(err) => return Err(err),
        Ok(meta) if !meta.is_file() => return Ok(ScriptState::User),
        Ok(_) => {}
    }
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(ScriptState::User);
    };
    Ok(classify(&text, marker))
}

/// The state of a script text.
pub fn classify(text: &str, marker: &str) -> ScriptState {
    let mut recorded = None;
    let mut body = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if recorded.is_none()
            && let Some(sum) = checksum_of(line)
        {
            recorded = Some(sum.to_owned());
            continue;
        }
        body.push_str(line);
    }
    let generated = text.lines().any(|line| line == marker)
        && recorded.is_some_and(|sum| sum == checksum(&body));
    if !generated {
        return ScriptState::User;
    }
    ScriptState::Generated(
        text.lines()
            .find_map(|line| line.strip_prefix("# Installer version: "))
            .map(str::to_owned),
    )
}

/// Writes an executable script. A user file stays unless `force` is true.
pub fn install_script(
    path: &Path,
    content: &str,
    marker: &str,
    force: bool,
    dry_run: bool,
) -> Result<Outcome, String> {
    let state = script_state(path, marker).map_err(|err| read_error(path, &err))?;
    let outcome = match state {
        ScriptState::Missing => Outcome::Created,
        ScriptState::User if !force => {
            return Err(format!(
                "{} exists and is not a script that cargo-fallow generated, or someone edited it. Run again with --force to replace it.",
                path.display()
            ));
        }
        _ if fs::read_to_string(path).is_ok_and(|old| old == content) => Outcome::Unchanged,
        _ => Outcome::Updated,
    };
    if dry_run || outcome == Outcome::Unchanged {
        return Ok(outcome);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| write_error(parent, &err))?;
    }
    // A link at the path is replaced, not followed.
    if fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        fs::remove_file(path).map_err(|err| write_error(path, &err))?;
    }
    fs::write(path, content).map_err(|err| write_error(path, &err))?;
    set_executable(path).map_err(|err| write_error(path, &err))?;
    Ok(outcome)
}

/// Removes a generated script. A user file stays unless `force` is true.
pub fn remove_script(
    path: &Path,
    marker: &str,
    force: bool,
    dry_run: bool,
) -> Result<Outcome, String> {
    let state = script_state(path, marker).map_err(|err| read_error(path, &err))?;
    match state {
        ScriptState::Missing => Ok(Outcome::NotPresent),
        ScriptState::User if !force => Ok(Outcome::Kept),
        _ => {
            if !dry_run {
                fs::remove_file(path).map_err(|err| write_error(path, &err))?;
            }
            Ok(Outcome::Removed)
        }
    }
}

/// The message of a failed read.
pub fn read_error(path: &Path, err: &io::Error) -> String {
    format!("cannot read {}: {err}", path.display())
}

/// The message of a failed write.
pub fn write_error(path: &Path, err: &io::Error) -> String {
    format!("cannot write {}: {err}", path.display())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the Unix form of the function can fail"
)]
const fn set_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARKER: &str = "# Generated by test.";

    fn script() -> String {
        with_checksum(
            &format!("#!/bin/sh\n{MARKER}\n# Installer version: 1.2.3\necho hi\n"),
            MARKER,
        )
    }

    #[test]
    fn a_generated_script_is_recognized_by_its_checksum() {
        assert_eq!(
            classify(&script(), MARKER),
            ScriptState::Generated(Some("1.2.3".to_owned()))
        );
    }

    #[test]
    fn a_markdown_file_gets_its_checksum_in_a_comment() {
        let marker = "<!-- Generated by test. -->";
        let text = with_checksum(&format!("---\nname: x\n---\n{marker}\nbody\n"), marker);
        assert!(text.contains("<!-- Checksum: "), "{text}");
        assert_eq!(classify(&text, marker), ScriptState::Generated(None));
        assert_eq!(
            classify(&text.replace("body", "edit"), marker),
            ScriptState::User
        );
    }

    #[test]
    fn an_edit_or_a_missing_marker_makes_a_user_file() {
        let edited = script().replace("echo hi", "echo bye");
        assert_eq!(classify(&edited, MARKER), ScriptState::User);
        assert_eq!(classify("#!/bin/sh\necho hi\n", MARKER), ScriptState::User);
    }

    #[test]
    fn install_refuses_a_user_file_without_force() {
        let dir = tempfile::tempdir().unwrap_or_else(|err| panic!("{err}"));
        let path = dir.path().join("gate.sh");
        fs::write(&path, "#!/bin/sh\nmine\n").unwrap_or_else(|err| panic!("{err}"));
        assert!(install_script(&path, &script(), MARKER, false, false).is_err());
        assert_eq!(
            fs::read_to_string(&path).ok().as_deref(),
            Some("#!/bin/sh\nmine\n")
        );
        assert_eq!(
            install_script(&path, &script(), MARKER, true, false),
            Ok(Outcome::Updated)
        );
        assert_eq!(
            install_script(&path, &script(), MARKER, false, false),
            Ok(Outcome::Unchanged)
        );
    }

    #[test]
    fn remove_keeps_a_user_file_without_force() {
        let dir = tempfile::tempdir().unwrap_or_else(|err| panic!("{err}"));
        let path = dir.path().join("gate.sh");
        fs::write(&path, "mine\n").unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            remove_script(&path, MARKER, false, false),
            Ok(Outcome::Kept)
        );
        assert!(path.exists());
        assert_eq!(
            remove_script(&path, MARKER, true, false),
            Ok(Outcome::Removed)
        );
        assert!(!path.exists());
    }
}
