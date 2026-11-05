//! The hook configuration files of the harnesses: `.claude/settings.json` and
//! `.codex/hooks.json`.
//!
//! The merge keeps each other key, event and handler. A handler belongs to
//! cargo-fallow when its command names `/hooks/cargo-fallow-gate.sh`, so the
//! fallow gate (`fallow-gate.sh`) and this gate never remove each other.

use std::fs;
use std::io;
use std::path::Path;

use serde_json::{Map, Value, json};

use super::files::{Outcome, read_error, write_error};

/// The file name of the gate script.
pub const GATE_SCRIPT: &str = "cargo-fallow-gate.sh";

/// The part of a handler command that marks a cargo-fallow handler.
const OWNED_SUFFIX: &str = "/hooks/cargo-fallow-gate.sh";

/// The hook event of the gate.
const EVENT: &str = "PreToolUse";

/// How long the Claude Code harness waits for the gate, in seconds. The gate
/// runs `check` up to two times. A timeout keeps the gate open.
const CLAUDE_TIMEOUT_SECONDS: u64 = 300;

/// A harness that runs the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum Harness {
    /// Claude Code: `.claude/settings.json` and `.claude/hooks/`.
    Claude,
    /// Codex: `.codex/hooks.json` and `.codex/hooks/`.
    Codex,
}

impl Harness {
    /// Both harnesses.
    pub const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    /// The name in the output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// The folder of the harness in the project.
    pub const fn dir(self) -> &'static str {
        match self {
            Self::Claude => ".claude",
            Self::Codex => ".codex",
        }
    }

    /// The hook configuration file, relative to the project.
    pub fn settings_path(self) -> String {
        match self {
            Self::Claude => ".claude/settings.json".to_owned(),
            Self::Codex => ".codex/hooks.json".to_owned(),
        }
    }

    /// The gate script, relative to the project.
    pub fn script_path(self) -> String {
        format!("{}/hooks/{GATE_SCRIPT}", self.dir())
    }

    /// The tool matcher of the handler. Codex reads a regular expression.
    const fn matcher(self) -> &'static str {
        match self {
            Self::Claude => "Bash",
            Self::Codex => "^Bash$",
        }
    }

    /// The handler command. It walks up from the folder of the harness
    /// process to the nearest folder with the gate script, and stops at the
    /// first `.git` entry, so a nested worktree does not reach the gate of the
    /// checkout around it. Claude Code also tries `$CLAUDE_PROJECT_DIR`. When
    /// no script is found, the command exits 0, which keeps the gate open.
    pub fn command(self) -> String {
        let script = self.script_path();
        let walk = format!(
            "d=\"$(pwd)\"; until [ -f \"$d/{script}\" ] || [ -e \"$d/.git\" ] || [ \"$d\" = / ]; do d=\"$(dirname \"$d\")\"; done; if [ -f \"$d/{script}\" ]; then exec \"$d/{script}\"; fi; "
        );
        match self {
            Self::Claude => format!(
                "{walk}if [ -f \"$CLAUDE_PROJECT_DIR/{script}\" ]; then exec \"$CLAUDE_PROJECT_DIR/{script}\"; fi; exit 0"
            ),
            Self::Codex => format!("{walk}exit 0"),
        }
    }

    /// The handler object.
    fn handler(self) -> Value {
        let mut handler = json!({ "type": "command", "command": self.command() });
        if self == Self::Claude {
            handler["timeout"] = json!(CLAUDE_TIMEOUT_SECONDS);
        }
        handler
    }
}

/// True when a handler belongs to cargo-fallow.
fn owned(handler: &Value) -> bool {
    handler
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(OWNED_SUFFIX))
}

/// Reads a JSON object file. A missing or empty file is an empty object. A
/// file that is not a JSON object is an error, so the command does not
/// change it.
fn read_object(path: &Path) -> Result<Option<Map<String, Value>>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(read_error(path, &err)),
    };
    if text.trim().is_empty() {
        return Ok(Some(Map::new()));
    }
    match serde_json::from_str(&text) {
        Ok(Value::Object(map)) => Ok(Some(map)),
        _ => Err(format!(
            "{} is not a JSON object. Fix the file, then run the command again.",
            path.display()
        )),
    }
}

/// Writes a JSON object with two spaces of indent and a newline.
fn write_object(path: &Path, map: &Map<String, Value>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| write_error(parent, &err))?;
    }
    let mut text = serde_json::to_string_pretty(map).map_err(|err| err.to_string())?;
    text.push('\n');
    fs::write(path, text).map_err(|err| write_error(path, &err))
}

/// The `PreToolUse` entries of a configuration, without the cargo-fallow
/// handlers. An entry that loses its last handler goes.
fn without_owned(map: &Map<String, Value>) -> (Vec<Value>, bool) {
    let entries = map
        .get("hooks")
        .and_then(|hooks| hooks.get(EVENT))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut removed = false;
    let mut kept = Vec::new();
    for mut entry in entries {
        if let Some(handlers) = entry.get_mut("hooks").and_then(Value::as_array_mut) {
            let before = handlers.len();
            handlers.retain(|handler| !owned(handler));
            if handlers.len() != before {
                removed = true;
                if handlers.is_empty() {
                    continue;
                }
            }
        }
        kept.push(entry);
    }
    (kept, removed)
}

