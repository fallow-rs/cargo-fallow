//! The scope of a run: the findings that the report keeps.
//!
//! The analysis always reads the whole workspace. The scope only removes
//! findings from the report, before the gates count them. A full scope keeps
//! each finding.

use serde_json::Value;

use crate::diff::ChangedFiles;
use crate::error::CliError;
use crate::section::{Section, to_value};
use crate::workspace::Workspace;

/// The line of the human output that names the changed files.
pub fn changed_line(changed: &ChangedFiles) -> String {
    let count = changed.files.len();
    let base: String = changed.base.chars().take(12).collect();
    format!(
        "Scope: {count} {} changed since `{}` (fork point {base})\n",
        if count == 1 { "file" } else { "files" },
        changed.reference,
    )
}

/// The flag that scopes a run to the changed files.
pub const CHANGED_SINCE_FLAG: &str = "--changed-since";

/// The findings that a run keeps.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    /// The changed files of `--changed-since`, or `None` for each file.
    pub changed: Option<ChangedFiles>,
}

impl Scope {
    /// The scope of the flags. `changed_since` is the ref of
    /// `--changed-since`.
    pub fn load(workspace: &Workspace, changed_since: Option<&str>) -> Result<Self, CliError> {
        Ok(Self {
            changed: changed_since
                .map(|reference| ChangedFiles::load(workspace, CHANGED_SINCE_FLAG, reference))
                .transpose()?,
        })
    }

    /// True when the scope keeps each finding.
    pub const fn is_full(&self) -> bool {
        self.changed.is_none()
    }

    /// True when the scope keeps a finding at `path`.
    pub fn keeps(&self, path: &str) -> bool {
        self.changed
            .as_ref()
            .is_none_or(|changed| changed.keeps(path))
    }

    /// True when the scope keeps a finding with one of these places.
    pub fn keeps_any<'a>(&self, paths: impl Iterator<Item = &'a str>) -> bool {
        self.changed
            .as_ref()
            .is_none_or(|changed| changed.keeps_any(paths))
    }

    /// The `changed_since` member of a JSON report, or `None` for a full
    /// scope.
    pub fn changed_since_json(&self) -> Result<Option<Value>, CliError> {
        self.changed.as_ref().map(to_value).transpose()
    }

    /// The line of the human output that names the scope, or `None` for a full
    /// scope.
    pub fn human_line(&self) -> Option<String> {
        self.changed.as_ref().map(changed_line)
    }

    /// The section of one command with the scope in its JSON and human output.
    pub fn annotate(&self, mut section: Section) -> Result<Section, CliError> {
        if let Some(value) = self.changed_since_json()?
            && let Value::Object(map) = &mut section.json
        {
            map.insert("changed_since".to_owned(), value);
        }
        if let Some(line) = self.human_line() {
            section.human.insert_str(0, &line);
        }
        Ok(section)
    }
}
