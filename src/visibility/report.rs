//! The report of the experimental `visibility` command.

use std::fmt::Write as _;

use serde::Serialize;

use crate::graph::Graph;
use crate::visibility::{Analysis, Finding};

/// The schema name. The command is an experiment, so the shape can change
/// without a version step.
pub const SCHEMA: &str = "cargo-fallow-visibility-experimental";

/// The report.
#[derive(Debug, Clone, Serialize)]
pub struct VisibilityReport {
    /// The schema name.
    pub schema: &'static str,
    /// The size of the graph.
    pub graph: GraphSummary,
    /// The crates of the graph: one for each target.
    pub crates: Vec<CrateRow>,
    /// The findings.
    pub findings: Vec<FindingRow>,
    /// The findings that only an untyped credit stops: a method call, a field
    /// access, or a name in macro tokens, in an attribute or in a doc block.
    pub undecided: Vec<FindingRow>,
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
    /// The imports.
    pub imports: usize,
    /// The references.
    pub references: usize,
    /// The parsed files, counted once for each crate.
    pub files: u32,
    /// The files that a declaration reached but that did not parse.
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
    /// The one-based column of the name.
    pub column: u32,
    /// The one-based last line of the item.
    pub end_line: u32,
    /// True when only test code reaches the item.
    pub test_only: bool,
}

impl VisibilityReport {
    /// Builds the report.
    pub fn new(graph: &Graph, analysis: &Analysis) -> Self {
        let rows =
            |findings: &[Finding]| findings.iter().map(|finding| row(graph, finding)).collect();
        Self {
            schema: SCHEMA,
            graph: GraphSummary {
                crates: graph.crates.len(),
                modules: graph.modules.len(),
                items: graph.items.len(),
                imports: graph.imports.len(),
                references: graph.refs.len(),
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
                .map(|entry| CrateRow {
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
                })
                .collect(),
            findings: rows(&analysis.findings),
            undecided: rows(&analysis.undecided),
        }
    }

    /// The report as text for a person.
    pub fn human(&self) -> String {
        let mut text = String::new();
        for finding in &self.findings {
            let _ = writeln!(
                text,
                "{}:{}:{} {} {} `{}`",
                finding.file,
                finding.line,
                finding.column,
                finding.kind,
                finding.item_kind,
                finding.item
            );
        }
        let _ = writeln!(
            text,
            "{} findings, {} undecided, in {} crates, {} files, {} items",
            self.findings.len(),
            self.undecided.len(),
            self.graph.crates,
            self.graph.files,
            self.graph.items
        );
        text
    }
}

fn row(graph: &Graph, finding: &Finding) -> FindingRow {
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
        test_only: finding.test_only,
    }
}
