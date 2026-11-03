//! The report of the `visibility` command.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use fallow_report_protocol::{GateFile, GateOutcome, GateStatus};
use serde::Serialize;

use crate::gates::RustGateName;
use crate::graph::Graph;
use crate::graph::extract::{ItemKind, Vis};
use crate::visibility::precise::PreciseSummary;
use crate::visibility::{Analysis, Finding, FindingKind};

/// The version of the report shape. A change that removes a member or changes
/// its type increments it.
pub const SCHEMA_VERSION: u32 = 1;
/// The `kind` of the report.
pub const KIND: &str = "visibility";
/// The type of the action of a finding on an item with a visibility keyword.
pub const CHANGE_ACTION: &str = "change-visibility";
/// The type of the action of a finding on an enum variant.
pub const REMOVE_ACTION: &str = "remove-item";

/// The report.
#[derive(Debug, Clone, Serialize)]
pub struct VisibilityReport {
    /// The version of the report shape.
    pub schema_version: u32,
    /// The version of cargo-fallow.
    pub version: &'static str,
    /// Always `visibility`.
    pub kind: &'static str,
    /// Always `rust`.
    pub language: &'static str,
    /// `fast` or `precise`.
    pub mode: &'static str,
    /// The number of findings.
    pub total_issues: usize,
    /// The count of each finding kind.
    pub summary: Summary,
    /// The gate outcomes, keyed by gate name.
    pub gate_outcomes: BTreeMap<RustGateName, GateOutcome>,
    /// The result of `--precise` mode. `None` in the fast mode.
    pub precise: Option<PreciseSummary>,
    /// The size of the graph.
    pub graph: GraphSummary,
    /// The crates of the graph: one for each target.
    pub crates: Vec<CrateRow>,
    /// The findings, sorted by file, line and kind.
    pub findings: Vec<FindingRow>,
    /// The candidates that only an untyped credit keeps: a method call, a field
    /// access, or a name in macro tokens or in an attribute. They are not
    /// findings. `--precise` mode decides them.
    pub undecided: Vec<FindingRow>,
}

/// The count of each finding kind.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    /// The `dead_public` findings.
    pub dead_public: usize,
    /// The `unnecessary_public` findings.
    pub unnecessary_public: usize,
    /// The `unnecessary_restricted_visibility` findings.
    pub unnecessary_restricted_visibility: usize,
    /// The undecided candidates.
    pub undecided: usize,
}

/// The size of the graph.
#[derive(Debug, Clone, Serialize)]
pub struct GraphSummary {
    /// The crates: one for each target.
    pub crates: usize,
    /// The modules.
    pub modules: usize,
    /// The items.
    pub items: usize,
    /// The parsed files, counted once for each crate.
    pub files: u32,
    /// The files that a declaration reached but that did not parse. Their
    /// items and uses are not in the graph.
    pub skipped_files: Vec<SkippedRow>,
    /// The `.rs` files under the root folders that no declaration reached.
    pub unreached_files: u32,
}

/// One crate of the graph.
#[derive(Debug, Clone, Serialize)]
pub struct CrateRow {
    /// The package.
    pub package: String,
    /// The crate name.
    pub name: String,
    /// The target kind.
    pub kind: &'static str,
    /// True for a production binary.
    pub production: bool,
    /// The features of the package.
    pub features: Vec<String>,
    /// The features that the target needs.
    pub required_features: Vec<String>,
    /// The crate names that the code can use.
    pub externs: Vec<ExternRow>,
}

/// A crate name that the code can use.
#[derive(Debug, Clone, Serialize)]
pub struct ExternRow {
    /// The name in code.
    pub name: String,
    /// The workspace crate, or `None` for a crate outside the workspace.
    pub workspace_crate: Option<String>,
    /// The `cfg` or target triple of a platform-specific dependency.
    pub platform: Option<String>,
    /// True for an optional dependency.
    pub optional: bool,
}

/// A file that did not parse.
#[derive(Debug, Clone, Serialize)]
pub struct SkippedRow {
    /// The path.
    pub path: String,
    /// The reason.
    pub reason: String,
}

