//! The configuration of the structure checks.
//!
//! The configuration is in the root `Cargo.toml`, in one table for each check:
//!
//! ```toml
//! [workspace.metadata.fallow.boundaries]
//! [workspace.metadata.fallow.size]
//! [workspace.metadata.fallow.complexity]
//! [workspace.metadata.fallow.dupes]
//! ```
//!
//! Cargo already parses the manifest, so `cargo metadata` gives the tables as
//! JSON and the tool needs no TOML parser. Each check reads only its own table,
//! so another check can add a table under `fallow` without a change here. An
//! unknown key in a table of this module is an error, because a misspelled
//! limit must not pass without notice.

use std::collections::BTreeMap;

use clap::ValueEnum;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::error::{CliError, ErrorCode};
use crate::workspace::Workspace;

/// The key of the tool under `[workspace.metadata]`.
const ROOT_KEY: &str = "fallow";

/// Reads the table `[workspace.metadata.fallow.<key>]`, or the default when the
/// table is not there.
pub fn section<T: DeserializeOwned + Default>(
    workspace: &Workspace,
    key: &str,
) -> Result<T, CliError> {
    let Some(value) = workspace
        .metadata
        .get(ROOT_KEY)
        .and_then(|root| root.get(key))
    else {
        return Ok(T::default());
    };
    serde_json::from_value(value.clone()).map_err(|err| {
        CliError::new(
            ErrorCode::InvalidConfig,
            format!("`[workspace.metadata.{ROOT_KEY}.{key}]` is not valid: {err}"),
        )
        .with_help("Correct the table in the root Cargo.toml.")
    })
}

/// True when `path` is one of `prefixes` or is in a folder of `prefixes`.
pub fn matches_prefix(path: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|prefix| {
        let prefix = prefix.trim_end_matches('/');
        prefix.is_empty()
            || path == prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// `[workspace.metadata.fallow.boundaries]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct Boundaries {
    /// Named sets of package names. A rule names a set as `@name`.
    pub groups: BTreeMap<String, Vec<String>>,
    /// The rules on the dependencies between workspace packages.
    pub crates: Vec<CrateRule>,
    /// The rules on code patterns in source folders.
    pub sources: Vec<SourceRule>,
    /// The rules on paths between the modules of one package.
    pub modules: Vec<ModuleRule>,
    /// The rules on the items that code may use, with resolved paths.
    pub items: Vec<ItemRule>,
    /// The rules on `pub use <path>::*` re-exports.
    pub reexports: Vec<ReexportRule>,
}

/// A rule on the workspace items that the code of some packages must not use.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ItemRule {
    /// The rule name in the findings.
    pub rule: String,
    /// The packages that the rule checks: names, `@group` or `*`.
    pub from: Vec<String>,
    /// The items that they must not use, as `crate_name::module::item`. A
    /// module or a type includes its items.
    pub deny: Vec<String>,
    /// The modules or items that a use can go through to stay allowed, for
    /// example a session module that re-exports a denied function.
    #[serde(default)]
    pub allow_via: Vec<String>,
    /// The message, with `{from}` and `{to}` as placeholders.
    #[serde(default)]
    pub message: Option<String>,
}

/// A rule on the glob re-exports of some packages.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ReexportRule {
    /// The rule name in the findings.
    pub rule: String,
    /// The packages that the rule checks: names, `@group` or `*`.
    pub from: Vec<String>,
    /// The crates or modules that a `pub use <path>::*` must not read, as
    /// `crate_name` or `crate_name::module`. A module includes its child
    /// modules.
    pub deny: Vec<String>,
    /// The message, with `{from}` and `{to}` as placeholders.
    #[serde(default)]
    pub message: Option<String>,
}

/// The section of `Cargo.toml` that a crate rule checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DependencyKind {
    /// `[dependencies]`.
    Normal,
    /// `[dev-dependencies]`.
    Dev,
    /// `[build-dependencies]`.
    Build,
}

