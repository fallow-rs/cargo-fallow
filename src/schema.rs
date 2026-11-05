//! The JSON Schema of each JSON document that the tool writes.
//!
//! `cargo fallow schema` prints the schema. `docs/output-schema.json` holds a
//! copy, and a test fails when the copy and the generated schema differ. So a
//! change of a report field needs a change of the published schema.

use schemars::generate::SchemaSettings;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde_json::{Map, Value, json};

use crate::gates::RustGateName;
use crate::structure::report::Report;

/// The `$id` of the schema.
const SCHEMA_ID: &str =
    "https://github.com/fallow-rs/cargo-fallow/blob/main/docs/output-schema.json";

/// The JSON documents, keyed by the name of their definition: the `kind` of a
/// report, or a name for a document with no `kind`.
const DOCUMENTS: [&str; 24] = [
    "check",
    "deps",
    "fix",
    "visibility",
    "visibility-trace",
    "deps-trace",
    "dupes-trace",
    "files",
    "boundaries",
    "size",
    "complexity",
    "dupes",
    "api",
    "mutants",
    "mutants-inspect",
    "baseline",
    "health-snapshot",
    "explain",
    "init",
    "hooks",
    "hooks-status",
    "agent",
    "agent-status",
    "error",
];

/// The schema of each JSON document.
pub fn document() -> Value {
    let mut generator = SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator();
    let documents: Vec<(&str, Schema)> = DOCUMENTS
        .iter()
        .map(|&name| (name, subschema(&mut generator, name)))
        .collect();
    let mut defs: Map<String, Value> = generator.take_definitions(true).into_iter().collect();
    let mut any_of = Vec::new();
    for (name, schema) in documents {
        let key = format!("document-{name}");
        defs.insert(key.clone(), with_constants(name, schema.to_value()));
        any_of.push(json!({ "$ref": format!("#/$defs/{key}") }));
    }
    for (name, value) in &mut defs {
        if let Some(description) = shared_description(name) {
            strip_descriptions(value);
            value["description"] = json!(description);
        }
        close_gate_names(value);
    }
    defs.sort_keys();
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": SCHEMA_ID,
        "title": "cargo-fallow JSON output",
        "description": "Each JSON document that cargo-fallow writes: the report of each command (also `fix`), the inspect report of `mutants --inspect`, the trace of `visibility --trace`, `deps --trace` and `dupes --trace`, the baseline file of `--save-baseline`, the snapshot of `check --save-snapshot`, the rule of `explain`, the report of `init`, the reports of `hooks install`, `hooks uninstall`, `hooks status`, `agent install`, `agent uninstall` and `agent status`, and the error document of exit code 2. The `document-<name>` definitions name each document.",
        "anyOf": any_of,
        "$defs": defs,
    })
}

/// The documents with no `kind` and no `language` member, or with a `kind`
/// that the type gives and no `language`.
const NO_KIND: [&str; 8] = [
    "baseline",
    "explain",
    "error",
    "init",
    "hooks",
    "hooks-status",
    "agent",
    "agent-status",
];

/// The document schema with the fixed `kind` and `language` of the document.
/// The structure reports share one type, so the type cannot give the value.
fn with_constants(name: &str, schema: Value) -> Value {
    if NO_KIND.contains(&name) {
        return schema;
    }
    let mut constants = json!({
        "properties": {
            "kind": { "const": name },
            "language": { "const": "rust" },
            "_meta": {
                "description": "With `--explain`: the meaning, an example and the fix of each rule with a finding, keyed by rule, and the documentation address.",
                "type": "object",
                "properties": {
                    "rules": { "type": "object" },
                    "docs": { "type": "string" },
                },
            },
        },
    });
    if crate::suppress::check_name(name).is_some() {
        constants["properties"]["suppressed"] = json!({
            "description": "The number of findings that a suppression comment hid.",
            "type": "integer",
            "minimum": 0,
        });
        constants["properties"]["stale_suppressions"] = json!({
            "description": "The suppression comments of the check that hid no finding.",
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "line": { "type": "integer", "minimum": 1 },
                    "kinds": { "type": "array", "items": { "type": "string" } },
                },
                "required": ["path", "line", "kinds"],
            },
        });
    }
    json!({ "allOf": [schema, constants] })
}

/// The description of a type of `fallow-report-protocol`. The text of that
/// crate names the parts of the JavaScript CLI, so this schema uses its own.
fn shared_description(name: &str) -> Option<&'static str> {
    match name {
        "ErrorOutput" => Some("The error document of exit code 2, on stdout with `--format json`."),
        "GateOutcome" => Some("The outcome of one gate."),
        "GateStatus" => Some("`pass`, `warn`, `fail` or `skipped`."),
        "GateFile" => Some("A file that a gate judged."),
        "GateFileReason" => Some("The reason that a gate judged a file."),
        _ => None,
    }
}

