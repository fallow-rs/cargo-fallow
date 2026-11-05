//! The `feature-unification-drift` rule: a crate that the build of one member
//! and the build of the workspace compile with different features.
//!
//! Cargo resolves features for each build. When one member turns on a feature
//! of a crate that another member also uses, `cargo build -p <member>` and
//! `cargo build --workspace` compile two variants of that crate and of each
//! crate that depends on it. A switch between the two commands then compiles
//! the crates again. A feature-only entry in the manifest of the member pins
//! the features, so both builds are equal.
//!
//! The rule is opt-in: `unify-features` in `[workspace.metadata.fallow.deps]`
//! names the members to compare. It runs `cargo tree` for each named member
//! and once for the workspace, with the normal and the build edges. Features
//! that a dev-dependency adds for `cargo test` are out of scope.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::error::{CliError, ErrorCode};
use crate::workspace::Workspace;

use super::report::{Action, Finding, Rule, SkippedPackage};

/// The arguments of `cargo tree` after the scope.
pub const TREE_ARGS: [&str; 6] = [
    "--edges",
    "normal,build",
    "--prefix",
    "none",
    "--format",
    "{p} [{f}]",
];

/// The findings and the members that the rule could not judge.
#[derive(Debug, Default)]
pub struct Drift {
    /// The findings.
    pub findings: Vec<Finding>,
    /// The members whose `cargo tree` run failed.
    pub skipped: Vec<SkippedPackage>,
}

/// The members that `unify-features` names. A name that is no workspace
/// member is an error.
pub fn members(
    workspace: &Workspace,
    metadata: &serde_json::Value,
) -> Result<Vec<String>, CliError> {
    let Some(list) = metadata["fallow"]["deps"].get("unify-features") else {
        return Ok(Vec::new());
    };
    let invalid = || {
        CliError::new(
            ErrorCode::InvalidConfig,
            "`unify-features` in `[workspace.metadata.fallow.deps]` is not a list of package names",
        )
        .with_help("Write the setting as `unify-features = [\"my-cli\"]`.")
    };
    let names: Vec<String> = list
        .as_array()
        .ok_or_else(invalid)?
        .iter()
        .map(|name| name.as_str().map(str::to_owned).ok_or_else(invalid))
        .collect::<Result<_, _>>()?;
    for name in &names {
        if !workspace.members.iter().any(|member| &member.name == name) {
            return Err(CliError::new(
                ErrorCode::InvalidConfig,
                format!("`unify-features` names `{name}`, which is no workspace member"),
            )
            .with_help("Write the package name of a workspace member."));
        }
    }
    Ok(names)
}

/// Compares the tree of each member with the tree of the workspace. `tree`
/// runs `cargo tree` with the scope arguments and returns its output.
pub fn check(
    workspace: &Workspace,
    members: &[String],
    tree: impl Fn(&[&str]) -> Result<String, String>,
) -> Drift {
    let mut drift = Drift::default();
    if members.is_empty() {
        return drift;
    }
    let whole = match tree(&["--workspace"]) {
        Ok(text) => parse_tree(&text),
        Err(reason) => {
            drift
                .skipped
                .extend(members.iter().map(|member| skipped(member, &reason)));
            return drift;
        }
    };
    for member in members {
        let own = match tree(&["--package", member]) {
            Ok(text) => parse_tree(&text),
            Err(reason) => {
                drift.skipped.push(skipped(member, &reason));
                continue;
            }
        };
        let manifest = workspace
            .members
            .iter()
            .find(|candidate| &candidate.name == member)
            .map(|candidate| candidate.manifest_path.clone())
            .unwrap_or_default();
        let entries = std::fs::read_to_string(workspace.root.as_std_path().join(&manifest))
            .map(|text| super::manifest::entries(&text))
            .unwrap_or_default();
        // The member itself is no dependency that it can pin.
        for difference in differences(&own, &whole)
            .into_iter()
            .filter(|difference| &difference.name != member)
        {
            let mut found = finding(member, &manifest, difference);
            place(&mut found, &entries);
            drift.findings.push(found);
        }
    }
    drift
}

fn skipped(member: &str, reason: &str) -> SkippedPackage {
    SkippedPackage {
        package: member.to_owned(),
        reason: format!(
            "`cargo tree` failed, so the `feature-unification-drift` rule cannot judge it: {reason}"
        ),
    }
}

/// The crates of one tree, as `(name, version)`, with the union of their
/// features.
type Tree = BTreeMap<(String, String), BTreeSet<String>>;

/// Reads the output of `cargo tree --prefix none --format "{p} [{f}]"`. A
/// line is `name vX.Y.Z (source) [features]`, and a repeated crate ends with
/// `(*)`.
pub fn parse_tree(text: &str) -> Tree {
    let mut tree = Tree::new();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let (Some(name), Some(version)) = (words.next(), words.next()) else {
            continue;
        };
        let Some(version) = version.strip_prefix('v') else {
            continue;
        };
        let (Some(open), Some(close)) = (line.rfind('['), line.rfind(']')) else {
            continue;
        };
        if close < open {
            continue;
        }
        let features = tree
            .entry((name.to_owned(), version.to_owned()))
            .or_default();
        features.extend(
            line[open + 1..close]
                .split(',')
                .map(str::trim)
                .filter(|feature| !feature.is_empty())
                .map(str::to_owned),
        );
    }
    tree
}

