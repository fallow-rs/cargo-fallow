//! The workspace model, built from `cargo metadata`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use cargo_metadata::camino::{Utf8Path, Utf8PathBuf};
use cargo_metadata::{DependencyKind, MetadataCommand, Package, Target as CargoTarget};
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
    /// The `[workspace.metadata]` table of the root manifest, or `null`. It is
    /// not part of the report.
    pub metadata: serde_json::Value,
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
    /// The names of the direct dependencies. They are not part of the report.
    #[serde(skip)]
    pub dependencies: Vec<String>,
    /// The direct dependencies with their kind and name in code. They are not
    /// part of the report.
    #[serde(skip)]
    pub links: Vec<Link>,
    /// The names of the features of the package. They are not part of the report.
    #[serde(skip)]
    pub features: Vec<String>,
}

/// The section of `Cargo.toml` that declares a dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// `[dependencies]`.
    Normal,
    /// `[dev-dependencies]`.
    Dev,
    /// `[build-dependencies]`.
    Build,
}

/// One direct dependency of a member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The package name of the dependency.
    pub package: String,
    /// The `package = "..."` rename: the key in `Cargo.toml`, when it differs
    /// from the package name.
    pub rename: Option<String>,
    /// The section of the dependency.
    pub kind: LinkKind,
    /// The `cfg` or target triple of a `[target.'...'.dependencies]` table.
    pub platform: Option<String>,
    /// True for an optional dependency, which a feature enables.
    pub optional: bool,
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
    /// The features that the target needs. They are not part of the report.
    #[serde(skip)]
    pub required_features: Vec<String>,
    /// True when rustdoc tests the documentation of the target. It is not part
    /// of the report.
    #[serde(skip)]
    pub doctest: bool,
}

/// Reads the workspace. Cargo does not resolve dependencies, does not use the
/// network and does not write `Cargo.lock`.
pub fn load(manifest_path: Option<&Path>) -> Result<Workspace, CliError> {
    metadata(manifest_path, None).map(|metadata| model(&metadata))
}

/// Reads the workspace and resolves the dependencies with all features on.
/// Cargo can use the network. `cargo_flags` go to `cargo metadata`, for
/// example `--offline`. `Cargo.lock` stays as it was before the call: the
/// function removes a new lock file and writes back the old bytes of a
/// changed one. The raw metadata has the library names and the `links` keys
/// of the dependencies.
pub fn load_resolved(
    manifest_path: Option<&Path>,
    cargo_flags: &[String],
) -> Result<(Workspace, cargo_metadata::Metadata), CliError> {
    let root = metadata(manifest_path, None)?.workspace_root;
    let lock = LockGuard::new(root.join(LOCK_FILE).into_std_path_buf())?;
    let resolved = metadata(manifest_path, Some(cargo_flags));
    lock.restore()?;
    let metadata = resolved?;
    Ok((model(&metadata), metadata))
}

/// The name of the lock file in the workspace root.
const LOCK_FILE: &str = "Cargo.lock";

/// The bytes of the lock file before a call that can write it.
struct LockGuard {
    path: PathBuf,
    before: Option<Vec<u8>>,
}

impl LockGuard {
    fn new(path: PathBuf) -> Result<Self, CliError> {
        let before = match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(lock_error("read", &path, &err)),
        };
        Ok(Self { path, before })
    }

    /// Puts the lock file back in its state before the call.
    fn restore(self) -> Result<(), CliError> {
        let after = match fs::read(&self.path) {
            Ok(bytes) => Some(bytes),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(lock_error("read", &self.path, &err)),
        };
        if after == self.before {
            return Ok(());
        }
        match &self.before {
            Some(bytes) => {
                fs::write(&self.path, bytes).map_err(|err| lock_error("restore", &self.path, &err))
            }
            None => {
                fs::remove_file(&self.path).map_err(|err| lock_error("remove", &self.path, &err))
            }
        }
    }
}

fn lock_error(action: &str, path: &Path, err: &io::Error) -> CliError {
    CliError::new(
        ErrorCode::MetadataFailed,
        format!("cannot {action} `{}`: {err}", path.display()),
    )
}

