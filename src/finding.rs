//! The finding shape that each check shares, and the CodeClimate output.
//!
//! `fallow-report-protocol` has no finding type yet. The types in this module
//! follow the proposal for that crate: a stable id, the check and the rule, a
//! severity, a message, a location and the next steps. When the protocol crate
//! has the types, this module uses them and keeps the same wire shape.

use serde::Serialize;
use serde_json::{Value, json};

/// The prefix of a CodeClimate `check_name`, the same as in fallow.
const CHECK_NAME_PREFIX: &str = "fallow/";
/// The FNV-1a 64 offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// The FNV-1a 64 prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
/// The byte after each part of a fingerprint, so `["ab", "c"]` and
/// `["a", "bc"]` give different values. The same byte as in fallow.
const PART_SEPARATOR: u8 = 0xff;

/// The severity of a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// A finding that the rule counts as an error.
    Error,
    /// A finding that the rule counts as a warning. The JSON value is
    /// `warn`, the same as in the fallow JSON output. SARIF keeps `warning`.
    #[serde(rename = "warn")]
    Warning,
}

/// The CodeClimate category of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    /// Code or a dependency that can hide a defect.
    BugRisk,
    /// Code that is harder to read than necessary.
    Clarity,
    /// A large or complex unit.
    Complexity,
    /// Copied code.
    Duplication,
}

impl Category {
    /// The CodeClimate spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BugRisk => "Bug Risk",
            Self::Clarity => "Clarity",
            Self::Complexity => "Complexity",
            Self::Duplication => "Duplication",
        }
    }
}

/// The CodeClimate severity of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Impact {
    /// `minor`.
    Minor,
    /// `major`.
    Major,
    /// `critical`.
    Critical,
}

impl Impact {
    /// The CodeClimate spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Minor => "minor",
            Self::Major => "major",
            Self::Critical => "critical",
        }
    }
}

/// A place in a file. Lines and columns start at 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct Location {
    /// The path relative to the workspace root, with `/` separators.
    pub path: String,
    /// The first line, when the check knows it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// The first column, in characters, when the check knows it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// The last line, when the place is a range.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
}

impl Location {
    /// A place at a line and a column.
    pub fn at(path: impl Into<String>, line: u32, column: u32) -> Self {
        Self {
            path: path.into(),
            line: Some(line),
            column: Some(column),
            end_line: None,
        }
    }
}

/// A next step for a finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct Action {
    /// The kind of the step.
    #[serde(rename = "type")]
    pub kind: String,
    /// True when a tool can apply the step without a person.
    pub auto_fixable: bool,
    /// What the step does.
    pub description: String,
}

impl Action {
    /// A step with these fields.
    pub fn new(kind: &str, auto_fixable: bool, description: &str) -> Self {
        Self {
            kind: kind.to_owned(),
            auto_fixable,
            description: description.to_owned(),
        }
    }
}

/// One finding of one check. The field order is the wire order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct Finding {
    /// A stable id: 16 hex digits of a hash of the check, the rule and the
    /// identity of the finding. A run on the same code gives the same id.
    pub id: String,
    /// The check that found it, for example `deps`.
    pub check: &'static str,
    /// The rule, for example `unused-dependency`.
    pub rule: String,
    /// The severity of the rule.
    pub severity: Severity,
    /// What is wrong.
    pub message: String,
    /// The main place.
    pub location: Location,
    /// The other places of the same finding, for example the other copies of
    /// a clone.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub related_locations: Vec<Location>,
    /// The next steps.
    pub actions: Vec<Action>,
    /// The CodeClimate category.
    #[serde(skip)]
    pub category: Category,
    /// The CodeClimate severity.
    #[serde(skip)]
    pub impact: Impact,
}

