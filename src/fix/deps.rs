//! The edits of `fix` for the `deps` findings.
//!
//! The edit uses `toml_edit`, so each comment and each format choice of the
//! manifest stays. An edit removes one dependency entry, or moves it to the
//! `[dev-dependencies]` table of the same platform. When no table of the
//! package declares the key after the edit, the feature values that name the
//! key go too.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use toml_edit::{DocumentMut, Item, Table, TableLike};

use crate::deps::manifest::normalize_platform;
use crate::deps::report::{DepsReport, Rule};

/// The section label of the workspace table.
const WORKSPACE_SECTION: &str = "workspace.dependencies";

/// The dependency tables of a package, in the written forms that Cargo reads.
const PACKAGE_TABLES: [&str; 5] = [
    "dependencies",
    "dev-dependencies",
    "dev_dependencies",
    "build-dependencies",
    "build_dependencies",
];

/// True when `fix` can apply the action of a finding of `rule`. A warning is
/// not fixed: a person decides it. An entry that turns on features is not
/// fixed either: see `Finding::manual`.
pub const fn fixes(rule: Rule) -> bool {
    matches!(
        rule,
        Rule::UnusedDependency
            | Rule::UnusedDevDependency
            | Rule::UnusedBuildDependency
            | Rule::UnusedWorkspaceDependency
            | Rule::MisplacedDependency
    )
}

/// One planned edit. The field order is the wire order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[schemars(rename = "FixEdit")]
pub struct Edit {
    /// The action type of the finding: `remove-dependency` or
    /// `move-to-dev-dependencies`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// The rule of the finding.
    pub rule: Rule,
    /// The package. A workspace entry has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// The `Cargo.toml` path, relative to the workspace root.
    pub manifest_path: String,
    /// The table of the entry.
    pub section: String,
    /// The platform of a `[target.<platform>.*]` table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// The key in `Cargo.toml`.
    pub dependency: String,
    /// The one-based line of the entry before the edit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// True when the command wrote the edit.
    pub applied: bool,
}

/// The kind of the move action.
const MOVE: &str = "move-to-dev-dependencies";

/// The edits for the fixable findings of a report, in report order.
pub fn plan(report: &DepsReport) -> Vec<Edit> {
    report
        .findings
        .iter()
        .filter_map(|finding| {
            let action = finding
                .actions
                .first()
                .filter(|action| action.auto_fixable)?;
            Some(Edit {
                kind: action.kind,
                rule: finding.rule,
                package: finding.package.clone(),
                manifest_path: finding.manifest_path.clone(),
                section: finding.section.clone(),
                platform: finding.platform.clone(),
                dependency: finding.dependency.clone(),
                line: finding.line,
                applied: false,
            })
        })
        .collect()
}

/// Applies the edits of one manifest to its text and returns the new text.
pub fn apply(text: &str, edits: &[&Edit]) -> Result<String, String> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|err| format!("the manifest does not parse: {err}"))?;
    let mut removed = BTreeSet::new();
    for edit in edits {
        let platform = edit.platform.as_deref();
        let entry = table(&mut doc, &edit.section, platform, &edit.dependency)
            .and_then(|table| {
                let key = table.key(&edit.dependency).cloned()?;
                table.remove(&edit.dependency).map(|item| (key, item))
            })
            .ok_or_else(|| {
                format!(
                    "the manifest has no `{}` entry in `{}`",
                    edit.dependency, edit.section
                )
            })?;
        if edit.kind == MOVE {
            let (key, item) = entry;
            let dev = dev_table(&mut doc, platform)
                .ok_or_else(|| "the manifest has no place for `[dev-dependencies]`".to_owned())?;
            // The key keeps its decor, so a comment above the entry moves with it.
            match dev.as_table_mut() {
                Some(table) => {
                    table.insert_formatted(&key, item);
                }
                None => {
                    dev.as_table_like_mut()
                        .ok_or_else(|| "`[dev-dependencies]` is not a table".to_owned())?
                        .insert(key.get(), item);
                }
            }
        } else if edit.section != WORKSPACE_SECTION {
            removed.insert(edit.dependency.as_str());
        }
    }
    for key in removed {
        if !declared(&doc, key) {
            remove_feature_values(&mut doc, key);
        }
    }
    Ok(doc.to_string())
}