/// Runs `cargo metadata`. `resolve` is `None` for a run without the
/// dependencies, or the extra flags for a run that resolves them with all
/// features on.
fn metadata(
    manifest_path: Option<&Path>,
    resolve: Option<&[String]>,
) -> Result<cargo_metadata::Metadata, CliError> {
    let mut command = MetadataCommand::new();
    match resolve {
        Some(flags) => {
            command.features(cargo_metadata::CargoOpt::AllFeatures);
            command.other_options(flags.to_vec());
        }
        None => {
            command.no_deps();
        }
    }
    if let Some(path) = manifest_path {
        check_manifest(path)?;
        command.manifest_path(path);
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "the workspace loader is the one module that spawns a process: `cargo metadata`"
    )]
    let metadata = command
        .exec()
        .map_err(|err| metadata_error(&err, resolve.is_some()))?;
    Ok(metadata)
}

fn model(metadata: &cargo_metadata::Metadata) -> Workspace {
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
    Workspace {
        root,
        target_dir: metadata.target_directory.clone(),
        members,
        metadata: metadata.workspace_metadata.clone(),
    }
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

fn metadata_error(err: &cargo_metadata::Error, resolve: bool) -> CliError {
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
    .with_help(if resolve {
        "The command resolves the dependencies with all features on, so a feature \
         that needs an unavailable dependency fails it. Run `cargo metadata \
         --all-features --format-version 1` in the workspace to see the full error."
    } else {
        "Run `cargo metadata --no-deps` in the workspace to see the full error."
    })
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
        dependencies: package
            .dependencies
            .iter()
            .map(|dependency| dependency.name.clone())
            .collect(),
        links: package.dependencies.iter().map(link_of).collect(),
        features: package.features.keys().cloned().collect(),
    }
}

fn link_of(dependency: &cargo_metadata::Dependency) -> Link {
    Link {
        package: dependency.name.clone(),
        rename: dependency.rename.clone(),
        kind: match dependency.kind {
            DependencyKind::Development => LinkKind::Dev,
            DependencyKind::Build => LinkKind::Build,
            _ => LinkKind::Normal,
        },
        platform: dependency.target.as_ref().map(ToString::to_string),
        optional: dependency.optional,
    }
}

fn target_of(root: &Utf8Path, target: &CargoTarget) -> Target {
    Target {
        name: target.name.clone(),
        kind: target.kind.iter().map(ToString::to_string).collect(),
        src_path: relative_path(root, &target.src_path),
        edition: target.edition.as_str().to_owned(),
        required_features: target.required_features.clone(),
        doctest: target.doctest,
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

/// Runs `cargo check --workspace --all-targets` with `flags`, for example
/// `--offline`, and with `--all-features` when `all_features` is true.
/// Returns each error line of the compiler on a failure.
pub fn cargo_check(
    manifest_path: Option<&Path>,
    root: &Path,
    flags: &[String],
    all_features: bool,
) -> Result<(), String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    #[expect(
        clippy::disallowed_methods,
        reason = "the workspace loader is the one module that spawns cargo: `cargo check` after `fix`"
    )]
    let mut command = std::process::Command::new(cargo);
    // A relative `--manifest-path` names a file from the current folder, the
    // same as for `cargo metadata`, so only a run without it starts at the root.
    if manifest_path.is_none() {
        command.current_dir(root);
    }
    if all_features {
        command.arg("--all-features");
    }
    command
        .args([
            "check",
            "--workspace",
            "--all-targets",
            "--quiet",
            "--message-format",
            "short",
        ])
        .args(flags);
    if let Some(path) = manifest_path {
        command.arg("--manifest-path").arg(path);
    }
    let output = command
        .output()
        .map_err(|err| format!("cannot run `cargo check`: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let errors: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("error") || line.contains(": error"))
        .collect();
    Err(if errors.is_empty() {
        format!("`cargo check` failed with {}", output.status)
    } else {
        errors.join("\n")
    })
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
