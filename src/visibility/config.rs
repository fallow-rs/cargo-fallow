//! The `[workspace.metadata.fallow.visibility]` table.
//!
//! ```toml
//! [workspace.metadata.fallow.visibility]
//! production = ["app:app"]
//! exclude-crates = ["my_api"]
//! public-crates = ["my_api", "my_types"]
//! uniform-fields = true
//! kinds = ["dead-public", "unnecessary-public"]
//! ```
//!
//! A flag on the command line wins over the table: a list flag replaces the
//! list of the table, and `--uniform-fields` turns the option on. An unknown
//! key is an error, because a misspelled key must not pass without notice.

use serde::Deserialize;

use crate::cli::VisibilityOptions;
use crate::error::{CliError, ErrorCode};
use crate::visibility::FindingKind;
use crate::workspace::Workspace;

/// The table key under `[workspace.metadata.fallow]`.
const KEY: &str = "visibility";

/// The table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
struct Config {
    /// The production binaries, as `PACKAGE:BIN`.
    production: Vec<String>,
    /// The library crates whose public items are an external API.
    exclude_crates: Vec<String>,
    /// The libraries whose exported items are an external API. Each other
    /// library is internal.
    public_crates: Option<Vec<String>>,
    /// True when the fields of one struct change visibility as a group.
    uniform_fields: bool,
    /// The finding kinds to report.
    kinds: Vec<KindName>,
}

/// A finding kind, with the spelling of the `--kind` flag.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum KindName {
    #[serde(alias = "dead_public")]
    DeadPublic,
    #[serde(alias = "unnecessary_public")]
    UnnecessaryPublic,
    #[serde(alias = "unnecessary_restricted_visibility")]
    UnnecessaryRestrictedVisibility,
}

impl From<KindName> for FindingKind {
    fn from(name: KindName) -> Self {
        match name {
            KindName::DeadPublic => Self::DeadPublic,
            KindName::UnnecessaryPublic => Self::UnnecessaryPublic,
            KindName::UnnecessaryRestrictedVisibility => Self::UnnecessaryRestricted,
        }
    }
}

/// The `exclude-crates` list and the `public-crates` patterns of the table,
/// or `None` when the table is not valid. The `deps` check reads them for the
/// `unused-feature` rule: such a library has users outside the workspace.
pub fn api_crates(workspace: &Workspace) -> Option<(Vec<String>, Vec<String>)> {
    crate::structure::config::section::<Config>(workspace, KEY)
        .ok()
        .map(|config| {
            (
                config.exclude_crates,
                config.public_crates.unwrap_or_default(),
            )
        })
}

/// The `exclude-crates` list and the `public-crates` patterns of the table,
/// for `api`. The patterns are `None` when the table has no `public-crates`.
/// A pattern that matches no library is an error.
pub fn external_api(workspace: &Workspace) -> Result<(Vec<String>, Option<Vec<String>>), CliError> {
    let config: Config = crate::structure::config::section(workspace, KEY)?;
    if let Some(patterns) = &config.public_crates {
        check_public(workspace, patterns)?;
    }
    Ok((config.exclude_crates, config.public_crates))
}

/// Fails when a `public-crates` pattern matches no library of the workspace.
/// A typo would make each library internal without notice.
fn check_public(workspace: &Workspace, patterns: &[String]) -> Result<(), CliError> {
    for pattern in patterns {
        let matched = workspace.members.iter().any(|member| {
            member
                .targets
                .iter()
                .filter(|target| {
                    target
                        .kind
                        .iter()
                        .any(|kind| kind == "proc-macro" || kind.ends_with("lib"))
                })
                .any(|target| {
                    crate::visibility::name_matches(
                        pattern,
                        &crate::graph::crate_name(&target.name),
                    ) || crate::visibility::name_matches(pattern, &member.name)
                })
        });
        if !matched {
            return Err(CliError::new(
                ErrorCode::InvalidConfig,
                format!("the public crate `{pattern}` matches no library of the workspace"),
            )
            .with_help(
                "Write the crate name or the package name of a workspace library in \
                 `--public-crate` or `public-crates`.",
            ));
        }
    }
    Ok(())
}

/// The options of the run: the flags, with the table for each option that no
/// flag sets.
pub fn resolve(
    workspace: &Workspace,
    flags: &VisibilityOptions,
) -> Result<VisibilityOptions, CliError> {
    let config: Config = crate::structure::config::section(workspace, KEY)?;
    let mut options = flags.clone();
    if options.production.is_empty() {
        options.production = config
            .production
            .iter()
            .map(|entry| {
                crate::cli::parse_production(entry).map_err(|err| {
                    CliError::new(
                        ErrorCode::InvalidConfig,
                        format!("`[workspace.metadata.fallow.{KEY}]` is not valid: {err}"),
                    )
                    .with_help("Write each `production` entry as `PACKAGE:BIN`.")
                })
            })
            .collect::<Result<_, _>>()?;
    }
    if options.exclude.is_empty() {
        options.exclude = config.exclude_crates;
    }
    if options.public.is_none() {
        options.public = config.public_crates;
    }
    if let Some(patterns) = &options.public {
        check_public(workspace, patterns)?;
    }
    options.uniform_fields |= config.uniform_fields;
    if options.kinds.is_empty() {
        options.kinds = config.kinds.into_iter().map(FindingKind::from).collect();
    }
    Ok(options)
}