/// One finding.
#[derive(Debug, Clone, Serialize)]
pub struct FindingRow {
    /// The finding kind.
    pub kind: &'static str,
    /// The package.
    pub package: String,
    /// The crate name.
    #[serde(rename = "crate")]
    pub krate: String,
    /// The item path from the crate root.
    pub item: String,
    /// The item kind.
    pub item_kind: &'static str,
    /// The file, relative to the workspace root.
    pub file: String,
    /// The one-based line of the name.
    pub line: u32,
    /// The one-based column of the name, in characters.
    pub column: u32,
    /// The one-based last line of the item.
    pub end_line: u32,
    /// The written visibility.
    pub visibility: String,
    /// The smaller visibility. An empty text means private.
    pub suggested: String,
    /// True when only test code reaches the item.
    pub test_only: bool,
    /// The next steps.
    pub actions: Vec<Action>,
}

/// The change of the visibility keyword.
#[derive(Debug, Clone, Serialize)]
pub struct Action {
    /// `change-visibility`, or `remove-item` for an enum variant.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// Always false: a change can need a matching change in other code, for
    /// example in a `pub use` or in a field group.
    pub auto_fixable: bool,
    /// What the step does.
    pub description: &'static str,
    /// The text to replace: the written keyword.
    pub from: String,
    /// The new text. An empty text removes the keyword.
    pub to: String,
    /// The one-based position of the keyword, or `None` when the item has no
    /// keyword of its own, for example an enum variant.
    pub range: Option<Range>,
}

/// A range of text with one-based lines and columns in characters. The end is
/// the position after the last character.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Range {
    /// The line of the first character.
    pub line: u32,
    /// The column of the first character.
    pub column: u32,
    /// The line of the end.
    pub end_line: u32,
    /// The column of the end.
    pub end_column: u32,
}

/// The inputs of one report.
pub struct Inputs<'a> {
    /// The graph.
    pub graph: &'a Graph,
    /// The findings and the undecided candidates.
    pub analysis: &'a Analysis,
    /// The result of `--precise` mode.
    pub precise: Option<PreciseSummary>,
    /// True when a finding fails the run.
    pub enforced: bool,
}

impl VisibilityReport {
    /// Builds the report.
    pub fn new(inputs: Inputs<'_>) -> Self {
        let Inputs {
            graph,
            analysis,
            precise,
            enforced,
        } = inputs;
        let rows = |findings: &[Finding]| -> Vec<FindingRow> {
            let mut rows: Vec<FindingRow> =
                findings.iter().map(|finding| row(graph, finding)).collect();
            rows.sort_by(|a, b| {
                (&a.file, a.line, a.column, a.kind, &a.item)
                    .cmp(&(&b.file, b.line, b.column, b.kind, &b.item))
            });
            rows
        };
        let findings = rows(&analysis.findings);
        let undecided = rows(&analysis.undecided);
        let mut summary = Summary {
            undecided: undecided.len(),
            ..Summary::default()
        };
        for finding in &analysis.findings {
            match finding.kind {
                FindingKind::DeadPublic => summary.dead_public += 1,
                FindingKind::UnnecessaryPublic => summary.unnecessary_public += 1,
                FindingKind::UnnecessaryRestricted => {
                    summary.unnecessary_restricted_visibility += 1;
                }
            }
        }
        let mut gate_outcomes = BTreeMap::new();
        gate_outcomes.insert(
            RustGateName::VisibilityFindings,
            findings_gate(&findings, enforced),
        );
        Self {
            schema_version: SCHEMA_VERSION,
            version: crate::report::TOOL_VERSION,
            kind: KIND,
            language: "rust",
            mode: if precise.is_some() { "precise" } else { "fast" },
            total_issues: findings.len(),
            summary,
            gate_outcomes,
            precise,
            graph: GraphSummary {
                crates: graph.crates.len(),
                modules: graph.modules.len(),
                items: graph.items.len(),
                files: graph.files,
                skipped_files: graph
                    .skipped
                    .iter()
                    .map(|file| SkippedRow {
                        path: file.path.clone(),
                        reason: file.reason.clone(),
                    })
                    .collect(),
                unreached_files: graph.unreached,
            },
            crates: graph
                .crates
                .iter()
                .map(|entry| crate_row(graph, entry))
                .collect(),
            findings,
            undecided,
        }
    }

    /// True when an enforced gate failed.
    pub fn fails_run(&self) -> bool {
        self.gate_outcomes.values().any(GateOutcome::fails_run)
    }

