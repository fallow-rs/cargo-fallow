//! The compiler messages of the `cargo check` guard of `fix`.
//!
//! The guard compares the warnings before and after the edits. An edit that
//! adds a warning breaks a workspace that builds with `-D warnings`, so the
//! guard leaves it out, the same as an edit that adds an error.

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;

/// The lint that a `dead-public` edit makes the compiler report on purpose.
const DEAD_CODE: &str = "dead_code";

/// One compiler message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// True for an error, false for a warning.
    pub error: bool,
    /// The lint or error code, for example `private_interfaces` or `E0603`.
    pub code: String,
    /// The message.
    pub message: String,
    /// The file of the primary span.
    pub file: String,
    /// The one-based line of the primary span.
    pub line: u64,
    /// The one-based column of the primary span.
    pub column: u64,
}

impl Diagnostic {
    /// The message as one line in the short format of the compiler.
    pub fn line(&self) -> String {
        let level = if self.error { "error" } else { "warning" };
        let code = if self.code.is_empty() {
            String::new()
        } else {
            format!("[{}]", self.code)
        };
        if self.file.is_empty() {
            return format!("{level}{code}: {}", self.message);
        }
        format!(
            "{}:{}:{}: {level}{code}: {}",
            self.file, self.line, self.column, self.message
        )
    }

    /// The identity of a warning that an edit of a keyword does not change:
    /// the line and the column can move, so they are not part of it.
    fn key(&self) -> (&str, &str, &str) {
        (&self.file, &self.code, &self.message)
    }
}

/// The compiler messages in the JSON output of `cargo check`. A line that is
/// not a compiler message is skipped.
pub fn diagnostics(stdout: &str) -> Vec<Diagnostic> {
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|value| value["reason"] == "compiler-message")
        .filter_map(|value| {
            let message = &value["message"];
            let level = message["level"].as_str()?;
            if level != "warning" && level != "error" {
                return None;
            }
            let span = message["spans"]
                .as_array()
                .and_then(|spans| {
                    spans
                        .iter()
                        .find(|span| span["is_primary"] == true)
                        .or_else(|| spans.first())
                })
                .cloned()
                .unwrap_or(Value::Null);
            Some(Diagnostic {
                error: level == "error",
                code: message["code"]["code"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                message: message["message"].as_str().unwrap_or_default().to_owned(),
                file: span["file_name"].as_str().unwrap_or_default().to_owned(),
                line: span["line_start"].as_u64().unwrap_or(0),
                column: span["column_start"].as_u64().unwrap_or(0),
            })
        })
        .collect()
}

/// The warnings of `after` that `before` does not have. A warning that is
/// in both counts as new only when `after` has it more times. A warning that
/// only names what the warnings of its file and lint named before is not
/// new, for example "unused imports: `A` and `B`" after "unused import: `A`"
/// and "unused import: `B`". A `dead_code` warning is never new: a
/// `dead-public` edit shows the dead code on purpose.
pub fn new_warnings<'a>(before: &[Diagnostic], after: &'a [Diagnostic]) -> Vec<&'a Diagnostic> {
    let mut known: FxHashMap<(&str, &str, &str), usize> = FxHashMap::default();
    let mut named: FxHashMap<(&str, &str), FxHashSet<&str>> = FxHashMap::default();
    for warning in before {
        *known.entry(warning.key()).or_default() += 1;
        named
            .entry((&warning.file, &warning.code))
            .or_default()
            .extend(crate::fix::visibility::named_items(&warning.message));
    }
    after
        .iter()
        .filter(|warning| !warning.error && warning.code != DEAD_CODE)
        .filter(|warning| match known.get_mut(&warning.key()) {
            Some(count) if *count > 0 => {
                *count -= 1;
                false
            }
            _ => true,
        })
        .filter(|warning| {
            let names = crate::fix::visibility::named_items(&warning.message);
            let old = named.get(&(warning.file.as_str(), warning.code.as_str()));
            names.is_empty() || !old.is_some_and(|old| names.iter().all(|name| old.contains(name)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warning(code: &str, message: &str, line: u64) -> Diagnostic {
        Diagnostic {
            error: false,
            code: code.to_owned(),
            message: message.to_owned(),
            file: "src/lib.rs".to_owned(),
            line,
            column: 1,
        }
    }

    #[test]
    fn a_compiler_message_line_gives_a_diagnostic() {
        let stdout = "{\"reason\":\"compiler-artifact\"}\n\
            {\"reason\":\"compiler-message\",\"message\":{\"level\":\"warning\",\"message\":\"m\",\
            \"code\":{\"code\":\"private_interfaces\"},\"spans\":[{\"file_name\":\"a.rs\",\
            \"line_start\":3,\"column_start\":5,\"is_primary\":true}]}}\nnot json\n";
        let found = diagnostics(stdout);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line(), "a.rs:3:5: warning[private_interfaces]: m");
    }

    #[test]
    fn a_merged_warning_with_only_old_names_is_not_new() {
        let before = [warning("unused_imports", "unused import: `A`", 1)];
        let after = [
            warning("unused_imports", "unused imports: `A` and `B`", 1),
            warning("unused_imports", "unused import: `C`", 2),
        ];
        let new = new_warnings(&before, &after);
        assert_eq!(new.len(), 2, "B is new, so the merged warning is new too");
        let before = [
            warning("unused_imports", "unused import: `A`", 1),
            warning("unused_imports", "unused import: `B`", 1),
        ];
        let after = [warning("unused_imports", "unused imports: `A` and `B`", 1)];
        assert!(new_warnings(&before, &after).is_empty());
    }

    #[test]
    fn a_message_without_a_file_has_no_place() {
        let mut message = warning("", "aborting", 0);
        message.file = String::new();
        message.error = true;
        assert_eq!(message.line(), "error: aborting");
    }

    #[test]
    fn a_moved_warning_and_dead_code_are_not_new() {
        let before = [warning("unused_mut", "m", 3)];
        let after = [
            warning("unused_mut", "m", 9),
            warning("unused_mut", "m", 10),
            warning(DEAD_CODE, "function `f` is never used", 4),
        ];
        let new = new_warnings(&before, &after);
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].line, 10);
    }
}