/// Removes each `description` in a schema.
fn strip_descriptions(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("description");
            map.values_mut().for_each(strip_descriptions);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_descriptions),
        _ => {}
    }
}

/// Limits the keys of each `gate_outcomes` map to the gate names.
fn close_gate_names(value: &mut Value) {
    let Value::Object(map) = value else {
        if let Value::Array(items) = value {
            items.iter_mut().for_each(close_gate_names);
        }
        return;
    };
    if let Some(Value::Object(properties)) = map.get_mut("properties")
        && let Some(Value::Object(gates)) = properties.get_mut("gate_outcomes")
    {
        let names: Vec<&str> = RustGateName::ALL.iter().map(|name| name.as_str()).collect();
        gates.insert("propertyNames".to_owned(), json!({ "enum": names }));
    }
    map.values_mut().for_each(close_gate_names);
}

/// The schema of one document, as a reference into the definitions.
fn subschema(generator: &mut SchemaGenerator, name: &str) -> Schema {
    use crate::structure::{boundaries, complexity, dupes, size};
    match name {
        "check" => generator.subschema_for::<crate::check::report::CheckReport>(),
        "deps" => generator.subschema_for::<crate::deps::report::DepsReport>(),
        "fix" => generator.subschema_for::<crate::fix::FixReport>(),
        "visibility" => generator.subschema_for::<crate::visibility::report::VisibilityReport>(),
        "visibility-trace" => {
            generator.subschema_for::<crate::visibility::trace::VisibilityTrace>()
        }
        "deps-trace" => generator.subschema_for::<crate::deps::trace::DepsTrace>(),
        "dupes-trace" => generator.subschema_for::<dupes::trace::DupesTrace>(),
        "files" => structure::<crate::files::Summary, crate::files::Finding>(generator),
        "boundaries" => structure::<boundaries::Summary, boundaries::Finding>(generator),
        "size" => structure::<size::Summary, size::Finding>(generator),
        "complexity" => structure::<complexity::Summary, complexity::Finding>(generator),
        "dupes" => structure::<dupes::Summary, dupes::CloneGroup>(generator),
        "api" => structure::<crate::api::Summary, crate::api::Finding>(generator),
        "mutants" => generator.subschema_for::<crate::mutation::report::Report>(),
        "mutants-inspect" => generator.subschema_for::<crate::mutation::report::InspectReport>(),
        "baseline" => generator.subschema_for::<crate::baseline::File>(),
        "health-snapshot" => generator.subschema_for::<crate::hotspots::snapshot::Snapshot>(),
        _ => setup_subschema(generator, name),
    }
}

/// The schema of a document of a setup command (`explain`, `init`, `hooks`),
/// or of the error document.
fn setup_subschema(generator: &mut SchemaGenerator, name: &str) -> Schema {
    match name {
        "explain" => generator.subschema_for::<crate::explain::Explanation>(),
        "init" => generator.subschema_for::<crate::init::InitReport>(),
        "hooks" => generator.subschema_for::<crate::hooks::install::HooksReport>(),
        "hooks-status" => generator.subschema_for::<crate::hooks::install::HooksStatus>(),
        "agent" => generator.subschema_for::<crate::agent::AgentReport>(),
        "agent-status" => generator.subschema_for::<crate::agent::AgentStatus>(),
        _ => generator.subschema_for::<fallow_report_protocol::ErrorOutput>(),
    }
}

fn structure<S: JsonSchema, F: JsonSchema>(generator: &mut SchemaGenerator) -> Schema {
    generator.subschema_for::<Report<S, F>>()
}

/// The schema as pretty JSON.
pub fn text() -> String {
    serde_json::to_string_pretty(&document()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::check::CheckName;

    #[test]
    fn the_sections_schema_names_each_check_with_a_report() {
        let schema = document();
        let named: BTreeSet<&str> = schema["$defs"]["CheckSections"]["properties"]
            .as_object()
            .map(|properties| properties.keys().map(String::as_str).collect())
            .unwrap_or_default();
        // The parse gate has no report of its own.
        let checks: BTreeSet<&str> = CheckName::ALL
            .iter()
            .filter(|name| **name != CheckName::Parse)
            .map(|name| name.as_str())
            .collect();
        assert_eq!(named, checks);
    }

    #[test]
    fn each_report_fixes_its_kind_and_each_gate_map_its_keys() {
        let schema = document();
        assert_eq!(
            schema["$defs"]["document-dupes"]["allOf"][1]["properties"]["kind"]["const"],
            "dupes"
        );
        let gates = &schema["$defs"]["CheckReport"]["properties"]["gate_outcomes"];
        assert!(
            gates["propertyNames"]["enum"]
                .as_array()
                .is_some_and(|names| names.iter().any(|name| name == "stale-baseline")),
            "{gates:#}"
        );
        assert!(
            !schema["$defs"]["GateOutcome"]
                .to_string()
                .contains("cli_report"),
            "the shared types use this tool's descriptions"
        );
    }
}
