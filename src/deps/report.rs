//! The report of a `deps` run.

use std::collections::BTreeMap;

use fallow_report_protocol::GateOutcome;
use serde::Serialize;

use crate::gates::RustGateName;
use crate::report::TOOL_VERSION;

/// The version of the report shape. A change that removes a member or
/// changes its type increments it.
pub const SCHEMA_VERSION: u32 = 1;

/// The rule of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rule {
    /// A `[dependencies]` entry that no code uses.
    UnusedDependency,
    /// A `[dev-dependencies]` entry that no code uses.
    UnusedDevDependency,
    /// A `[build-dependencies]` entry that the build script does not use.
    UnusedBuildDependency,
    /// A `[workspace.dependencies]` entry that no member inherits.
    UnusedWorkspaceDependency,
    /// A `[dependencies]` entry that only test code, benches or examples use.
    MisplacedDependency,
    /// An optional dependency that no code uses. A feature names it.
    UnusedOptionalDependency,
    /// A dependency that no code uses, and that a feature of the package
    /// configures, for example `std = ["dep/std"]`.
    FeatureOnlyDependency,
    /// An ignore entry that hides no finding.
    RedundantIgnore,
}

impl Rule {
    /// The wire name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnusedDependency => "unused-dependency",
            Self::UnusedDevDependency => "unused-dev-dependency",
            Self::UnusedBuildDependency => "unused-build-dependency",
            Self::UnusedWorkspaceDependency => "unused-workspace-dependency",
            Self::MisplacedDependency => "misplaced-dependency",
            Self::UnusedOptionalDependency => "unused-optional-dependency",
            Self::FeatureOnlyDependency => "feature-only-dependency",
            Self::RedundantIgnore => "redundant-ignore",
        }
    }

    /// The severity. Only an `error` finding counts for the gate.
    pub const fn severity(self) -> Severity {
        match self {
            Self::UnusedDependency
            | Self::UnusedDevDependency
            | Self::UnusedBuildDependency
            | Self::UnusedWorkspaceDependency
            | Self::MisplacedDependency => Severity::Error,
            Self::UnusedOptionalDependency
            | Self::FeatureOnlyDependency
            | Self::RedundantIgnore => Severity::Warning,
        }
    }

    /// A one-line description of the rule.
    pub const fn description(self) -> &'static str {
        match self {
            Self::UnusedDependency => "A [dependencies] entry that no code of the package uses.",
            Self::UnusedDevDependency => {
                "A [dev-dependencies] entry that no test, bench or example uses."
            }
            Self::UnusedBuildDependency => {
                "A [build-dependencies] entry that the build script does not use."
            }
            Self::UnusedWorkspaceDependency => {
                "A [workspace.dependencies] entry that no member declares."
            }
            Self::MisplacedDependency => {
                "A [dependencies] entry that only tests, benches or examples use."
            }
            Self::UnusedOptionalDependency => {
                "An optional dependency that no code uses. A feature enables it."
            }
            Self::FeatureOnlyDependency => {
                "A dependency that no code uses. A feature of the package sets its features."
            }
            Self::RedundantIgnore => "An ignore entry that hides no finding.",
        }
    }
}

/// The severity of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Counts for the `error-severity-findings` gate.
    Error,
    /// Does not count for a gate. The JSON value is `warn`.
    #[serde(rename = "warn")]
    Warning,
}

/// One finding. The field order is the wire order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// The rule.
    pub rule: Rule,
    /// The severity of the rule.
    pub severity: Severity,
    /// The package. A workspace finding has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// The `Cargo.toml` path, relative to the workspace root.
    pub manifest_path: String,
    /// The one-based line of the entry, when the scan found it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// The table: `dependencies`, `dev-dependencies`, `build-dependencies`,
    /// `workspace.dependencies`, or the ignore table of a `redundant-ignore`.
    pub section: String,
    /// The platform of a `[target.<platform>.*]` table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// The key in `Cargo.toml`: the dependency name or its rename.
    pub dependency: String,
    /// The package name of a renamed dependency.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_name: Option<String>,
    /// True for an optional dependency.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    /// The parts of the package that use the dependency. Only a
    /// `misplaced-dependency` finding has them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub used_by: Vec<&'static str>,
    /// True when `[dev-dependencies]` declares the same package. It is not
    /// part of the report.
    #[serde(skip)]
    pub in_dev: bool,
    /// True when the entry turns on features of the dependency. Another
    /// dependency can need those features through Cargo feature unification,
    /// so `fix` does not change the entry. It is not part of the report.
    #[serde(skip)]
    pub sets_features: bool,
    /// The next steps. `cargo fallow fix` applies a step with `auto_fixable`.
    pub actions: Vec<Action>,
}