/// The parts of a finding that the builder needs.
pub struct Draft<'a> {
    /// The check.
    pub check: &'static str,
    /// The rule.
    pub rule: &'a str,
    /// The severity.
    pub severity: Severity,
    /// The message.
    pub message: String,
    /// The main place.
    pub location: Location,
    /// The parts that make the finding unique in its check and rule. Use names
    /// before line numbers where a name is unique, so an edit above the
    /// finding does not change the id.
    pub identity: &'a [&'a str],
    /// The CodeClimate category.
    pub category: Category,
    /// The CodeClimate severity.
    pub impact: Impact,
}

/// The rule ids that had a snake_case form before, with that form.
const SNAKE_CASE_RULES: [(&str, &str); 3] = [
    ("dead-public", "dead_public"),
    ("unnecessary-public", "unnecessary_public"),
    (
        "unnecessary-restricted-visibility",
        "unnecessary_restricted_visibility",
    ),
];

/// The rule name in the hash of a finding id. A rule that had a snake_case
/// id keeps that form in the hash, so that the ids in a saved baseline still
/// match.
pub fn id_rule(rule: &str) -> &str {
    SNAKE_CASE_RULES
        .iter()
        .find(|(kebab, _)| *kebab == rule)
        .map_or(rule, |(_, snake)| snake)
}

impl Finding {
    /// Builds a finding with its id, no related places and no actions.
    pub fn new(draft: Draft<'_>) -> Self {
        let mut parts = vec![draft.check, id_rule(draft.rule)];
        parts.extend_from_slice(draft.identity);
        Self {
            id: fingerprint(&parts),
            check: draft.check,
            rule: draft.rule.to_owned(),
            severity: draft.severity,
            message: draft.message,
            location: draft.location,
            related_locations: Vec::new(),
            actions: Vec::new(),
            category: draft.category,
            impact: draft.impact,
        }
    }

    /// The finding with these next steps.
    #[must_use]
    pub fn with_actions(mut self, actions: Vec<Action>) -> Self {
        self.actions = actions;
        self
    }

    /// The finding with these other places.
    #[must_use]
    pub fn with_related(mut self, related: Vec<Location>) -> Self {
        self.related_locations = related;
        self
    }

    /// The CodeClimate `check_name`: `fallow/` and the rule in kebab case.
    pub fn check_name(&self) -> String {
        format!("{CHECK_NAME_PREFIX}{}", self.rule.replace('_', "-"))
    }
}

/// The place of each key among the entries with the same key, in line order.
/// An id that uses a name plus this index stays the same when a line above the
/// entry changes, and stays unique when two entries share a name.
pub fn occurrences<K: Ord + Clone>(entries: &[(K, u32)]) -> Vec<usize> {
    entries
        .iter()
        .map(|(key, line)| {
            entries
                .iter()
                .filter(|(other, other_line)| other == key && other_line < line)
                .count()
        })
        .collect()
}

