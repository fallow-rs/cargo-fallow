//! The GitHub formats: workflow commands for annotations, and a job summary.
//!
//! Both formats read the SARIF log of a run, the one location contract that
//! each command shares. They give the same text as the jq filters of the
//! GitHub Action, so a script outside the action has the same output. Each
//! path starts at the repository root, so GitHub can find the file.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::OnceLock;

use fallow_report_protocol::GateOutcome;
use serde_json::Value;

use super::markdown::{code, escape};
use crate::gates::RustGateName;

/// The most annotations of one run. GitHub shows at most 10 of each level
/// for a step, so the errors come first.
pub const MAX_ANNOTATIONS: usize = 50;

/// The most findings in the table of the job summary.
pub const MAX_SUMMARY_FINDINGS: usize = 50;

/// The parts of a job summary.
pub struct Summary<'a> {
    /// The `kind` of the report, for example `check`.
    pub kind: &'a str,
    /// The number of findings.
    pub total_issues: usize,
    /// The exit code of the run.
    pub exit_code: u8,
    /// The mutation score line of a `mutants` run.
    pub mutants_line: Option<String>,
    /// The gate outcomes of the run.
    pub gates: &'a BTreeMap<RustGateName, GateOutcome>,
    /// The SARIF log of the run.
    pub sarif: &'a Value,
}

/// The path from the repository root to the workspace root, with a `/` at
/// the end, or an empty string outside a git work tree.
pub fn path_prefix() -> &'static str {
    static PREFIX: OnceLock<String> = OnceLock::new();
    PREFIX.get_or_init(|| {
        let Some(root) = crate::workspace::loaded_root() else {
            return String::new();
        };
        match crate::diff::git(root, &["rev-parse", "--show-prefix"]) {
            Ok(output) => String::from_utf8_lossy(&output).trim().to_owned(),
            // Outside a work tree each path stays relative to the workspace.
            Err(detail) if detail.contains("not a git repository") => String::new(),
            Err(detail) => {
                super::warn(&format!(
                    "git cannot give the path from the repository root, so the paths stay relative to the workspace root: {}",
                    detail.trim()
                ));
                String::new()
            }
        }
    })
}

/// The SARIF results of each run of a log.
fn results(sarif: &Value) -> Vec<&Value> {
    sarif["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|run| run["results"].as_array().into_iter().flatten())
        .collect()
}

/// The workflow commands of a SARIF log: at most [`MAX_ANNOTATIONS`], the
/// errors first, then a notice when the log has more results.
pub fn annotations(sarif: &Value, prefix: &str) -> String {
    let mut all = results(sarif);
    // A stable sort keeps the report order within each level.
    all.sort_by_key(|result| rank(result["level"].as_str()));
    let mut lines: Vec<String> = all
        .iter()
        .take(MAX_ANNOTATIONS)
        .map(|result| annotation(result, prefix))
        .collect();
    if all.len() > MAX_ANNOTATIONS {
        lines.push(format!(
            "::notice::cargo-fallow wrote {MAX_ANNOTATIONS} of {} findings as annotations, errors first. The job summary and the report files hold all of them.",
            all.len()
        ));
    }
    lines.join("\n")
}

/// The order of a SARIF level: errors first. A result with no level is a
/// warning, the SARIF default.
fn rank(level: Option<&str>) -> u8 {
    match level.unwrap_or("warning") {
        "error" => 0,
        "warning" => 1,
        _ => 2,
    }
}

fn annotation(result: &Value, prefix: &str) -> String {
    let place = &result["locations"][0]["physicalLocation"];
    let region = &place["region"];
    let mut properties = Vec::new();
    if let Some(uri) = place["artifactLocation"]["uri"].as_str() {
        properties.push(format!(
            "file={}",
            property_escape(&format!("{prefix}{uri}"))
        ));
    }
    for (name, key) in [
        ("line", "startLine"),
        ("col", "startColumn"),
        ("endLine", "endLine"),
    ] {
        if let Some(value) = region[key].as_u64() {
            properties.push(format!("{name}={value}"));
        }
    }
    let rule = result["ruleId"].as_str().unwrap_or("finding");
    properties.push(format!(
        "title={}",
        property_escape(&format!("cargo-fallow {rule}"))
    ));
    let command = match result["level"].as_str().unwrap_or("warning") {
        "error" => "error",
        "warning" => "warning",
        _ => "notice",
    };
    format!(
        "::{command} {}::{}",
        properties.join(","),
        data_escape(result["message"]["text"].as_str().unwrap_or_default())
    )
}

/// The escape of the message of a workflow command.
pub fn data_escape(text: &str) -> String {
    text.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// The escape of a property value of a workflow command.
fn property_escape(text: &str) -> String {
    data_escape(text).replace(':', "%3A").replace(',', "%2C")
}

/// The job summary in markdown: the issue count and the exit code, the gate
/// table and the findings table.
pub fn summary(summary: &Summary<'_>, prefix: &str) -> String {
    let issues = summary.total_issues;
    let mut text = format!(
        "### cargo fallow {}\n\n{issues} {}, exit code {}.\n",
        summary.kind,
        if issues == 1 { "issue" } else { "issues" },
        summary.exit_code
    );
    if let Some(line) = &summary.mutants_line {
        let _ = write!(text, "\n{line}\n");
    }
    text.push_str("\n| Gate | Status | Enforced | Observed |\n| --- | --- | --- | --- |\n");
    for (name, outcome) in summary.gates {
        let _ = writeln!(
            text,
            "| `{}` | {} | {} | {} |",
            name.as_str(),
            serde_json::to_value(outcome.status)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
            if outcome.enforced { "yes" } else { "no" },
            outcome.observed.map(number).unwrap_or_default()
        );
    }
    let all = results(summary.sarif);
    if !all.is_empty() {
        let shown = all.len().min(MAX_SUMMARY_FINDINGS);
        let count = if all.len() > shown {
            format!(" ({shown} of {})", all.len())
        } else {
            String::new()
        };
        let _ = write!(
            text,
            "\n#### Findings{count}\n\n| Location | Rule | Message |\n| --- | --- | --- |\n"
        );
        for result in all.iter().take(shown) {
            let place = &result["locations"][0]["physicalLocation"];
            let mut location = format!(
                "{prefix}{}",
                place["artifactLocation"]["uri"]
                    .as_str()
                    .unwrap_or_default()
            );
            if let Some(line) = place["region"]["startLine"].as_u64() {
                let _ = write!(location, ":{line}");
            }
            let _ = writeln!(
                text,
                "| {} | {} | {} |",
                code(&location),
                code(result["ruleId"].as_str().unwrap_or_default()),
                escape(result["message"]["text"].as_str().unwrap_or_default())
            );
        }
    }
    text
}

/// A number as jq writes it: a whole number has no fraction.
fn number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the value is a whole number under 1e15"
        )]
        let whole = value as i64;
        whole.to_string()
    } else {
        value.to_string()
    }
}