/// The edits of each manifest, in manifest order.
pub fn by_manifest(edits: &[Edit]) -> BTreeMap<&str, Vec<&Edit>> {
    let mut grouped: BTreeMap<&str, Vec<&Edit>> = BTreeMap::new();
    for edit in edits {
        grouped
            .entry(edit.manifest_path.as_str())
            .or_default()
            .push(edit);
    }
    grouped
}

/// The table of a section with the key, on the platform when there is one.
/// Cargo also reads the `dev_dependencies` and `build_dependencies` spellings.
fn table<'a>(
    doc: &'a mut DocumentMut,
    section: &str,
    platform: Option<&str>,
    key: &str,
) -> Option<&'a mut dyn TableLike> {
    if section == WORKSPACE_SECTION {
        return doc
            .get_mut("workspace")?
            .get_mut("dependencies")?
            .as_table_like_mut();
    }
    let base = platform_base(doc, platform)?;
    let name = [section.to_owned(), section.replace('-', "_")]
        .into_iter()
        .find(|name| {
            base.get(name)
                .and_then(Item::as_table_like)
                .is_some_and(|table| table.contains_key(key))
        })?;
    base.get_mut(&name)?.as_table_like_mut()
}

/// The root table, or the `[target.<platform>]` table.
fn platform_base<'a>(
    doc: &'a mut DocumentMut,
    platform: Option<&str>,
) -> Option<&'a mut dyn TableLike> {
    let Some(platform) = platform else {
        return Some(doc.as_table_mut());
    };
    let target = doc.get_mut("target")?.as_table_like_mut()?;
    let wanted = normalize_platform(platform);
    let key = target
        .iter()
        .map(|(key, _)| key.to_owned())
        .find(|key| normalize_platform(key) == wanted)?;
    target.get_mut(&key)?.as_table_like_mut()
}

/// The `[dev-dependencies]` item of the platform. The function adds the table
/// when the manifest has none.
fn dev_table<'a>(doc: &'a mut DocumentMut, platform: Option<&str>) -> Option<&'a mut Item> {
    let base = platform_base(doc, platform)?;
    let name = if base.contains_key("dev_dependencies") && !base.contains_key("dev-dependencies") {
        "dev_dependencies"
    } else {
        "dev-dependencies"
    };
    if !base.contains_key(name) {
        base.insert(name, Item::Table(Table::new()));
    }
    base.get_mut(name)
}

/// True when a dependency table of the package, on any platform, still has
/// the key.
fn declared(doc: &DocumentMut, key: &str) -> bool {
    let has = |base: &dyn TableLike| {
        PACKAGE_TABLES.iter().any(|name| {
            base.get(name)
                .and_then(Item::as_table_like)
                .is_some_and(|table| table.contains_key(key))
        })
    };
    let targets = doc
        .get("target")
        .and_then(Item::as_table_like)
        .into_iter()
        .flat_map(|target| target.iter().filter_map(|(_, item)| item.as_table_like()));
    has(doc.as_table()) || targets.into_iter().any(has)
}

