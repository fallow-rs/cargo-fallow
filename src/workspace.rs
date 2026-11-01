//! The workspace model, built from `cargo metadata --no-deps`.

use std::path::{Path, PathBuf};

use cargo_metadata::camino::{Utf8Path, Utf8PathBuf};
use cargo_metadata::{MetadataCommand, Package, Target as CargoTarget};
use serde::Serialize;

use crate::error::{CliError, ErrorCode};

/// The workspace that one run analyzes.
#[derive(Debug, Clone)]
pub struct Workspace {
    /// The absolute workspace root. It is not part of the report.
    pub root: Utf8PathBuf,
    /// The absolute target folder. It is not part of the report.
    pub target_dir: Utf8PathBuf,
    /// The members, sorted by name.
    pub members: Vec<Member>,
}

/// One workspace member.
#[derive(Debug, Clone, Serialize)]
pub struct Member {
    /// The package name.
    pub name: String,
    /// The package version.
    pub version: String,
    /// The `Cargo.toml` path, relative to the workspace root.
    pub manifest_path: String,
    /// The targets, sorted by kind and then by name.
    pub targets: Vec<Target>,
}

/// One target of a member.
#[derive(Debug, Clone, Serialize)]
pub struct Target {
    /// The target name.
    pub name: String,
    /// The target kinds, for example `lib`, `bin` or `test`.
    pub kind: Vec<String>,
    /// The root source file, relative to the workspace root.
    pub src_path: String,
    /// The Rust edition of the target.
    pub edition: String,
}

/// Reads the workspace. Cargo does not resolve dependencies, does not use the
/// network and does not write `Cargo.lock`.
pub fn load(manifest_path: Option<&Path>) -> Result<Workspace, CliError> {
    let mut command = MetadataCommand::new();
    command.no_deps();
    if let Some(path) = manifest_path {
        check_manifest(path)?;
        command.manifest_path(path);
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "the workspace loader is the one module that spawns a process: `cargo metadata`"
    )]
    let metadata = command.exec().map_err(|err| metadata_error(&err))?;
    let root = metadata.workspace_root.clone();
    let mut members: Vec<Member> = metadata
        .workspace_packages()
        .into_iter()
        .map(|package| member(&root, package))
        .collect();
    members.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.manifest_path.cmp(&b.manifest_path))
    });
    Ok(Workspace {
        root,
        target_dir: metadata.target_directory,
        members,
    })
}

fn check_manifest(path: &Path) -> Result<(), CliError> {
    if path.is_file() {
        return Ok(());
    }
    Err(CliError::new(
        ErrorCode::ManifestNotFound,
        format!("the manifest `{}` does not exist", path.display()),
    )
    .with_help("Pass the path to a Cargo.toml file with --manifest-path."))
}

fn metadata_error(err: &cargo_metadata::Error) -> CliError {
    let message = match err {
        cargo_metadata::Error::CargoMetadata { stderr } => {
            first_cargo_error(stderr).unwrap_or_else(|| err.to_string())
        }
        _ => err.to_string(),
    };
    CliError::new(
        ErrorCode::MetadataFailed,
        format!("cargo metadata failed: {message}"),
    )
    .with_help("Run `cargo metadata --no-deps` in the workspace to see the full error.")
}

/// The first error line of the cargo stderr, with its location when cargo
/// prints one on the next line.
fn first_cargo_error(stderr: &str) -> Option<String> {
    let mut lines = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let first = lines.next()?;
    let message = first.strip_prefix("error: ").unwrap_or(first);
    let location = lines.next().and_then(|line| line.strip_prefix("--> "));
    Some(location.map_or_else(
        || message.to_owned(),
        |location| format!("{message} ({location})"),
    ))
}

fn member(root: &Utf8Path, package: &Package) -> Member {
    let mut targets: Vec<Target> = package
        .targets
        .iter()
        .map(|target| target_of(root, target))
        .collect();
    targets.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));
    Member {
        name: package.name.to_string(),
        version: package.version.to_string(),
        manifest_path: relative_path(root, &package.manifest_path),
        targets,
    }
}

fn target_of(root: &Utf8Path, target: &CargoTarget) -> Target {
    Target {
        name: target.name.clone(),
        kind: target.kind.iter().map(ToString::to_string).collect(),
        src_path: relative_path(root, &target.src_path),
        edition: target.edition.as_str().to_owned(),
    }
}

/// The path relative to `root`, with `/` separators. A path outside `root`
/// stays absolute, with `/` separators.
pub fn relative_path(root: &Utf8Path, path: &Utf8Path) -> String {
    path.strip_prefix(root).map_or_else(
        |_| path.as_str().replace(std::path::MAIN_SEPARATOR, "/"),
        |relative| {
            relative
                .components()
                .map(|part| part.as_str())
                .collect::<Vec<_>>()
                .join("/")
        },
    )
}

/// The absolute path of a relative report path.
pub fn absolute_path(root: &Utf8Path, relative: &str) -> PathBuf {
    root.join(relative).into_std_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_error_keeps_its_location() {
        let stderr = "error: unclosed table, expected `]`\n --> /w/Cargo.toml:1:9\n  |\n";
        assert_eq!(
            first_cargo_error(stderr).as_deref(),
            Some("unclosed table, expected `]` (/w/Cargo.toml:1:9)")
        );
        assert_eq!(first_cargo_error("\n\n"), None);
        assert_eq!(
            first_cargo_error("error: no members\nCaused by:\n").as_deref(),
            Some("no members")
        );
    }

    #[test]
    fn path_inside_root_becomes_relative() {
        let root = Utf8Path::new("/work/repo");
        let path = Utf8Path::new("/work/repo/crates/a/src/lib.rs");
        assert_eq!(relative_path(root, path), "crates/a/src/lib.rs");
    }

    #[test]
    fn root_manifest_is_a_bare_file_name() {
        let root = Utf8Path::new("/work/repo");
        assert_eq!(
            relative_path(root, Utf8Path::new("/work/repo/Cargo.toml")),
            "Cargo.toml"
        );
    }

    #[test]
    fn path_outside_root_stays_absolute() {
        let root = Utf8Path::new("/work/repo");
        assert_eq!(
            relative_path(root, Utf8Path::new("/other/x/lib.rs")),
            "/other/x/lib.rs"
        );
    }

    #[test]
    fn prefix_match_needs_a_whole_component() {
        let root = Utf8Path::new("/work/repo");
        assert_eq!(
            relative_path(root, Utf8Path::new("/work/repo-b/lib.rs")),
            "/work/repo-b/lib.rs"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_separators_become_slashes() {
        let root = Utf8Path::new(r"C:\work\repo");
        assert_eq!(
            relative_path(root, Utf8Path::new(r"C:\work\repo\crates\a\lib.rs")),
            "crates/a/lib.rs"
        );
    }
}
