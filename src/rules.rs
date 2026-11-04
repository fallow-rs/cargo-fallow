//! The severity of each rule: `[workspace.metadata.fallow.rules]`.
//!
//! A team sets a rule to `error`, `warn` or `off`. `warning` is an alias of
//! `warn`. An `off` rule gives no finding. An `error` rule counts for the gate
//! of its check, and a `warn` rule does not. A rule that the table does not
//! name keeps its own severity and gate.
//!
//! ```toml
//! [workspace.metadata.fallow.rules]
//! unused-optional-dependency = "error"
//! dead-public = "warn"
//! code-duplication = "off"
//! ```

use std::collections::BTreeMap;

use serde_json::Value;

use crate::error::{CliError, ErrorCode};
use crate::finding::{Finding, Severity};
use crate::workspace::Workspace;

/// The configured level of a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// The finding has the `error` severity and counts for the gate.
    Error,
    /// The finding has the `warn` severity and does not count for the gate.
    Warn,
    /// The rule gives no finding.
    Off,
}

/// The rule levels of the workspace, keyed by the rule name with `-` for
/// each `_`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    levels: BTreeMap<String, Level>,
}

/// The table name.
const TABLE: &str = "rules";

/// The rules that the table cannot change: the parse gate judges each target
/// root file, a surviving mutant has its own gate, and a stale suppression
/// comment is a note about the comments.
const FIXED: [&str; 3] = ["parse-error", "surviving-mutant", "stale-suppression"];

impl Rules {
    /// Reads the table. An unknown rule or an unknown level is an error.
    /// `boundary_rules` are the rule names of the boundaries configuration.
    pub fn load(workspace: &Workspace, boundary_rules: &[String]) -> Result<Self, CliError> {
        let Some(table) = workspace
            .metadata
            .get("fallow")
            .and_then(|root| root.get(TABLE))
        else {
            return Ok(Self::default());
        };
        let Value::Object(entries) = table else {
            return Err(invalid("must be a table of rule names"));
        };
        let mut levels = BTreeMap::new();
        for (rule, value) in entries {
            let key = normalize(rule);
            let known = crate::explain::RULES
                .iter()
                .any(|explanation| normalize(explanation.rule) == key)
                || boundary_rules.iter().any(|name| normalize(name) == key);
            if !known {
                return Err(invalid(&format!("names `{rule}`, which is not a rule"))
                    .with_help("Run `cargo fallow explain <rule>`, or see `cargo fallow explain --help`, for the rule names. A boundary rule uses the `rule` name of its table."));
            }
            if FIXED.iter().any(|fixed| normalize(fixed) == key) {
                return Err(
                    invalid(&format!("names `{rule}`, whose level cannot change")).with_help(
                        format!(
                            "Remove `{rule}` from the table. The rules {} have a fixed level.",
                            FIXED.join(", ")
                        ),
                    ),
                );
            }
            let level = match value.as_str() {
                Some("error") => Level::Error,
                Some("warn" | "warning") => Level::Warn,
                Some("off") => Level::Off,
                _ => {
                    return Err(invalid(&format!(
                        "sets `{rule}` to {value}, but a level is \"error\", \"warn\" or \"off\""
                    )));
                }
            };
            levels.insert(key, level);
        }
        Ok(Self { levels })
    }

    /// True when the table names no rule.
    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }

    /// The level of the first name that the table has: the rule, then the
    /// other names of the finding, for example the kind of a boundary rule.
    pub fn level(&self, names: &[&str]) -> Option<Level> {
        names
            .iter()
            .find_map(|name| self.levels.get(&normalize(name)).copied())
    }

    /// True when the table turns the rule off.
    pub fn is_off(&self, names: &[&str]) -> bool {
        self.level(names) == Some(Level::Off)
    }

    /// True when a finding counts for the gate of its check. `default` is
    /// the count of a rule that the table does not name.
    pub fn counts(&self, names: &[&str], default: bool) -> bool {
        match self.level(names) {
            Some(Level::Error) => true,
            Some(Level::Warn | Level::Off) => false,
            None => default,
        }
    }

    /// The severity of a finding with the configured level.
    pub fn severity(&self, names: &[&str], default: Severity) -> Severity {
        match self.level(names) {
            Some(Level::Error) => Severity::Error,
            Some(Level::Warn) => Severity::Warning,
            _ => default,
        }
    }

    /// Sets the severity of each finding of a configured rule.
    pub fn apply(&self, findings: &mut [Finding]) {
        if self.is_empty() {
            return;
        }
        for finding in findings {
            finding.severity = self.severity(&[&finding.rule], finding.severity);
        }
    }
}

/// The rule name with `-` for each `_`, so `dead_public` and `dead-public`
/// are one rule.
fn normalize(rule: &str) -> String {
    rule.replace('_', "-")
}

fn invalid(detail: &str) -> CliError {
    CliError::new(
        ErrorCode::InvalidConfig,
        format!("`[workspace.metadata.fallow.{TABLE}]` {detail}"),
    )
    .with_help("Correct the table in the root Cargo.toml.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(pairs: &[(&str, Level)]) -> Rules {
        Rules {
            levels: pairs
                .iter()
                .map(|(rule, level)| (normalize(rule), *level))
                .collect(),
        }
    }

    #[test]
    fn a_level_changes_the_count_and_the_severity() {
        let rules = rules(&[("dead_public", Level::Error), ("complexity", Level::Warn)]);
        assert!(rules.counts(&["dead-public"], false));
        assert!(!rules.counts(&["complexity"], true));
        assert!(rules.counts(&["code-duplication"], true));
        assert_eq!(
            rules.severity(&["dead_public"], Severity::Error),
            Severity::Error
        );
        assert_eq!(
            rules.severity(&["complexity"], Severity::Error),
            Severity::Warning
        );
        assert_eq!(
            rules.severity(&["unused-file"], Severity::Error),
            Severity::Error
        );
    }

    #[test]
    fn the_first_configured_name_wins() {
        let rules = rules(&[("crate-dependency", Level::Off)]);
        assert!(rules.is_off(&["process-stays-a-leaf", "crate-dependency"]));
        assert!(!rules.is_off(&["process-stays-a-leaf"]));
    }
}