/// Removes `dep:key`, `key/feature` and `key?/feature` from each feature.
fn remove_feature_values(doc: &mut DocumentMut, key: &str) {
    let Some(features) = doc.get_mut("features").and_then(Item::as_table_like_mut) else {
        return;
    };
    let names_key = |value: &str| {
        value == format!("dep:{key}")
            || value
                .strip_prefix(key)
                .is_some_and(|rest| rest.starts_with('/') || rest.starts_with("?/"))
    };
    for (_, item) in features.iter_mut() {
        if let Some(values) = item.as_array_mut() {
            let before = values.len();
            let one_line = !values.to_string().contains('\n');
            values.retain(|value| !value.as_str().is_some_and(names_key));
            // A one-line array gets the plain format again, so no space stays
            // where a removed first value was. A multi-line array keeps its
            // lines.
            if one_line && values.len() != before {
                values.fmt();
            }
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "a test fails loudly when an edit does not apply"
)]
mod tests {
    use super::*;

    fn edit(kind: &'static str, section: &str, platform: Option<&str>, key: &str) -> Edit {
        Edit {
            kind,
            rule: Rule::UnusedDependency,
            package: Some("app".to_owned()),
            manifest_path: "Cargo.toml".to_owned(),
            section: section.to_owned(),
            platform: platform.map(str::to_owned),
            dependency: key.to_owned(),
            line: None,
            applied: false,
        }
    }

    #[test]
    fn removal_keeps_the_other_entries_and_their_comments() {
        let text = "[dependencies]\n# a\na = \"1\"\n# b\nb = \"1\" # tail\n# c\nc = \"1\"\n";
        let remove = edit("remove-dependency", "dependencies", None, "b");
        let out = apply(text, &[&remove]).expect("the edit applies");
        assert_eq!(out, "[dependencies]\n# a\na = \"1\"\n# c\nc = \"1\"\n");
    }

    #[test]
    fn removal_finds_the_platform_table_by_its_normal_form() {
        let text = "[target.\"cfg( unix )\".dependencies]\na = \"1\"\nb = \"1\"\n";
        let remove = edit("remove-dependency", "dependencies", Some("cfg(unix)"), "a");
        let out = apply(text, &[&remove]).expect("the edit applies");
        assert_eq!(out, "[target.\"cfg( unix )\".dependencies]\nb = \"1\"\n");
    }

    #[test]
    fn removal_cleans_the_feature_values_of_the_key() {
        let text = "[dependencies]\na = { version = \"1\", optional = true }\n\n[features]\nx = [\"dep:a\", \"a/f\", \"a?/g\", \"ab/f\"]\n";
        let remove = edit("remove-dependency", "dependencies", None, "a");
        let out = apply(text, &[&remove]).expect("the edit applies");
        assert!(out.contains("x = [\"ab/f\"]"), "{out}");
    }

    #[test]
    fn feature_values_stay_while_another_table_declares_the_key() {
        let text = "[dependencies]\na = \"1\"\n\n[build-dependencies]\na = \"1\"\n\n[features]\nx = [\"a/f\"]\n";
        let remove = edit("remove-dependency", "dependencies", None, "a");
        let out = apply(text, &[&remove]).expect("the edit applies");
        assert!(out.contains("x = [\"a/f\"]"), "{out}");
    }

    #[test]
    fn move_adds_the_dev_table_and_keeps_the_value() {
        let text = "[dependencies]\na = { version = \"1\", features = [\"x\"] }\n";
        let moved = edit(MOVE, "dependencies", None, "a");
        let out = apply(text, &[&moved]).expect("the edit applies");
        assert_eq!(
            out,
            "[dependencies]\n\n[dev-dependencies]\na = { version = \"1\", features = [\"x\"] }\n"
        );
    }

    #[test]
    fn missing_entry_is_an_error() {
        let remove = edit("remove-dependency", "dependencies", None, "z");
        assert!(apply("[dependencies]\na = \"1\"\n", &[&remove]).is_err());
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "a test fails loudly when an edit does not apply"
)]
mod more_tests {
    use super::*;

    fn edit(kind: &'static str, section: &str, key: &str) -> Edit {
        Edit {
            kind,
            rule: Rule::MisplacedDependency,
            package: Some("app".to_owned()),
            manifest_path: "Cargo.toml".to_owned(),
            section: section.to_owned(),
            platform: None,
            dependency: key.to_owned(),
            line: None,
            applied: false,
        }
    }

    #[test]
    fn move_keeps_the_comment_above_the_entry() {
        let text = "[dependencies]\n# for the tests\na = \"1\"\n\n[dev-dependencies]\nb = \"1\"\n";
        let out = apply(text, &[&edit(MOVE, "dependencies", "a")]).expect("the edit applies");
        assert!(
            out.contains("b = \"1\"\n# for the tests\na = \"1\"\n"),
            "{out}"
        );
    }

    #[test]
    fn the_table_with_the_key_wins_when_both_spellings_exist() {
        let text = "[dev-dependencies]\na = \"1\"\n\n[dev_dependencies]\nb = \"1\"\n";
        let out = apply(text, &[&edit("remove-dependency", "dev-dependencies", "b")])
            .expect("the edit applies");
        assert_eq!(out, "[dev-dependencies]\na = \"1\"\n\n[dev_dependencies]\n");
    }
}