/// FNV-1a 64 over the parts, with [`PART_SEPARATOR`] after each part, as 16
/// lowercase hex digits. The same hash as the CodeClimate fingerprints of
/// fallow.
pub fn fingerprint(parts: &[&str]) -> String {
    let mut hash = FNV_OFFSET;
    for part in parts {
        for byte in part.bytes().chain(std::iter::once(PART_SEPARATOR)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    format!("{hash:016x}")
}

/// The findings as a CodeClimate report: a JSON array with one issue for each
/// finding.
pub fn codeclimate(findings: &[Finding]) -> Value {
    findings.iter().map(issue).collect()
}

fn issue(finding: &Finding) -> Value {
    let mut value = json!({
        "type": "issue",
        "check_name": finding.check_name(),
        "description": finding.message,
        "categories": [finding.category.as_str()],
        "severity": finding.impact.as_str(),
        "fingerprint": finding.id,
        "location": location(&finding.location),
    });
    if !finding.related_locations.is_empty() {
        value["other_locations"] = finding.related_locations.iter().map(location).collect();
    }
    value
}

fn location(place: &Location) -> Value {
    let begin = place.line.unwrap_or(1).max(1);
    let mut lines = json!({ "begin": begin });
    if let Some(end) = place.end_line {
        lines["end"] = json!(end.max(begin));
    }
    json!({ "path": place.path, "lines": lines })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft<'a>(identity: &'a [&'a str], location: Location) -> Draft<'a> {
        Draft {
            check: "deps",
            rule: "unused-dependency",
            severity: Severity::Error,
            message: "m".to_owned(),
            location,
            identity,
            category: Category::BugRisk,
            impact: Impact::Major,
        }
    }

    #[test]
    fn a_kebab_case_visibility_rule_keeps_the_id_of_its_snake_case_form() {
        let location = Location::at("src/lib.rs".to_owned(), 3, 1);
        let mut draft = draft(&["my_crate", "helper", "function"], location);
        draft.check = "visibility";
        draft.rule = "dead-public";
        let finding = Finding::new(draft);
        assert_eq!(finding.rule, "dead-public");
        assert_eq!(
            finding.id,
            fingerprint(&[
                "visibility",
                "dead_public",
                "my_crate",
                "helper",
                "function"
            ])
        );
    }

    #[test]
    fn severity_serializes_as_error_and_warn() {
        assert_eq!(json!(Severity::Error), json!("error"));
        assert_eq!(json!(Severity::Warning), json!("warn"));
    }

    #[test]
    fn occurrences_count_the_earlier_lines_of_the_same_key() {
        let entries = [("a", 9), ("b", 1), ("a", 3), ("a", 5)];
        assert_eq!(occurrences(&entries), [2, 0, 0, 1]);
    }

    #[test]
    fn fingerprint_is_fnv1a_with_a_separator_after_each_part() {
        // FNV-1a 64 of the single byte 0xff.
        assert_eq!(fingerprint(&[""]), "af64724c8602eb6e");
        assert_eq!(fingerprint(&[]), "cbf29ce484222325");
        assert_ne!(fingerprint(&["ab", "c"]), fingerprint(&["a", "bc"]));
        assert_eq!(fingerprint(&["a", "b"]).len(), 16);
    }

    #[test]
    fn the_id_depends_on_the_identity_and_not_on_the_message() {
        let place = Location::at("Cargo.toml", 3, 1);
        let one = Finding::new(draft(&["Cargo.toml", "serde"], place.clone()));
        let mut moved = draft(&["Cargo.toml", "serde"], Location::at("Cargo.toml", 9, 1));
        moved.message = "other".to_owned();
        assert_eq!(one.id, Finding::new(moved).id);
        assert_ne!(
            one.id,
            Finding::new(draft(&["Cargo.toml", "toml"], place)).id
        );
    }

    #[test]
    fn codeclimate_issue_has_the_fallow_fields() {
        let finding =
            Finding::new(draft(&["x"], Location::at("Cargo.toml", 3, 1))).with_related(vec![
                Location {
                    path: "b.rs".to_owned(),
                    line: Some(2),
                    column: None,
                    end_line: Some(1),
                },
            ]);
        let report = codeclimate(std::slice::from_ref(&finding));
        assert_eq!(
            report,
            json!([{
                "type": "issue",
                "check_name": "fallow/unused-dependency",
                "description": "m",
                "categories": ["Bug Risk"],
                "severity": "major",
                "fingerprint": finding.id,
                "location": { "path": "Cargo.toml", "lines": { "begin": 3 } },
                "other_locations": [{ "path": "b.rs", "lines": { "begin": 2, "end": 2 } }],
            }])
        );
    }

    #[test]
    fn a_place_without_a_line_begins_at_line_one_and_names_are_kebab_case() {
        let mut finding = Finding::new(draft(
            &["x"],
            Location {
                path: "Cargo.toml".to_owned(),
                line: None,
                column: None,
                end_line: None,
            },
        ));
        finding.rule = "dead-public".to_owned();
        let report = codeclimate(&[finding]);
        assert_eq!(report[0]["location"]["lines"], json!({ "begin": 1 }));
        assert_eq!(report[0]["check_name"], "fallow/dead-public");
    }
}