    /// The report as text for a person.
    pub fn human(&self) -> String {
        let mut text = String::new();
        let _ = writeln!(text, "cargo fallow visibility ({} mode)", self.mode);
        for finding in &self.findings {
            let to = if finding.suggested.is_empty() {
                "private"
            } else {
                finding.suggested.as_str()
            };
            let _ = writeln!(
                text,
                "  {}:{}:{} {} {} `{}`: {} -> {}{}",
                finding.file,
                finding.line,
                finding.column,
                finding.kind,
                finding.item_kind,
                finding.item,
                finding.visibility,
                to,
                if finding.test_only {
                    " (only tests use it)"
                } else {
                    ""
                }
            );
        }
        let s = &self.summary;
        let _ = writeln!(
            text,
            "{} findings: {} dead_public, {} unnecessary_public, {} \
             unnecessary_restricted_visibility",
            self.total_issues,
            s.dead_public,
            s.unnecessary_public,
            s.unnecessary_restricted_visibility
        );
        if s.undecided > 0 {
            let hint = if self.precise.is_none() {
                " Run with `--precise` to decide them."
            } else {
                ""
            };
            let _ = writeln!(
                text,
                "{} candidates stay undecided, so they are not findings.{hint}",
                s.undecided
            );
        }
        if let Some(precise) = &self.precise {
            let _ = writeln!(text, "{}", precise.human_line());
        }
        if !self.graph.skipped_files.is_empty() {
            let _ = writeln!(
                text,
                "{} files did not parse, so their uses are not in the graph.",
                self.graph.skipped_files.len()
            );
        }
        text.push_str("\nGates:\n");
        for (name, outcome) in &self.gate_outcomes {
            let _ = writeln!(
                text,
                "  {}: {}",
                name.as_str(),
                crate::render::gate_text(outcome)
            );
        }
        text
    }

    /// The report as a SARIF log. Each finding is a result. The level is
    /// `error` when the gate is enforced, else `warning`.
    pub fn sarif(&self) -> serde_json::Value {
        use serde_json::json;
        let outcome = &self.gate_outcomes[&RustGateName::VisibilityFindings];
        let level = if outcome.enforced { "error" } else { "warning" };
        let results: Vec<serde_json::Value> = self
            .findings
            .iter()
            .map(|finding| {
                json!({
                    "ruleId": finding.kind,
                    "level": level,
                    "message": { "text": message(finding) },
                    "locations": [{
                        "physicalLocation": {
                            "artifactLocation": { "uri": finding.file },
                            "region": {
                                "startLine": finding.line,
                                "startColumn": finding.column,
                            },
                        }
                    }],
                })
            })
            .collect();
        let mut kinds: Vec<&'static str> = self.findings.iter().map(|row| row.kind).collect();
        kinds.sort_unstable();
        kinds.dedup();
        let rules: Vec<serde_json::Value> = kinds
            .into_iter()
            .map(|kind| json!({ "id": kind, "shortDescription": { "text": rule_text(kind) } }))
            .collect();
        crate::render::sarif_document(&rules, &self.gate_outcomes, &results)
    }
}

fn crate_row(graph: &Graph, entry: &crate::graph::Crate) -> CrateRow {
    CrateRow {
        package: entry.package.clone(),
        name: entry.name.clone(),
        kind: entry.kind.label(),
        production: entry.production,
        features: entry.features.clone(),
        required_features: entry.required_features.clone(),
        externs: entry
            .externs
            .iter()
            .map(|name| ExternRow {
                name: name.name.clone(),
                workspace_crate: name.target.map(|id| graph.crates[id].name.clone()),
                platform: name.platform.clone(),
                optional: name.optional,
            })
            .collect(),
    }
}

fn message(finding: &FindingRow) -> String {
    let to = if finding.suggested.is_empty() {
        "private".to_owned()
    } else {
        format!("`{}`", finding.suggested)
    };
    match finding.kind {
        "dead_public" => format!(
            "No code reaches the {} `{}`. Make it {to}, and remove it when the compiler \
             reports it as dead code.",
            finding.item_kind, finding.item
        ),
        "unnecessary_public" => format!(
            "Only its own crate uses the {} `{}`. Make it {to}.",
            finding.item_kind, finding.item
        ),
        _ => format!(
            "Only its own module uses the {} `{}`. Make it {to}.",
            finding.item_kind, finding.item
        ),
    }
}

