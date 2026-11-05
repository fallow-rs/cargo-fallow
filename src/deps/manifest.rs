//! A line scanner for the dependency tables of a `Cargo.toml` file.
//!
//! `cargo metadata` gives the dependencies, but not their lines and not the
//! `[workspace.dependencies]` table. This scanner reads only the table headers
//! and the keys of the dependency tables. It does not parse the values.

/// The section of a dependency table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    /// `[dependencies]`.
    Normal,
    /// `[dev-dependencies]`.
    Dev,
    /// `[build-dependencies]`.
    Build,
    /// `[workspace.dependencies]`.
    Workspace,
}

impl Section {
    /// The table name in the report.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Normal => "dependencies",
            Self::Dev => "dev-dependencies",
            Self::Build => "build-dependencies",
            Self::Workspace => "workspace.dependencies",
        }
    }

    fn of(name: &str) -> Option<Self> {
        match name {
            "dependencies" => Some(Self::Normal),
            "dev-dependencies" | "dev_dependencies" => Some(Self::Dev),
            "build-dependencies" | "build_dependencies" => Some(Self::Build),
            _ => None,
        }
    }
}

/// One dependency key in a dependency table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The section.
    pub section: Section,
    /// The platform of a `[target.<platform>.*]` table, without quotes.
    pub platform: Option<String>,
    /// The key: the dependency name or its rename.
    pub key: String,
    /// The one-based line of the key or of the `[dependencies.<key>]` header.
    pub line: u32,
    /// True when the entry inherits from `[workspace.dependencies]` with
    /// `workspace = true`.
    pub inherits: bool,
}

/// The table that the scanner is in.
#[derive(Debug, Clone)]
enum Table {
    /// A table with one dependency for each key.
    Deps(Section, Option<String>),
    /// A `[dependencies.<key>]` table, with the index of its entry.
    Dep(usize),
    /// Any other table.
    Other,
}

/// The dependency keys of a manifest, in file order. A key appears once for
/// each table.
pub fn entries(text: &str) -> Vec<Entry> {
    let mut found: Vec<Entry> = Vec::new();
    let mut table = Table::Other;
    let mut depth = 0_i32;
    for (index, raw) in text.lines().enumerate() {
        let line = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let code = strip_comment(raw);
        let trimmed = code.trim();
        if depth > 0 {
            depth += bracket_balance(trimmed);
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        if let Some(header) = trimmed.strip_prefix('[') {
            table = header_table(header, &mut found, line);
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        depth = bracket_balance(value);
        record_key(&table, key, value.trim(), &mut found, line);
    }
    found
}

/// The table that a header line opens. The text starts after the first `[`.
/// A `[dependencies.<key>]` header adds its entry to `found`.
fn header_table(header: &str, found: &mut Vec<Entry>, line: u32) -> Table {
    if header.starts_with('[') {
        return Table::Other;
    }
    let Some(name) = header.rfind(']').map(|end| &header[..end]) else {
        return Table::Other;
    };
    let parts = split_dotted(name);
    match dependency_table(&parts) {
        Some((section, platform, Some(key))) => {
            Table::Dep(push(found, section, platform, key, line))
        }
        Some((section, platform, None)) => Table::Deps(section, platform),
        None => Table::Other,
    }
}

/// Records one `key = value` line of the current table.
fn record_key(table: &Table, key: &str, value: &str, found: &mut Vec<Entry>, line: u32) {
    let parts = split_dotted(key);
    match table {
        Table::Deps(section, platform) => {
            let inherits = match parts.as_slice() {
                [_] => inline_inherits(value),
                [_, field] => field == "workspace" && value == "true",
                _ => false,
            };
            if let Some(first) = parts.into_iter().next() {
                let index = push(found, *section, platform.clone(), first, line);
                found[index].inherits |= inherits;
            }
        }
        Table::Dep(index) => {
            if parts == ["workspace"] && value == "true" {
                found[*index].inherits = true;
            }
        }
        Table::Other => {}
    }
}

/// True when an inline table has `workspace = true`.
fn inline_inherits(value: &str) -> bool {
    let compact: String = value.chars().filter(|ch| !ch.is_whitespace()).collect();
    compact.starts_with('{')
        && (compact.contains("{workspace=true") || compact.contains(",workspace=true"))
}

fn push(
    found: &mut Vec<Entry>,
    section: Section,
    platform: Option<String>,
    key: String,
    line: u32,
) -> usize {
    if let Some(index) = found.iter().position(|entry| {
        entry.section == section && entry.platform == platform && entry.key == key
    }) {
        return index;
    }
    found.push(Entry {
        section,
        platform,
        key,
        line,
        inherits: false,
    });
    found.len() - 1
}

/// The section, the platform and the dependency key of a table header.
fn dependency_table(parts: &[String]) -> Option<(Section, Option<String>, Option<String>)> {
    let strs: Vec<&str> = parts.iter().map(String::as_str).collect();
    match strs.as_slice() {
        ["workspace", "dependencies"] => Some((Section::Workspace, None, None)),
        ["workspace", "dependencies", key] => {
            Some((Section::Workspace, None, Some((*key).to_owned())))
        }
        [name] => Some((Section::of(name)?, None, None)),
        [name, key] => Some((Section::of(name)?, None, Some((*key).to_owned()))),
        ["target", platform, name] => {
            Some((Section::of(name)?, Some((*platform).to_owned()), None))
        }
        ["target", platform, name, key] => Some((
            Section::of(name)?,
            Some((*platform).to_owned()),
            Some((*key).to_owned()),
        )),
        _ => None,
    }
}

/// The text before a `#` that is outside a string.
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        match quote {
            Some('"') if escaped => escaped = false,
            Some('"') if ch == '\\' => escaped = true,
            Some(open) if ch == open => quote = None,
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == '#' => return &line[..index],
            Some(_) | None => {}
        }
    }
    line
}

