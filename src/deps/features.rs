//! The `unused-feature` rule: a Cargo feature that turns on nothing and that
//! nothing names.
//!
//! The rule judges only the features that no user outside the workspace can
//! turn on: the features of a package that a registry cannot publish
//! (`publish = false`), and that `exclude-crates` of
//! `[workspace.metadata.fallow.visibility]` does not name. A feature with a
//! value, for example `["dep:serde"]` or `["log/std"]`, has an effect, so it
//! is not a finding. A feature gets credit when one of these names it:
//!
//! - `default`, or another feature of the package,
//! - a dependency entry of a member, in `features = [..]`, or a feature of a
//!   member as `key/feature` or `key?/feature`,
//! - `required-features` of a target,
//! - the name as a whole word in any file under the repository root, also in
//!   a comment or a string: `cfg`, a build script, a CI workflow, a script, a
//!   manifest outside the workspace. The `[features]` tables of the members
//!   and the ignore lists of the manifests do not count,
//! - `CARGO_FEATURE_<NAME>` in a file. When a file reads `CARGO_FEATURE` with
//!   no name, no package with a build script gets a finding.
//!
//! The scan is lexical, so an extra word only gives more credit. When the scan
//! cannot read a file or a folder, the rule reports nothing. The scan runs
//! only when a candidate exists.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use cargo_metadata::{Metadata, Package, TargetKind};

/// The start of the variable that Cargo sets for each feature in the
/// environment of a build script.
const FEATURE_VARIABLE: &str = "CARGO_FEATURE";
/// The folder names that hold no source of the repository. Cargo target
/// folders are found by their `CACHEDIR.TAG` file, not by their name.
const SKIPPED_FOLDERS: [&str; 2] = [".git", "node_modules"];
/// The file that Cargo writes in each target folder.
const CACHE_TAG: &str = "CACHEDIR.TAG";
/// The size of one read of a file, in bytes.
const CHUNK_BYTES: usize = 1024 * 1024;
/// The tables of a manifest whose words give no credit: the ignore lists,
/// which a finding names.
const SKIPPED_TABLES: [&str; 6] = [
    "package.metadata.fallow.deps",
    "workspace.metadata.fallow.deps",
    "package.metadata.cargo-shear",
    "workspace.metadata.cargo-shear",
    "package.metadata.cargo-machete",
    "workspace.metadata.cargo-machete",
];
/// The table that defines the features. In a member manifest, its words give
/// no credit.
const FEATURES_TABLE: &str = "features";

/// The facts of the workspace that the rule reads once.
#[derive(Debug, Default)]
pub struct FeatureRule {
    /// The candidate features of each package, keyed by package id.
    candidates: BTreeMap<String, Vec<String>>,
    /// The candidate names that a file names.
    credited: BTreeSet<String>,
    /// True when a file reads `CARGO_FEATURE` with no name.
    generic_read: bool,
    /// True when the scan could not read a file or a folder.
    incomplete: bool,
}

impl FeatureRule {
    /// Finds the candidates and scans the repository for their names. An
    /// invalid visibility table gives no candidate.
    pub fn new(workspace: &crate::workspace::Workspace, metadata: &Metadata) -> Self {
        let Some((exclude, public)) = crate::visibility::config::api_crates(workspace) else {
            return Self::default();
        };
        let excluded = Excluded { exclude, public };
        let candidates: BTreeMap<String, Vec<String>> = metadata
            .workspace_packages()
            .into_iter()
            .map(|package| {
                (
                    package.id.repr.clone(),
                    candidates_of(metadata, package, &excluded),
                )
            })
            .filter(|(_, names)| !names.is_empty())
            .collect();
        if candidates.is_empty() {
            return Self::default();
        }
        let names: BTreeSet<&str> = candidates.values().flatten().map(String::as_str).collect();
        let members: BTreeSet<PathBuf> = metadata
            .workspace_packages()
            .iter()
            .map(|package| real_path(package.manifest_path.as_std_path()))
            .collect();
        let root = repository_root(metadata.workspace_root.as_std_path());
        let mut roots = vec![real_path(&root)];
        roots.extend(
            metadata
                .workspace_packages()
                .iter()
                .filter_map(|package| package.manifest_path.parent())
                .map(|folder| real_path(folder.as_std_path())),
        );
        let mut scan = Scan::new(&names, &members, &roots);
        for folder in roots.clone() {
            scan.folder(&folder);
        }
        Self {
            credited: scan.credited,
            generic_read: scan.generic_read,
            incomplete: scan.incomplete,
            candidates,
        }
    }

