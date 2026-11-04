//! `dupes --trace <fingerprint>`: the instances of one clone group.

use std::fmt::Write as _;

use serde::Serialize;
use serde_json::Value;

use super::CloneGroup;
use crate::error::{CliError, ErrorCode};

/// The `kind` of the trace document.
pub const KIND: &str = "dupes-trace";

/// The trace of one fingerprint. The field order is the wire order.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct DupesTrace {
    /// The version of the trace shape.
    pub schema_version: u32,
    /// The version of cargo-fallow.
    pub version: &'static str,
    /// Always `dupes-trace`.
    pub kind: &'static str,
    /// Always `rust`.
    pub language: &'static str,
    /// The value of `--trace`.
    pub fingerprint: String,
    /// The clone group with the fingerprint, as in the `findings` of a
    /// `dupes` report.
    #[schemars(with = "Vec<CloneGroup>")]
    pub clone_groups: Vec<Value>,
}

/// The trace of `fingerprint` in the JSON of a `dupes` report with no scope.
pub fn trace(report: &Value, fingerprint: &str) -> Result<DupesTrace, CliError> {
    let clone_groups: Vec<Value> = report["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|group| group["fingerprint"] == fingerprint)
        .cloned()
        .collect();
    if clone_groups.is_empty() {
        return Err(CliError::new(
            ErrorCode::InvalidArgument,
            format!("no clone group has the fingerprint `{fingerprint}`"),
        )
        .with_help(
            "Give the `fingerprint` of a clone group of `cargo fallow dupes --format json`, \
             with the same `--mode`, `--min-tokens` and `--min-lines`.",
        ));
    }
    Ok(DupesTrace {
        schema_version: crate::trace::SCHEMA_VERSION,
        version: crate::report::TOOL_VERSION,
        kind: KIND,
        language: "rust",
        fingerprint: fingerprint.to_owned(),
        clone_groups,
    })
}

impl DupesTrace {
    /// The trace as text for a person.
    pub fn human(&self) -> String {
        let mut text = String::new();
        let _ = writeln!(text, "cargo fallow dupes --trace {}", self.fingerprint);
        for group in &self.clone_groups {
            let _ = writeln!(
                text,
                "\n{} lines, {} tokens:",
                group["line_count"], group["token_count"]
            );
            for instance in group["instances"].as_array().into_iter().flatten() {
                let _ = writeln!(
                    text,
                    "  {}:{}-{}",
                    instance["file"].as_str().unwrap_or_default(),
                    instance["start_line"],
                    instance["end_line"]
                );
            }
        }
        text
    }
}