/// The open `[` and `{` minus the closed `]` and `}`, outside strings.
fn bracket_balance(text: &str) -> i32 {
    let mut quote: Option<char> = None;
    let mut balance = 0;
    for ch in text.chars() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some(_) => {}
            None => match ch {
                '"' | '\'' => quote = Some(ch),
                '[' | '{' => balance += 1,
                ']' | '}' => balance -= 1,
                _ => {}
            },
        }
    }
    balance
}

/// The parts of a dotted key, without quotes and without the spaces around
/// the dots.
fn split_dotted(key: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in key.chars() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some(_) => current.push(ch),
            None => match ch {
                '"' | '\'' => quote = Some(ch),
                '.' => parts.push(std::mem::take(&mut current).trim().to_owned()),
                _ => current.push(ch),
            },
        }
    }
    parts.push(current.trim().to_owned());
    parts
}

/// The platform text without spaces, so that two spellings of one `cfg`
/// compare equal.
pub fn normalize_platform(platform: &str) -> String {
    platform.chars().filter(|ch| !ch.is_whitespace()).collect()
}

/// The one-based line of the first quoted `name` after the header line of
/// `table`, while the scanner stays in that table.
pub fn line_in_table(text: &str, table: &str, name: &str) -> Option<u32> {
    let quoted = [format!("\"{name}\""), format!("'{name}'")];
    let mut inside = false;
    for (index, raw) in text.lines().enumerate() {
        let code = strip_comment(raw).trim();
        if let Some(header) = code.strip_prefix('[') {
            inside = header
                .rfind(']')
                .is_some_and(|end| split_dotted(&header[..end]).join(".") == table);
            continue;
        }
        if inside && quoted.iter().any(|quoted| code.contains(quoted.as_str())) {
            return u32::try_from(index + 1).ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(text: &str) -> Vec<(Section, Option<String>, String, u32)> {
        entries(text)
            .into_iter()
            .map(|entry| (entry.section, entry.platform, entry.key, entry.line))
            .collect()
    }

    #[test]
    fn keys_of_each_table_form_are_found() {
        let text = "[package]\nname = \"a\"\n\n[dependencies]\nserde = \"1\"\n\
                    \"quoted\" = \"1\"\nfoo.workspace = true\nfoo.features = [\"x\"]\n\n\
                    [dev-dependencies.bar]\nversion = \"1\"\n\n\
                    [target.'cfg(windows)'.dependencies]\nwinapi = { version = \"0.3\" } # comment\n\n\
                    [target.\"x86_64-pc-windows-msvc\".build-dependencies.cc]\nversion = \"1\"\n\n\
                    [build_dependencies]\ncc = \"1\"\n";
        assert_eq!(
            keys(text),
            [
                (Section::Normal, None, "serde".to_owned(), 5),
                (Section::Normal, None, "quoted".to_owned(), 6),
                (Section::Normal, None, "foo".to_owned(), 7),
                (Section::Dev, None, "bar".to_owned(), 10),
                (
                    Section::Normal,
                    Some("cfg(windows)".to_owned()),
                    "winapi".to_owned(),
                    14
                ),
                (
                    Section::Build,
                    Some("x86_64-pc-windows-msvc".to_owned()),
                    "cc".to_owned(),
                    16
                ),
                (Section::Build, None, "cc".to_owned(), 20),
            ]
        );
    }

    #[test]
    fn lines_of_a_multi_line_array_are_not_keys() {
        let text = "[dependencies]\nfoo = { version = \"1\", features = [\n  \"a\",\n  \
                    \"b = c\",\n] }\nbar = \"1\"\n[[bin]]\nname = \"x\"\n";
        assert_eq!(
            keys(text),
            [
                (Section::Normal, None, "foo".to_owned(), 2),
                (Section::Normal, None, "bar".to_owned(), 6),
            ]
        );
    }

    #[test]
    fn each_inheritance_form_is_found() {
        let text = "[dependencies]\na = { workspace = true }\nb.workspace = true\n\
                    c = { version = \"1\", features = [\"workspace=true\"] }\n\
                    d = { features = [\"x\"], workspace = true }\n\
                    [dev-dependencies.e]\nworkspace = true\n[build-dependencies.f]\nversion = \"1\"\n";
        let inherits: Vec<(String, bool)> = entries(text)
            .into_iter()
            .map(|entry| (entry.key, entry.inherits))
            .collect();
        assert_eq!(
            inherits,
            [
                ("a".to_owned(), true),
                ("b".to_owned(), true),
                ("c".to_owned(), false),
                ("d".to_owned(), true),
                ("e".to_owned(), true),
                ("f".to_owned(), false),
            ]
        );
    }

    #[test]
    fn hash_in_a_string_is_not_a_comment() {
        let text = "[dependencies]\nfoo = { git = \"https://x/#frag\" }\n# bar = \"1\"\n";
        assert_eq!(keys(text), [(Section::Normal, None, "foo".to_owned(), 2)]);
    }

    #[test]
    fn workspace_table_and_other_tables() {
        let text = "[workspace]\nmembers = [\"a\"]\n[workspace.dependencies]\nserde = \"1\"\n\
                    [workspace.dependencies.toml]\nversion = \"1\"\n[features]\ndefault = []\n\
                    [workspace.metadata.dependencies]\nnot = \"a dep\"\n";
        assert_eq!(
            keys(text),
            [
                (Section::Workspace, None, "serde".to_owned(), 4),
                (Section::Workspace, None, "toml".to_owned(), 5),
            ]
        );
    }

    #[test]
    fn line_of_an_ignore_entry() {
        let text = "[package.metadata.fallow.deps]\nignored = [\n  \"a\",\n  \"b\",\n]\n\
                    [other]\nx = \"b\"\n";
        assert_eq!(
            line_in_table(text, "package.metadata.fallow.deps", "b"),
            Some(4)
        );
        assert_eq!(
            line_in_table(text, "package.metadata.fallow.deps", "c"),
            None
        );
    }

    #[test]
    fn platform_spelling_does_not_matter() {
        assert_eq!(
            normalize_platform("cfg(any(unix, windows))"),
            normalize_platform("cfg(any(unix,windows))")
        );
    }
}