fn rule_text(kind: &str) -> &'static str {
    match kind {
        "dead_public" => "A `pub` item that no code reaches.",
        "unnecessary_public" => "A `pub` item that only its own crate uses.",
        _ => "A restricted item that only its own module uses.",
    }
}

/// The outcome of the `visibility-findings` gate.
fn findings_gate(findings: &[FindingRow], enforced: bool) -> GateOutcome {
    let mut per_file: BTreeMap<&str, u32> = BTreeMap::new();
    for finding in findings {
        *per_file.entry(&finding.file).or_default() += 1;
    }
    let status = if findings.is_empty() {
        GateStatus::Pass
    } else {
        GateStatus::Fail
    };
    let count = u32::try_from(findings.len()).unwrap_or(u32::MAX);
    let mut outcome = GateOutcome::measured(status, enforced, f64::from(count), 0.0);
    outcome.files = per_file
        .into_iter()
        .map(|(path, error_count)| GateFile {
            path: path.to_owned(),
            error_count,
            panicked: false,
            reason: None,
        })
        .collect();
    outcome
}

const fn description(kind: FindingKind) -> &'static str {
    match kind {
        FindingKind::DeadPublic => {
            "Make the item `pub(crate)`. The compiler then reports it as dead code when no \
             code of the crate uses it."
        }
        FindingKind::UnnecessaryPublic => "Make the item `pub(crate)`.",
        FindingKind::UnnecessaryRestricted => "Remove the visibility keyword.",
    }
}

fn row(graph: &Graph, finding: &Finding) -> FindingRow {
    let item = &graph.items[finding.item];
    let krate = &graph.crates[item.krate];
    let written = written(&item.vis, item.kind);
    let suggested = match finding.kind {
        FindingKind::DeadPublic | FindingKind::UnnecessaryPublic => "pub(crate)".to_owned(),
        FindingKind::UnnecessaryRestricted => String::new(),
    };
    if item.kind == ItemKind::Variant {
        return variant_row(graph, finding);
    }
    let range = item.vis_span.map(|span| Range {
        line: span.line,
        column: span.column,
        end_line: span.end_line,
        end_column: span.end_column,
    });
    FindingRow {
        kind: finding.kind.label(),
        package: krate.package.clone(),
        krate: krate.name.clone(),
        item: graph.item_path(finding.item),
        item_kind: item.kind.label(),
        file: item.file.clone(),
        line: item.line,
        column: item.column,
        end_line: item.end_line,
        visibility: written.clone(),
        suggested: suggested.clone(),
        test_only: finding.test_only,
        actions: vec![Action {
            kind: CHANGE_ACTION,
            auto_fixable: false,
            description: description(finding.kind),
            from: written,
            to: suggested,
            range,
        }],
    }
}

/// A variant has no visibility keyword of its own, so its one step is to
/// remove it.
fn variant_row(graph: &Graph, finding: &Finding) -> FindingRow {
    let item = &graph.items[finding.item];
    let krate = &graph.crates[item.krate];
    FindingRow {
        kind: finding.kind.label(),
        package: krate.package.clone(),
        krate: krate.name.clone(),
        item: graph.item_path(finding.item),
        item_kind: item.kind.label(),
        file: item.file.clone(),
        line: item.line,
        column: item.column,
        end_line: item.end_line,
        visibility: String::new(),
        suggested: String::new(),
        test_only: finding.test_only,
        actions: vec![Action {
            kind: REMOVE_ACTION,
            auto_fixable: false,
            description: "An enum variant has no visibility keyword. Remove the variant when \
                          no code needs it.",
            from: String::new(),
            to: String::new(),
            range: None,
        }],
    }
}

/// The written visibility as text. An enum variant has the visibility of its
/// enum, with no keyword of its own.
fn written(vis: &Vis, kind: ItemKind) -> String {
    match vis {
        Vis::Public if kind == ItemKind::Variant => String::new(),
        Vis::Private => String::new(),
        Vis::Public => "pub".to_owned(),
        Vis::Crate => "pub(crate)".to_owned(),
        Vis::Super => "pub(super)".to_owned(),
        Vis::In(path) if path.len() == 1 && path[0] == "self" => "pub(self)".to_owned(),
        Vis::In(path) => format!("pub(in {})", path.join("::")),
    }
}
