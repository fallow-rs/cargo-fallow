//! The consumer workspaces of the `visibility` check.
//!
//! A consumer workspace is a separate Cargo workspace whose code uses the
//! items of the workspace, for example the cargo-fuzz workspace in `fuzz/`.
//! `consumers` of `[workspace.metadata.fallow.visibility]` lists their
//! manifests. Without the key, `fuzz/Cargo.toml` is a consumer when it exists
//! and is no member. The members of each consumer join the module graph of
//! the check, so their code counts as a use. They get no finding.

use std::path::PathBuf;

use crate::cli::VisibilityOptions;
use crate::error::{CliError, ErrorCode};
use crate::workspace::{self, Workspace};

/// The consumer manifest without a `consumers` key.
const FUZZ: &str = "fuzz/Cargo.toml";

/// The workspace with the members of the consumer workspaces.
#[derive(Debug, Clone)]
pub struct Extended {
    /// The workspace and the consumer members.
    pub workspace: Workspace,
    /// The absolute manifest of each consumer workspace that the run read.
    pub manifests: Vec<PathBuf>,
}

/// The workspace with the members of each consumer workspace, or `None` when
/// the run has no consumer. A consumer of the `consumers` key that does not
/// load is an error. The `fuzz/` default that does not load adds nothing.
pub fn extend(
    workspace: &Workspace,
    options: &VisibilityOptions,
) -> Result<Option<Extended>, CliError> {
    let (names, explicit) = options.consumers.as_ref().map_or_else(
        || (vec![FUZZ.to_owned()], false),
        |names| (names.clone(), true),
    );
    let mut extended = workspace.clone();
    let mut manifests = Vec::new();
    for name in names {
        let manifest = workspace.root.join(&name).into_std_path_buf();
        if workspace
            .members
            .iter()
            .any(|member| member.manifest_path == name)
        {
            continue;
        }
        let members = if manifest.is_file() {
            workspace::consumer_members(&workspace.root, &manifest, &options.cargo_flags)
        } else {
            Err(CliError::new(
                ErrorCode::InvalidConfig,
                format!("the consumer manifest `{name}` does not exist"),
            ))
        };
        let members = match members {
            Ok(members) => members,
            Err(err) if explicit => return Err(invalid(&name, &err)),
            Err(_) => continue,
        };
        for member in members {
            if !extended
                .members
                .iter()
                .any(|known| known.name == member.name)
            {
                extended.members.push(member);
            }
        }
        manifests.push(manifest);
    }
    if manifests.is_empty() {
        return Ok(None);
    }
    extended.members.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.manifest_path.cmp(&b.manifest_path))
    });
    Ok(Some(Extended {
        workspace: extended,
        manifests,
    }))
}

fn invalid(name: &str, err: &CliError) -> CliError {
    CliError::new(
        ErrorCode::InvalidConfig,
        format!(
            "`consumers` of `[workspace.metadata.fallow.visibility]` names `{name}`, which does \
             not load: {}",
            err.message
        ),
    )
    .with_help(
        "Write the path of the `Cargo.toml` of a separate workspace, relative to the workspace \
         root, or remove the entry.",
    )
}
