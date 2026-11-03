//! The crate names that the code of each package uses, from the module graph.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::graph::extract::{Origin, RefKind};
use crate::graph::{CrateKind, Graph, ModuleId};

/// The part of a package that uses a name. The labels are the names in the
/// `used_by` list of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Context {
    /// The code of a library, a proc-macro library or a binary, outside test
    /// code.
    Lib,
    /// See [`Context::Lib`].
    ProcMacro,
    /// See [`Context::Lib`].
    Bin,
    /// `#[cfg(test)]` code and `#[test]` functions of a library or a binary.
    UnitTest,
    /// A code block in the documentation of a library.
    Doctest,
    /// An integration test.
    Test,
    /// A bench.
    Bench,
    /// An example.
    Example,
    /// The build script.
    Build,
}

impl Context {
    /// The label in the report.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lib => "lib",
            Self::ProcMacro => "proc-macro",
            Self::Bin => "bin",
            Self::UnitTest => "unit-test",
            Self::Doctest => "doctest",
            Self::Test => "test",
            Self::Bench => "bench",
            Self::Example => "example",
            Self::Build => "build-script",
        }
    }

    /// True for the code that `[dependencies]` serves and `[dev-dependencies]`
    /// does not.
    pub const fn is_production(self) -> bool {
        matches!(self, Self::Lib | Self::ProcMacro | Self::Bin)
    }

    /// True for the code that `[dev-dependencies]` serves.
    pub const fn is_test(self) -> bool {
        matches!(
            self,
            Self::UnitTest | Self::Doctest | Self::Test | Self::Bench | Self::Example
        )
    }
}

/// The names that the code of one package uses.
#[derive(Debug, Clone, Default)]
pub struct PackageUsage {
    /// The first segment of each path and import, with the contexts that use it.
    pub names: BTreeMap<String, BTreeSet<Context>>,
    /// The contexts with an `include!` call. Code that the graph cannot see can
    /// use the names in [`PackageUsage::generated`] there, or any name when the
    /// package has no build script.
    pub included: BTreeSet<Context>,
    /// The names that the code of the build script can write: each `name::`
    /// in the text of its files, also in strings. `None` when the package has
    /// no build script.
    pub generated: Option<BTreeSet<String>>,
}

impl PackageUsage {
    /// The contexts that use `name`, with each `include!` context that can use it.
    pub fn contexts(&self, name: &str) -> BTreeSet<Context> {
        let mut contexts = self.names.get(name).cloned().unwrap_or_default();
        let generated = self
            .generated
            .as_ref()
            .is_none_or(|names| names.contains(name));
        if generated {
            contexts.extend(self.included.iter().copied());
        }
        contexts
    }
}

/// The usage of each package, keyed by package name. `root` is the
/// workspace root, for the files of the build scripts.
pub fn collect(graph: &Graph, root: &Path) -> BTreeMap<String, PackageUsage> {
    let mut usage: BTreeMap<String, PackageUsage> = BTreeMap::new();
    for krate in &graph.crates {
        usage.entry(krate.package.clone()).or_default();
    }
    for (id, krate) in graph.crates.iter().enumerate() {
        if krate.kind != CrateKind::Build {
            continue;
        }
        let files: BTreeSet<&str> = graph
            .items
            .iter()
            .filter(|item| item.krate == id)
            .map(|item| item.file.as_str())
            .collect();
        let names = usage
            .entry(krate.package.clone())
            .or_default()
            .generated
            .get_or_insert_default();
        for file in files {
            if let Ok(text) = fs::read_to_string(root.join(file)) {
                names.extend(path_prefixes(&text));
            }
        }
    }
    for import in &graph.imports {
        let krate = &graph.crates[import.krate];
        let context = context_of(krate.kind, is_test_module(graph, import.module), false);
        if let Some(first) = import.path.first() {
            add(&mut usage, &krate.package, first, context);
        }
    }
    for reference in &graph.refs {
        let krate = &graph.crates[reference.krate];
        let test = reference.test_only || is_test_module(graph, reference.module);
        let context = context_of(krate.kind, test, reference.origin == Origin::Doc);
        match &reference.kind {
            RefKind::Path { segments, .. } | RefKind::RecordField { segments, .. } => {
                if let Some(first) = segments.first() {
                    add(&mut usage, &krate.package, first, context);
                }
            }
            RefKind::Macro(name) if name == "include" => {
                usage
                    .entry(krate.package.clone())
                    .or_default()
                    .included
                    .insert(context);
            }
            RefKind::Macro(_) | RefKind::Method(_) | RefKind::Field(_) | RefKind::Member(_) => {}
        }
    }
    usage
}

/// Each identifier that `::` follows in the text. A lexical scan, so it also
/// sees the paths in strings and comments. Extra names only give more credit.
fn path_prefixes(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let bytes = text.as_bytes();
    let mut start: Option<usize> = None;
    for (index, byte) in bytes.iter().enumerate() {
        if byte.is_ascii_alphanumeric() || *byte == b'_' {
            start.get_or_insert(index);
            continue;
        }
        if let Some(begin) = start.take()
            && text[index..].starts_with("::")
            && !bytes[begin].is_ascii_digit()
        {
            names.insert(text[begin..index].to_owned());
        }
    }
    names
}

fn add(usage: &mut BTreeMap<String, PackageUsage>, package: &str, name: &str, context: Context) {
    let name = name.strip_prefix("r#").unwrap_or(name);
    if matches!(name, "crate" | "self" | "super" | "Self" | "$crate") {
        return;
    }
    usage
        .entry(package.to_owned())
        .or_default()
        .names
        .entry(name.to_owned())
        .or_default()
        .insert(context);
}

const fn context_of(kind: CrateKind, test: bool, doc: bool) -> Context {
    match kind {
        CrateKind::Build => Context::Build,
        CrateKind::Test => Context::Test,
        CrateKind::Bench => Context::Bench,
        CrateKind::Example => Context::Example,
        _ if doc => Context::Doctest,
        _ if test => Context::UnitTest,
        CrateKind::Lib => Context::Lib,
        CrateKind::ProcMacro => Context::ProcMacro,
        CrateKind::Bin => Context::Bin,
    }
}

/// True when the module or a parent module exists only in test builds.
fn is_test_module(graph: &Graph, module: ModuleId) -> bool {
    let mut current = Some(module);
    while let Some(id) = current {
        if graph.modules[id]
            .item
            .is_some_and(|item| graph.items[item].test_only)
        {
            return true;
        }
        current = graph.modules[id].parent;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_prefixes_are_found_in_code_and_strings() {
        let found =
            path_prefixes("use std::fs;\nlet s = \"pub fn g() { alpha::f(); ::beta::X }\"; 1::x");
        let found: Vec<&str> = found.iter().map(String::as_str).collect();
        assert_eq!(found, ["alpha", "beta", "std"]);
    }
}