/// A rule on the dependencies between workspace packages.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CrateRule {
    /// The rule name in the findings.
    pub rule: String,
    /// The packages that the rule checks: names, `@group` or `*`.
    pub from: Vec<String>,
    /// The workspace packages that they must not depend on: names, `@group` or
    /// `*` for each workspace package.
    pub deny: Vec<String>,
    /// The packages that `deny` names but that stay allowed.
    #[serde(default)]
    pub allow: Vec<String>,
    /// The sections that the rule checks. The default is all three.
    #[serde(default)]
    pub kinds: Option<Vec<DependencyKind>>,
    /// The message, with `{from}` and `{to}` as placeholders.
    #[serde(default)]
    pub message: Option<String>,
}

/// A rule that keeps code patterns out of source folders.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SourceRule {
    /// The rule name in the findings.
    pub rule: String,
    /// The folders and files that the rule checks, relative to the workspace root.
    pub paths: Vec<String>,
    /// The folders and files that the rule does not check.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// The patterns. A pattern is Rust code. It matches the same token sequence;
    /// whitespace and comments do not count.
    pub deny: Vec<String>,
    /// How a pattern matches. The default is `tokens`.
    #[serde(default, rename = "match")]
    pub matching: Matching,
    /// The package that owns the code, for the message.
    #[serde(default)]
    pub owner: Option<String>,
    /// The message, with `{path}`, `{pattern}` and `{owner}` as placeholders.
    #[serde(default)]
    pub message: Option<String>,
}

/// How a source pattern matches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Matching {
    /// The same token sequence. Whitespace and comments do not count, and a
    /// pattern in a string or a comment does not match.
    #[default]
    Tokens,
    /// The same text, anywhere in the file, also in comments and strings.
    Text,
}

/// A rule on the paths between the modules of one package.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ModuleRule {
    /// The rule name in the findings.
    pub rule: String,
    /// The package. The rule checks each target of the package.
    pub package: String,
    /// The modules that the rule checks, as `crate::a::b`. A module includes
    /// its child modules.
    pub from: Vec<String>,
    /// The modules that they must not use, as `crate::a::b`.
    pub deny: Vec<String>,
    /// The modules that `deny` names but that stay allowed.
    #[serde(default)]
    pub allow: Vec<String>,
    /// The message, with `{from}` and `{to}` as placeholders.
    #[serde(default)]
    pub message: Option<String>,
}

/// `[workspace.metadata.fallow.size]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct Size {
    /// A file over this line count must not grow.
    pub max_file_lines: u32,
    /// A function over this line count is a hotspot.
    pub max_function_lines: u32,
    /// The folders and files that the check skips.
    pub exclude: Vec<String>,
}

impl Default for Size {
    fn default() -> Self {
        Self {
            max_file_lines: 2000,
            max_function_lines: 60,
            exclude: Vec::new(),
        }
    }
}

/// `[workspace.metadata.fallow.complexity]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct Complexity {
    /// A function over this cyclomatic complexity is a finding.
    pub max_cyclomatic: u32,
    /// A function over this cognitive complexity is a finding.
    pub max_cognitive: u32,
    /// With `--coverage`, a function with a CRAP score at or over this value
    /// is a finding. The value 0 turns the CRAP check off.
    pub max_crap: f64,
    /// How the `max-cyclomatic` limit counts the cyclomatic complexity.
    pub cyclomatic_policy: CyclomaticPolicy,
    /// The folders and files that the check skips.
    pub exclude: Vec<String>,
}

impl Default for Complexity {
    fn default() -> Self {
        Self {
            max_cyclomatic: 20,
            max_cognitive: 15,
            max_crap: 30.0,
            cyclomatic_policy: CyclomaticPolicy::Standard,
            exclude: Vec::new(),
        }
    }
}

impl Complexity {
    /// Stops a run with a CRAP limit that is not a finite value of 0 or more.
    pub fn validate(&self) -> Result<(), CliError> {
        if self.max_crap.is_finite() && self.max_crap >= 0.0 {
            return Ok(());
        }
        Err(CliError::new(
            ErrorCode::InvalidConfig,
            format!(
                "`[workspace.metadata.{ROOT_KEY}.complexity]` is not valid: `max-crap` is {}, but it must be 0 or more",
                self.max_crap
            ),
        )
        .with_help("Set `max-crap` to a number, for example 30. The value 0 turns the CRAP check off."))
    }
}