/// One crate that both builds compile with different features.
#[derive(Debug, PartialEq, Eq)]
pub struct Difference {
    /// The crate name.
    pub name: String,
    /// The version.
    pub version: String,
    /// The features that only the workspace build turns on.
    pub workspace_only: Vec<String>,
    /// The features that only the build of the member turns on.
    pub member_only: Vec<String>,
}

/// The crates of the member tree that the workspace tree compiles with other
/// features. A crate that only the workspace build compiles is no problem.
pub fn differences(own: &Tree, whole: &Tree) -> Vec<Difference> {
    own.iter()
        .filter_map(|((name, version), features)| {
            let other = whole.get(&(name.clone(), version.clone()))?;
            let workspace_only: Vec<String> = other.difference(features).cloned().collect();
            let member_only: Vec<String> = features.difference(other).cloned().collect();
            (!workspace_only.is_empty() || !member_only.is_empty()).then(|| Difference {
                name: name.clone(),
                version: version.clone(),
                workspace_only,
                member_only,
            })
        })
        .collect()
}

fn finding(member: &str, manifest: &str, difference: Difference) -> Finding {
    let Difference {
        name,
        version,
        workspace_only,
        member_only,
    } = difference;
    let mut description = format!(
        "`cargo build -p {member}` and `cargo build --workspace` compile `{name}` {version} with \
         different features."
    );
    if !workspace_only.is_empty() {
        let _ = write!(
            description,
            " Add a feature-only entry `{name} = {{ version = \"{version}\", features = [{}] }}` \
             to the [dependencies] of `{member}`, or remove the features from the member that \
             turns them on.",
            quoted(&workspace_only)
        );
    }
    if !member_only.is_empty() {
        let _ = write!(
            description,
            " Only the build of `{member}` turns on {}.",
            quoted(&member_only)
        );
    }
    Finding {
        rule: Rule::FeatureUnificationDrift,
        severity: Rule::FeatureUnificationDrift.severity(),
        package: Some(member.to_owned()),
        manifest_path: manifest.to_owned(),
        line: None,
        section: "dependencies".to_owned(),
        platform: None,
        dependency: name,
        package_name: None,
        optional: false,
        used_by: Vec::new(),
        in_dev: false,
        manual: None,
        version: Some(version),
        workspace_only_features: workspace_only,
        member_only_features: member_only,
        actions: vec![Action {
            kind: "pin-features",
            auto_fixable: false,
            description,
            file: manifest.to_owned(),
            line: None,
        }],
    }
}

/// Puts the finding on the line of the entry of the crate, when the member
/// declares it in `[dependencies]` or `[build-dependencies]` for each
/// platform. A suppression comment can then name one crate.
fn place(found: &mut Finding, entries: &[super::manifest::Entry]) {
    use super::manifest::Section;
    let entry = entries.iter().find(|entry| {
        entry.key == found.dependency
            && entry.platform.is_none()
            && matches!(entry.section, Section::Normal | Section::Build)
    });
    if let Some(entry) = entry {
        found.line = Some(entry.line);
        entry.section.label().clone_into(&mut found.section);
        for action in &mut found.actions {
            action.line = Some(entry.line);
        }
    }
}

fn quoted(features: &[String]) -> String {
    features
        .iter()
        .map(|feature| format!("\"{feature}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_line_gives_the_crate_and_its_features() {
        let tree = parse_tree(
            "app v0.1.0 (/w/app) []\nbase v0.1.0 (/w/base) [fast]\nserde v1.0.200 [derive,std] (*)\n\
             noise\nbase v0.1.0 (/w/base) [slow]\n",
        );
        let base = &tree[&("base".to_owned(), "0.1.0".to_owned())];
        assert_eq!(base.iter().collect::<Vec<_>>(), ["fast", "slow"]);
        assert_eq!(tree[&("serde".to_owned(), "1.0.200".to_owned())].len(), 2);
        assert!(tree[&("app".to_owned(), "0.1.0".to_owned())].is_empty());
    }

    #[test]
    fn only_a_shared_crate_with_other_features_differs() {
        let own = parse_tree("base v1.0.0 [a]\nsame v1.0.0 [x]\n");
        let whole = parse_tree("base v1.0.0 [a,b]\nsame v1.0.0 [x]\nonly v1.0.0 [y]\n");
        assert_eq!(
            differences(&own, &whole),
            [Difference {
                name: "base".to_owned(),
                version: "1.0.0".to_owned(),
                workspace_only: vec!["b".to_owned()],
                member_only: Vec::new(),
            }]
        );
    }
}