/// The mutation score line of a `mutants` report.
pub fn mutants_line(json: &Value) -> Option<String> {
    if json["kind"] != "mutants" {
        return None;
    }
    let summary = &json["summary"];
    let score = summary["score"].as_f64()?;
    let percent = (score * 10.0).round() / 10.0;
    Some(format!(
        "Mutation score {}%: {} killed, {} survived, {} no coverage, {} timeout, {} pending.",
        number(percent),
        summary["killed"],
        summary["survived"],
        summary["no_coverage"],
        summary["timeout"],
        summary["pending"]
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn log(results: &Value) -> Value {
        json!({ "runs": [{ "results": results }] })
    }

    #[test]
    fn the_escapes_follow_the_workflow_command_rules() {
        assert_eq!(data_escape("100%\r\nnext"), "100%25%0D%0Anext");
        assert_eq!(property_escape("a:b,c%"), "a%3Ab%2Cc%25");
    }

    #[test]
    fn errors_come_first_and_the_list_has_a_cap() {
        let mut results: Vec<Value> = (0..MAX_ANNOTATIONS)
            .map(|index| json!({ "level": "warning", "ruleId": "w", "message": { "text": format!("w{index}") } }))
            .collect();
        results.push(json!({
            "level": "error",
            "ruleId": "e,1",
            "message": { "text": "bad\nthing" },
            "locations": [{ "physicalLocation": {
                "artifactLocation": { "uri": "src/a b.rs" },
                "region": { "startLine": 3, "startColumn": 2, "endLine": 4 }
            }}]
        }));
        let text = annotations(&log(&json!(results)), "crates/x/");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "::error file=crates/x/src a b.rs,line=3,col=2,endLine=4,title=cargo-fallow e%2C1::bad%0Athing"
                .replace("src a b", "src/a b")
        );
        assert_eq!(lines[1], "::warning title=cargo-fallow w::w0");
        assert_eq!(lines.len(), MAX_ANNOTATIONS + 1);
        assert!(
            lines[MAX_ANNOTATIONS].starts_with("::notice::cargo-fallow wrote 50 of 51 findings")
        );
        assert_eq!(annotations(&log(&json!([])), ""), "");
    }

    #[test]
    fn a_number_is_whole_or_has_its_fraction() {
        assert_eq!(number(4.0), "4");
        assert_eq!(number(20.69), "20.69");
    }
}
