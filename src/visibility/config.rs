//! The `[workspace.metadata.fallow.visibility]` table.
//!
//! ```toml
//! [workspace.metadata.fallow.visibility]
//! production = ["app:app"]
//! exclude-crates = ["my_api"]
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
    /// True when the fields of one struct change visibility as a group.
    uniform_fields: bool,
    /// The finding kinds to report.
    kinds: Vec<KindName>,
}

/// A finding kind, with the spelling of the `--kind` flag.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum KindName {
    DeadPublic,
    UnnecessaryPublic,
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
    options.uniform_fields |= config.uniform_fields;
    if options.kinds.is_empty() {
        options.kinds = config.kinds.into_iter().map(FindingKind::from).collect();
    }
    Ok(options)
}