/// How the `max-cyclomatic` limit counts the cyclomatic complexity. The health
/// score always uses the standard count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CyclomaticPolicy {
    /// Each decision adds 1: each `match` arm whose pattern is not `_` too.
    #[default]
    Standard,
    /// A flat `match` adds at most 1. A `match` is flat when no arm has a
    /// guard and no arm has a decision, also no nested `match`.
    FlatMatch,
}

/// The token normalization of the clone search.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Each token must be the same.
    Strict,
    /// The same as `strict` for Rust.
    #[default]
    Mild,
    /// String and character literals match each other.
    Weak,
    /// Names, strings and numbers match each other.
    Semantic,
}

impl Mode {
    /// The wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Mild => "mild",
            Self::Weak => "weak",
            Self::Semantic => "semantic",
        }
    }
}

/// `[workspace.metadata.fallow.dupes]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct Duplication {
    /// The token normalization.
    pub mode: Mode,
    /// The smallest clone, in tokens.
    pub min_tokens: u32,
    /// The smallest clone, in lines.
    pub min_lines: u32,
    /// The smallest number of instances of a clone group.
    pub min_occurrences: u32,
    /// The largest duplicated line percentage. Zero means no limit.
    pub threshold: f64,
    /// Skip a clone group when all instances are in one folder.
    pub skip_local: bool,
    /// Leave `use` items and `extern crate` items out of the token streams.
    pub ignore_imports: bool,
    /// Leave out test code: test targets, test modules
    /// and items with a test attribute.
    pub production: bool,
    /// The folders and files that the check skips.
    pub exclude: Vec<String>,
}

impl Default for Duplication {
    fn default() -> Self {
        Self {
            mode: Mode::Mild,
            min_tokens: 50,
            min_lines: 5,
            min_occurrences: 2,
            threshold: 0.0,
            skip_local: false,
            ignore_imports: true,
            production: false,
            exclude: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn workspace(metadata: serde_json::Value) -> Workspace {
        Workspace {
            root: "/w".into(),
            target_dir: "/w/target".into(),
            members: Vec::new(),
            metadata,
        }
    }

    #[test]
    fn a_missing_table_gives_the_defaults() {
        let size: Size = section(&workspace(serde_json::Value::Null), "size").unwrap_or_default();
        assert_eq!(size.max_file_lines, 2000);
        let other = workspace(json!({ "fallow": { "unused-dependencies": { "x": 1 } } }));
        let size: Size = section(&other, "size").unwrap_or_default();
        assert_eq!(size.max_function_lines, 60);
    }

    #[test]
    fn a_table_overrides_the_defaults_in_kebab_case() {
        let ws = workspace(json!({ "fallow": { "size": { "max-file-lines": 10 } } }));
        let size: Size = section(&ws, "size").unwrap_or_default();
        assert_eq!((size.max_file_lines, size.max_function_lines), (10, 60));
    }

    #[test]
    fn an_unknown_key_is_an_invalid_config_error() {
        let ws = workspace(json!({ "fallow": { "size": { "max-lines": 10 } } }));
        let Err(err) = section::<Size>(&ws, "size") else {
            panic!("an unknown key is an error");
        };
        assert_eq!(err.code, ErrorCode::InvalidConfig);
        assert!(err.message.contains("max-lines"), "{}", err.message);
    }

    #[test]
    fn a_prefix_matches_the_path_and_its_folder_only() {
        let prefixes = vec!["crates/cli/src/".to_owned(), "a.rs".to_owned()];
        assert!(matches_prefix("crates/cli/src/x.rs", &prefixes));
        assert!(matches_prefix("a.rs", &prefixes));
        assert!(!matches_prefix("crates/cli/srcx/x.rs", &prefixes));
        assert!(!matches_prefix("b/a.rs", &prefixes));
    }
}