/// A next step for one finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Action {
    /// The kind of the step.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// True when `cargo fallow fix` applies the step.
    pub auto_fixable: bool,
    /// What to do.
    pub description: String,
    /// The file to change, relative to the workspace root.
    pub file: String,
    /// The one-based line to change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

/// A package that the run did not judge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedPackage {
    /// The package name.
    pub package: String,
    /// The reason.
    pub reason: String,
}

/// The report of a `deps` run. The field order is the wire order.
#[derive(Debug, Clone, Serialize)]
pub struct DepsReport {
    /// The version of the report shape.
    pub schema_version: u32,
    /// The version of cargo-fallow.
    pub version: &'static str,
    /// Always `deps`.
    pub kind: &'static str,
    /// Always `rust`.
    pub language: &'static str,
    /// The number of workspace members.
    pub members: usize,
    /// The number of findings.
    pub total_issues: usize,
    /// The findings, sorted by manifest, line, table, key, platform, rule,
    /// package and severity.
    pub findings: Vec<Finding>,
    /// The packages that the run did not judge.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<SkippedPackage>,
    /// The gate outcomes, keyed by gate name.
    pub gate_outcomes: BTreeMap<RustGateName, GateOutcome>,
}

impl DepsReport {
    /// Builds the report.
    pub fn new(
        members: usize,
        mut findings: Vec<Finding>,
        skipped: Vec<SkippedPackage>,
        gate: GateOutcome,
    ) -> Self {
        findings.sort_by(|a, b| {
            (
                &a.manifest_path,
                a.line,
                &a.section,
                &a.dependency,
                &a.platform,
                a.rule,
                &a.package,
                a.severity,
            )
                .cmp(&(
                    &b.manifest_path,
                    b.line,
                    &b.section,
                    &b.dependency,
                    &b.platform,
                    b.rule,
                    &b.package,
                    b.severity,
                ))
        });
        let mut gate_outcomes = BTreeMap::new();
        gate_outcomes.insert(RustGateName::ErrorSeverityFindings, gate);
        Self {
            schema_version: SCHEMA_VERSION,
            version: TOOL_VERSION,
            kind: "deps",
            language: "rust",
            members,
            total_issues: findings.len(),
            findings,
            skipped,
            gate_outcomes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(package: Option<&str>, severity: Severity) -> Finding {
        Finding {
            rule: Rule::UnusedDependency,
            severity,
            package: package.map(str::to_owned),
            manifest_path: "Cargo.toml".to_owned(),
            line: Some(3),
            section: "dependencies".to_owned(),
            platform: None,
            dependency: "alpha".to_owned(),
            package_name: None,
            optional: false,
            used_by: Vec::new(),
            in_dev: false,
            sets_features: false,
            actions: Vec::new(),
        }
    }

    fn order(findings: Vec<Finding>) -> Vec<(Option<String>, Severity)> {
        DepsReport::new(
            1,
            findings,
            Vec::new(),
            GateOutcome::new(fallow_report_protocol::GateStatus::Pass, false),
        )
        .findings
        .into_iter()
        .map(|found| (found.package, found.severity))
        .collect()
    }

    #[test]
    fn findings_that_differ_only_in_package_or_severity_have_a_fixed_order() {
        let all = vec![
            finding(Some("b"), Severity::Error),
            finding(None, Severity::Error),
            finding(Some("a"), Severity::Warning),
            finding(Some("a"), Severity::Error),
        ];
        let mut reversed = all.clone();
        reversed.reverse();
        assert_eq!(order(all), order(reversed));
    }
}