    /// The features of `package` that the rule reports, with the line of each
    /// key in `[features]`.
    pub fn unused(&self, package: &Package, manifest: &str) -> Vec<(String, Option<u32>)> {
        if self.incomplete || (self.generic_read && has_build_script(package)) {
            return Vec::new();
        }
        self.candidates
            .get(&package.id.repr)
            .into_iter()
            .flatten()
            .filter(|name| !self.credited.contains(*name))
            .map(|name| (name.clone(), feature_line(manifest, name)))
            .collect()
    }
}

/// The features of `package` that only the scan can still credit: an empty
/// list, not `default`, and no feature, member or target names it.
fn candidates_of(metadata: &Metadata, package: &Package, excluded: &Excluded) -> Vec<String> {
    // Users outside the workspace can turn on the features of a published
    // package, with a dependency entry or with `cargo install --features`.
    if crate::workspace::is_published(package) || is_excluded(package, excluded) {
        return Vec::new();
    }
    let enabled = enabled_by_members(metadata, package);
    let required: BTreeSet<&str> = package
        .targets
        .iter()
        .flat_map(|target| target.required_features.iter().map(String::as_str))
        .collect();
    package
        .features
        .iter()
        .filter(|(name, values)| {
            values.is_empty()
                && name.as_str() != "default"
                && !enabled.contains(name.as_str())
                && !required.contains(name.as_str())
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// The root of the git work tree that holds the workspace, or the workspace
/// root when there is none.
fn repository_root(workspace_root: &Path) -> PathBuf {
    workspace_root
        .ancestors()
        .find(|folder| folder.join(".git").exists())
        .unwrap_or(workspace_root)
        .to_path_buf()
}

/// The path with each symlink resolved, or the path itself.
fn real_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The scan of the files for the candidate names.
struct Scan<'a> {
    names: &'a BTreeSet<&'a str>,
    /// The real paths of the member manifests.
    members: &'a BTreeSet<PathBuf>,
    /// The real folders that the scan can enter. A symlink to a folder
    /// outside them is not followed.
    roots: &'a [PathBuf],
    /// The variable name of each candidate, for `CARGO_FEATURE_<NAME>`.
    variables: BTreeMap<String, &'a str>,
    /// The real folders that the scan read.
    seen: BTreeSet<PathBuf>,
    credited: BTreeSet<String>,
    generic_read: bool,
    incomplete: bool,
}

impl<'a> Scan<'a> {
    fn new(
        names: &'a BTreeSet<&'a str>,
        members: &'a BTreeSet<PathBuf>,
        roots: &'a [PathBuf],
    ) -> Self {
        Self {
            names,
            members,
            roots,
            variables: names
                .iter()
                .map(|name| (variable_of(name), *name))
                .collect(),
            seen: BTreeSet::new(),
            credited: BTreeSet::new(),
            generic_read: false,
            incomplete: false,
        }
    }

    /// Scans each file under `folder`. A folder counts once. A Cargo target
    /// folder and a folder outside the roots are not read.
    fn folder(&mut self, folder: &Path) {
        if self.done() {
            return;
        }
        let real = real_path(folder);
        if !self.inside_roots(&real) || !self.seen.insert(real) {
            return;
        }
        if folder.join(CACHE_TAG).is_file() {
            return;
        }
        let Ok(entries) = fs::read_dir(folder) else {
            self.incomplete = true;
            return;
        };
        for entry in entries {
            match entry {
                Ok(entry) => self.scan_entry(&entry.path()),
                Err(_) => self.incomplete = true,
            }
        }
    }

    /// Scans one entry of a folder: a subfolder or a regular file.
    fn scan_entry(&mut self, path: &Path) {
        let Ok(meta) = fs::metadata(path) else {
            // A broken symlink holds no text.
            if fs::symlink_metadata(path).is_err() {
                self.incomplete = true;
            }
            return;
        };
        if meta.is_dir() {
            let skipped = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| SKIPPED_FOLDERS.contains(&name));
            if !skipped {
                self.folder(path);
            }
        } else if meta.is_file()
            && path.file_name().is_some_and(|name| name != "Cargo.lock")
            && self.inside_roots(path)
        {
            // A FIFO, a socket or a device holds no source, and a read of
            // it can block or never end.
            self.file(path);
        }
    }

    /// True when the real path of `path` is under one of the roots.
    fn inside_roots(&self, path: &Path) -> bool {
        let real = real_path(path);
        self.roots.iter().any(|root| real.starts_with(root))
    }

    /// True when each candidate has credit, so that no file can change the
    /// result.
    fn done(&self) -> bool {
        self.credited.len() == self.names.len()
    }

    /// Scans one file. A manifest is read whole, so that its tables can be
    /// left out. Any other file is read in chunks.
    fn file(&mut self, path: &Path) {
        if path.file_name().is_some_and(|name| name == "Cargo.toml") {
            let Ok(bytes) = fs::read(path) else {
                self.incomplete = true;
                return;
            };
            let member = self.members.contains(&real_path(path));
            self.text(&without_skipped_tables(
                &String::from_utf8_lossy(&bytes),
                member,
            ));
            return;
        }
        let Ok(mut file) = fs::File::open(path) else {
            self.incomplete = true;
            return;
        };
        let mut buffer: Vec<u8> = Vec::new();
        let mut chunk = vec![0_u8; CHUNK_BYTES];
        while !self.done() {
            let Ok(read) = file.read(&mut chunk) else {
                self.incomplete = true;
                return;
            };
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            // Cut after the last separator, so that no word spans two parts.
            // A big part without a separator is scanned whole.
            let cut = if buffer.len() > 2 * CHUNK_BYTES {
                buffer.len()
            } else {
                buffer
                    .iter()
                    .rposition(|byte| !is_word_byte(*byte))
                    .map_or(0, |at| at + 1)
            };
            let rest = buffer.split_off(cut);
            self.bytes(&buffer);
            buffer = rest;
        }
        self.bytes(&buffer);
    }

    /// Scans raw bytes. A text with NUL bytes, for example UTF-16, is also
    /// scanned without them.
    fn bytes(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.text(&String::from_utf8_lossy(bytes));
        if bytes.contains(&0) {
            let kept: Vec<u8> = bytes.iter().copied().filter(|byte| *byte != 0).collect();
            self.text(&String::from_utf8_lossy(&kept));
        }
    }

    /// Credits each candidate that the text names as a word or as a variable.
    fn text(&mut self, text: &str) {
        // A whole-word match needs a substring match, which is much faster.
        let open: BTreeSet<&str> = self
            .names
            .iter()
            .copied()
            .filter(|name| !self.credited.contains(*name) && text.contains(name))
            .collect();
        if !open.is_empty() {
            for word in words(text) {
                if open.contains(word) {
                    self.credited.insert(word.to_owned());
                }
            }
        }
        for (at, _) in text.match_indices(FEATURE_VARIABLE) {
            self.variable(&text[at + FEATURE_VARIABLE.len()..]);
        }
    }

    /// Credits the feature of one `CARGO_FEATURE_` variable. The text starts
    /// after the prefix. A read without a name is a generic read.
    fn variable(&mut self, rest: &str) {
        let Some(rest) = rest.strip_prefix('_') else {
            self.generic_read = true;
            return;
        };
        let name: String = rest
            .chars()
            .take_while(|ch| {
                !ch.is_lowercase() && (ch.is_alphanumeric() || matches!(ch, '_' | '.' | '+'))
            })
            .collect();
        if name.is_empty() {
            self.generic_read = true;
            return;
        }
        // A `.` or `+` at the end can end a sentence, not the name.
        let mut name = name.as_str();
        loop {
            if let Some(feature) = self.variables.get(name) {
                self.credited.insert((*feature).to_owned());
                break;
            }
            match name.strip_suffix(['.', '+']) {
                Some(shorter) if !shorter.is_empty() => name = shorter,
                _ => break,
            }
        }
    }
}

/// True for a byte that can be part of a word of [`words`]. A byte of a
/// multi-byte character counts as a word byte.
fn is_word_byte(byte: u8) -> bool {
    !byte.is_ascii() || byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+')
}

/// Each word of a text, and the parts of a word with `.` or `+`. A word is a
/// run of letters, digits, `_`, `-`, `.` and `+`, as in a feature name. A
/// `-F` before a word, as in `-Ffast`, does not count.
fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|ch: char| !(ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '+')))
        .filter(|word| !word.is_empty())
        .flat_map(|word| {
            let short = word.strip_prefix("-F").filter(|rest| !rest.is_empty());
            std::iter::once(word)
                .chain(short)
                .chain(word.split(['.', '+']).filter(|part| !part.is_empty()))
                .chain(word.split('.').map(|part| part.trim_matches('-')))
        })
}