/// Puts the `PreToolUse` entries back into the configuration. An empty list
/// removes the event, and an empty `hooks` object goes too.
fn with_entries(mut map: Map<String, Value>, entries: Vec<Value>) -> Map<String, Value> {
    let mut hooks = match map.remove("hooks") {
        Some(Value::Object(hooks)) => hooks,
        _ => Map::new(),
    };
    if entries.is_empty() {
        hooks.remove(EVENT);
    } else {
        hooks.insert(EVENT.to_owned(), Value::Array(entries));
    }
    if !hooks.is_empty() {
        map.insert("hooks".to_owned(), Value::Object(hooks));
    }
    map
}

/// Adds the gate handler to the configuration of `harness` in `root`.
pub fn install(root: &Path, harness: Harness, dry_run: bool) -> Result<Outcome, String> {
    let path = root.join(harness.settings_path());
    let current = read_object(&path)?;
    let existed = current.is_some();
    let map = current.unwrap_or_default();
    let (mut entries, _) = without_owned(&map);
    entries.push(json!({ "matcher": harness.matcher(), "hooks": [harness.handler()] }));
    let next = with_entries(map.clone(), entries);
    let outcome = if !existed {
        Outcome::Created
    } else if next == map {
        Outcome::Unchanged
    } else {
        Outcome::Updated
    };
    if !dry_run && outcome != Outcome::Unchanged {
        write_object(&path, &next)?;
    }
    Ok(outcome)
}

/// Removes the gate handler from the configuration of `harness` in `root`.
/// A file that only held the handler goes.
pub fn uninstall(root: &Path, harness: Harness, dry_run: bool) -> Result<Outcome, String> {
    let path = root.join(harness.settings_path());
    let Some(map) = read_object(&path)? else {
        return Ok(Outcome::NotPresent);
    };
    let (entries, removed) = without_owned(&map);
    if !removed {
        return Ok(Outcome::NotPresent);
    }
    let next = with_entries(map, entries);
    if !dry_run {
        if next.is_empty() {
            fs::remove_file(&path).map_err(|err| write_error(&path, &err))?;
        } else {
            write_object(&path, &next)?;
        }
    }
    Ok(Outcome::Removed)
}

/// True when the configuration of `harness` in `root` has the gate handler.
pub fn has_handler(root: &Path, harness: Harness) -> bool {
    read_object(&root.join(harness.settings_path()))
        .ok()
        .flatten()
        .is_some_and(|map| without_owned(&map).1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap_or_else(|err| panic!("{err}"))
    }

    fn read(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap_or_default()).unwrap_or(Value::Null)
    }

    #[test]
    fn install_keeps_the_other_handlers_and_runs_once() {
        let root = temp();
        let path = root.path().join(".claude/settings.json");
        fs::create_dir_all(root.path().join(".claude")).unwrap_or_else(|err| panic!("{err}"));
        let fallow = json!({ "type": "command", "command": "\"$CLAUDE_PROJECT_DIR\"/.claude/hooks/fallow-gate.sh" });
        let other = json!({
            "model": "x",
            "hooks": { "PreToolUse": [{ "matcher": "Bash", "hooks": [fallow] }] },
        });
        fs::write(&path, other.to_string()).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(
            install(root.path(), Harness::Claude, false),
            Ok(Outcome::Updated)
        );
        assert_eq!(
            install(root.path(), Harness::Claude, false),
            Ok(Outcome::Unchanged)
        );
        let value = read(&path);
        assert_eq!(value["model"], "x");
        let entries = value["hooks"]["PreToolUse"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["hooks"][0], fallow);
        assert!(has_handler(root.path(), Harness::Claude));

        assert_eq!(
            uninstall(root.path(), Harness::Claude, false),
            Ok(Outcome::Removed)
        );
        assert_eq!(read(&path), other);
        assert!(!has_handler(root.path(), Harness::Claude));
    }

    #[test]
    fn uninstall_removes_a_file_that_only_held_the_handler() {
        let root = temp();
        assert_eq!(
            install(root.path(), Harness::Codex, false),
            Ok(Outcome::Created)
        );
        let value = read(&root.path().join(".codex/hooks.json"));
        assert_eq!(value["hooks"]["PreToolUse"][0]["matcher"], "^Bash$");
        assert_eq!(
            uninstall(root.path(), Harness::Codex, false),
            Ok(Outcome::Removed)
        );
        assert!(!root.path().join(".codex/hooks.json").exists());
        assert_eq!(
            uninstall(root.path(), Harness::Codex, false),
            Ok(Outcome::NotPresent)
        );
    }

    #[test]
    fn a_file_that_is_not_an_object_is_not_changed() {
        let root = temp();
        let path = root.path().join(".claude/settings.json");
        fs::create_dir_all(root.path().join(".claude")).unwrap_or_else(|err| panic!("{err}"));
        fs::write(&path, "[1,").unwrap_or_else(|err| panic!("{err}"));
        assert!(install(root.path(), Harness::Claude, false).is_err());
        assert_eq!(fs::read_to_string(&path).ok().as_deref(), Some("[1,"));
    }
}
