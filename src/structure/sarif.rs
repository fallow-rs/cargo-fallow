//! The SARIF 2.1.0 log of a structure check.
//!
//! Each finding becomes a result. Each failed gate becomes a tool execution
//! notification, the same as in the SARIF log of `check`. A failed gate is not
//! a tool failure, so `executionSuccessful` stays true.

use std::collections::BTreeMap;

use fallow_report_protocol::{GateOutcome, GateStatus};
use serde_json::{Value, json};

use crate::finding::{Finding, Location};
use crate::gates::RustGateName as Gate;
use crate::report::TOOL_VERSION;

const SARIF_SCHEMA: &str = "https://json.schemastore.org/sarif-2.1.0.json";
const SARIF_VERSION: &str = "2.1.0";
const TOOL_NAME: &str = "cargo-fallow";
const INFORMATION_URI: &str = "https://github.com/fallow-rs/cargo-fallow";

/// The SARIF log. `rules` maps each rule id to its short description. Each
/// finding is a result at the level of its severity.
pub fn log(
    rules: &BTreeMap<String, String>,
    findings: &[Finding],
    gates: &BTreeMap<Gate, GateOutcome>,
) -> Value {
    let used: BTreeMap<&str, &str> = findings
        .iter()
        .filter_map(|item| {
            rules
                .get_key_value(&item.rule)
                .map(|(id, text)| (id.as_str(), text.as_str()))
        })
        .collect();
    let rules: Vec<Value> = used
        .iter()
        .map(|(id, text)| json!({ "id": id, "shortDescription": { "text": text } }))
        .collect();
    let results: Vec<Value> = findings.iter().map(result).collect();
    let mut invocation = json!({ "executionSuccessful": true });
    let notifications: Vec<Value> = gates
        .iter()
        .filter(|(_, outcome)| outcome.status == GateStatus::Fail)
        .map(|(gate, outcome)| notification(*gate, outcome))
        .collect();
    if !notifications.is_empty() {
        invocation["toolExecutionNotifications"] = Value::Array(notifications);
    }
    json!({
        "$schema": SARIF_SCHEMA,
        "version": SARIF_VERSION,
        "runs": [{
            "tool": {
                "driver": {
                    "name": TOOL_NAME,
                    "version": TOOL_VERSION,
                    "informationUri": INFORMATION_URI,
                    "rules": rules,
                }
            },
            "invocations": [invocation],
            "results": results,
        }],
    })
}

fn result(finding: &Finding) -> Value {
    let mut value = json!({
        "ruleId": finding.rule,
        "level": finding.severity.sarif_level(),
        "message": { "text": finding.message },
        "locations": [location(&finding.location)],
    });
    if !finding.related_locations.is_empty() {
        value["relatedLocations"] = finding
            .related_locations
            .iter()
            .enumerate()
            .map(|(index, place)| {
                let mut related = location(place);
                related["id"] = json!(index);
                related
            })
            .collect();
    }
    value
}

fn location(place: &Location) -> Value {
    let line = place.line.unwrap_or(1).max(1);
    let mut region = json!({ "startLine": line, "startColumn": place.column.unwrap_or(1).max(1) });
    if let Some(end) = place.end_line {
        region["endLine"] = json!(end.max(line));
    }
    json!({
        "physicalLocation": {
            "artifactLocation": { "uri": place.path },
            "region": region,
        }
    })
}

fn notification(gate: Gate, outcome: &GateOutcome) -> Value {
    let (level, enforced) = if outcome.enforced {
        ("error", "enforced")
    } else {
        ("warning", "not enforced")
    };
    json!({
        "level": level,
        "message": { "text": format!("The `{}` gate failed ({enforced}).", gate.as_str()) },
        "descriptor": { "id": gate.as_str() },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::{Category, Draft, Impact, Severity};
    use crate::structure::report::count_gate;

    fn place(path: &str, line: u32) -> Location {
        Location::at(path, line, 1)
    }

    #[test]
    fn a_failed_gate_becomes_a_notification_and_rules_list_only_used_ids() {
        let rules = BTreeMap::from([
            ("a".to_owned(), "Rule a.".to_owned()),
            ("b".to_owned(), "Rule b.".to_owned()),
        ]);
        let items = [Finding::new(Draft {
            check: "size",
            rule: "a",
            severity: Severity::Warning,
            message: "m".to_owned(),
            location: place("x.rs", 3),
            identity: &[],
            category: Category::Complexity,
            impact: Impact::Minor,
        })
        .with_related(vec![place("y.rs", 4)])];
        let gates = BTreeMap::from([(Gate::FileGrowth, count_gate(1, true))]);
        let log = log(&rules, &items, &gates);
        let run = &log["runs"][0];
        assert_eq!(
            run["tool"]["driver"]["rules"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(
            run["results"][0]["locations"][0]["physicalLocation"]["region"]["startLine"],
            3
        );
        assert_eq!(run["results"][0]["relatedLocations"][0]["id"], 0);
        // A warning stays a warning, also when its gate is enforced.
        assert_eq!(run["results"][0]["level"], "warning");
        let notes = &run["invocations"][0]["toolExecutionNotifications"];
        assert_eq!(notes[0]["descriptor"]["id"], "file-growth");
        assert_eq!(notes[0]["level"], "error");
        assert_eq!(run["invocations"][0]["executionSuccessful"], true);
    }

    #[test]
    fn a_passed_gate_has_no_notification() {
        let gates = BTreeMap::from([(Gate::FileGrowth, count_gate(0, true))]);
        let log = log(&BTreeMap::new(), &[], &gates);
        assert!(
            log["runs"][0]["invocations"][0]
                .get("toolExecutionNotifications")
                .is_none()
        );
    }
}