/// The text of a manifest without the bodies of [`SKIPPED_TABLES`], and of a
/// member manifest also without the body of `[features]`.
fn without_skipped_tables(manifest: &str, member: bool) -> String {
    let mut kept = String::new();
    let mut skipping = false;
    for line in manifest.lines() {
        if let Some(table) = header_name(line) {
            skipping = SKIPPED_TABLES.contains(&table) || (member && table == FEATURES_TABLE);
        }
        if !skipping {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    kept
}

/// The name of the table of a header line, for example `features` for
/// `[features] # note`.
fn header_name(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('[')?;
    let rest = rest.strip_prefix('[').unwrap_or(rest);
    rest.find(']').map(|end| rest[..end].trim())
}

/// The name of the variable of a feature: upper case, with `_` for `-`.
fn variable_of(feature: &str) -> String {
    feature.to_ascii_uppercase().replace('-', "_")
}

fn is_library(target: &cargo_metadata::Target) -> bool {
    target.kind.iter().any(|kind| {
        matches!(
            kind,
            TargetKind::Lib
                | TargetKind::RLib
                | TargetKind::DyLib
                | TargetKind::CDyLib
                | TargetKind::StaticLib
                | TargetKind::ProcMacro
        )
    })
}

/// True when `exclude-crates` names the library of the package.
/// The libraries that the visibility table makes an external API.
struct Excluded {
    /// The `exclude-crates` crate names.
    exclude: Vec<String>,
    /// The `public-crates` patterns.
    public: Vec<String>,
}

/// True when the visibility table makes a library of the package an external
/// API: `exclude-crates` or `public-crates` names it.
fn is_excluded(package: &Package, excluded: &Excluded) -> bool {
    package.targets.iter().any(|target| {
        let name = crate::graph::crate_name(&target.name);
        is_library(target)
            && (excluded.exclude.contains(&name)
                || excluded.public.iter().any(|pattern| {
                    crate::visibility::name_matches(pattern, &name)
                        || crate::visibility::name_matches(pattern, &package.name)
                }))
    })
}

fn has_build_script(package: &Package) -> bool {
    package
        .targets
        .iter()
        .any(|target| target.kind.contains(&TargetKind::CustomBuild))
}

/// The features of `package` that a feature of the package, `default`, a
/// dependency entry of a member or a feature of a member turns on.
fn enabled_by_members<'a>(metadata: &'a Metadata, package: &Package) -> BTreeSet<&'a str> {
    let mut enabled = BTreeSet::new();
    for member in metadata.workspace_packages() {
        let own = member.id == package.id;
        for dependency in &member.dependencies {
            if dependency.name == package.name.as_str() {
                enabled.extend(dependency.features.iter().map(String::as_str));
            }
        }
        enabled.extend(
            member
                .features
                .values()
                .flatten()
                .filter_map(|value| enabled_by_value(member, package, own, value)),
        );
    }
    enabled
}

/// The feature of `package` that one feature value of `member` turns on.
/// `own` is true when `member` is `package`.
fn enabled_by_value<'a>(
    member: &Package,
    package: &Package,
    own: bool,
    value: &'a str,
) -> Option<&'a str> {
    if own && !value.contains(['/', ':']) {
        return Some(value);
    }
    let (key, feature) = value.split_once('/')?;
    let key = key.trim_end_matches('?');
    let names_package = member.dependencies.iter().any(|dependency| {
        dependency.name == package.name.as_str()
            && dependency.rename.as_deref().unwrap_or(&dependency.name) == key
    });
    names_package.then_some(feature)
}

/// The one-based line of the key `name` in the `[features]` table.
pub fn feature_line(manifest: &str, name: &str) -> Option<u32> {
    let document = toml_edit::Document::parse(manifest).ok()?;
    let table = document.get(FEATURES_TABLE)?.as_table_like()?;
    let (key, _) = table.get_key_value(name)?;
    let start = key.span()?.start;
    let newlines = manifest.get(..start)?.matches('\n').count();
    u32::try_from(newlines + 1).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(text: &str, names: &[&str]) -> (BTreeSet<String>, bool) {
        let names: BTreeSet<&str> = names.iter().copied().collect();
        let members = BTreeSet::new();
        let mut scan = Scan::new(&names, &members, &[]);
        scan.text(text);
        (scan.credited, scan.generic_read)
    }

    #[test]
    fn a_name_counts_in_each_spelling() {
        let text = "#[cfg(feature = \"a\")] writeln!(f, \"#[cfg(feature = \\\"b\\\")]\")\n\
                    cargo build --features c,d -Fe FEATURES ?= f\nrun: x --features=g.\n\
                    #[cfg(feature = r\"h\")]";
        let (credited, generic) = found(text, &["a", "b", "c", "d", "e", "f", "g", "h", "unused"]);
        assert_eq!(credited.len(), 8, "{credited:?}");
        assert!(!credited.contains("unused"));
        assert!(!generic);
    }

    #[test]
    fn a_name_inside_a_longer_word_does_not_count() {
        let (credited, _) = found("my_fast_path fasting", &["fast"]);
        assert!(credited.is_empty(), "{credited:?}");
    }

    #[test]
    fn variables_credit_their_feature_and_a_read_of_each_feature_is_found() {
        let (credited, generic) = found("env::var(\"CARGO_FEATURE_SERDE_JSON\")", &["serde-json"]);
        assert!(credited.contains("serde-json"));
        assert!(!generic);
        let (_, generic) = found("k.starts_with(\"CARGO_FEATURE_\")", &["x"]);
        assert!(generic);
        let (_, generic) = found("k.starts_with(\"CARGO_FEATURE\")", &["x"]);
        assert!(generic);
        let (credited, generic) = found(
            "CARGO_FEATURE_FOO.BAR CARGO_FEATURE_END.",
            &["foo.bar", "end"],
        );
        assert_eq!(credited.len(), 2, "{credited:?}");
        assert!(!generic);
    }

    #[test]
    fn the_features_table_and_the_ignore_lists_of_a_manifest_give_no_credit() {
        let manifest = "[package]\nname = \"a\"\n\n[features]\nfast = []\n\n[package.metadata.fallow.deps]\nignored = [\"fast\"]\n\n[dependencies]\nb = { path = \"b\", features = [\"slow\"] }\n";
        let kept = without_skipped_tables(manifest, true);
        assert!(!kept.contains("fast"), "{kept}");
        assert!(kept.contains("slow"), "{kept}");
        let outside = without_skipped_tables(manifest, false);
        assert_eq!(outside.matches("fast").count(), 1, "{outside}");
    }

    #[test]
    fn the_line_of_a_feature_key_is_found_only_in_its_table() {
        let manifest = "[package]\nname = \"a\"\n\n[features] # the features\ndefault = []\n\"quoted\" = []\nplain = []\n\n[dependencies]\nplain = \"1\"\n";
        assert_eq!(feature_line(manifest, "plain"), Some(7));
        assert_eq!(feature_line(manifest, "quoted"), Some(6));
        assert_eq!(feature_line(manifest, "missing"), None);
        let tricky = "[package]\nname = \"a\"\n\n[features] # \"the\" features\nlist = [\n  \"plain=x\",\n]\nplain = []\n";
        assert_eq!(feature_line(tricky, "plain"), Some(8));
        assert_eq!(
            feature_line(&tricky.replace('\n', "\r\n"), "plain"),
            Some(8)
        );
    }
}
