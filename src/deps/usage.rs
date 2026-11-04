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
    /// The contexts with an `include!` call that the graph cannot read. Its code
    /// can use the names in [`PackageUsage::generated`], or any name when the
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
        // The output of a derive or macro crate names its runtime crate, for
        // example `serde_derive` output names `serde`.
        for macro_crate in macro_crates(name) {
            if let Some(used) = self.names.get(&macro_crate) {
                contexts.extend(used.iter().copied());
            }
        }
        if self.can_generate(name) {
            contexts.extend(self.included.iter().copied());
        }
        contexts
    }

    /// True when the code of an `include!` call that the graph cannot read
    /// can name `name`.
    pub fn can_generate(&self, name: &str) -> bool {
        self.generated
            .as_ref()
            .is_none_or(|names| names.contains(name))
    }
}

/// The macro crates whose output names the crate `name`, for example
/// `serde_derive` for `serde`.
pub fn macro_crates(name: &str) -> impl Iterator<Item = String> + '_ {
    MACRO_SUFFIXES
        .iter()
        .map(move |suffix| format!("{name}{suffix}"))
        .chain(
            MACRO_PAIRS
                .iter()
                .filter(move |(_, runtime)| *runtime == name)
                .map(|(macro_crate, _)| (*macro_crate).to_owned()),
        )
}

/// The suffixes of a macro crate whose output uses the crate without the suffix,
/// for example `tracing_attributes` and `tracing`.
const MACRO_SUFFIXES: [&str; 7] = [
    "_derive",
    "_derives",
    "_macro",
    "_macros",
    "_impl",
    "_internal",
    "_attributes",
];

/// Macro crates whose output uses a runtime crate with a different name, as
/// `(macro crate, runtime crate)`. The documentation of each macro crate
/// tells the user to add the runtime crate. A pair that is not here needs an
/// entry in the `ignored` list.
const MACRO_PAIRS: [(&str, &str); 2] = [("num_derive", "num_traits"), ("serde_repr", "serde")];

/// One place in the code of a package that names a crate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Site {
    /// The file, relative to the workspace root.
    pub file: String,
    /// The one-based line, or `None` when the place is not known.
    pub line: Option<u32>,
    /// The part of the package.
    pub context: Context,
    /// How the code names the crate: `import`, `path`, `macro-tokens`,
    /// `attribute`, `doc-code`, or `include` for an `include!` call that the
    /// graph cannot read.
    pub via: &'static str,
    /// The crate name in the code.
    pub name: String,
}

/// The places in the code of `package` whose first path segment is one of
/// `names`. With `include` true, each `include!` call that the graph cannot
/// read is a place too, because its code can name each crate.
pub fn sites(graph: &Graph, package: &str, names: &[String], include: bool) -> Vec<Site> {
    let path_of = |file: crate::graph::FileId| {
        usize::try_from(file)
            .ok()
            .and_then(|file| graph.paths.get(file))
            .cloned()
            .unwrap_or_default()
    };
    let wanted = |name: &str| {
        let name = name.strip_prefix("r#").unwrap_or(name);
        names.iter().find(|wanted| *wanted == name).cloned()
    };
    let mut found = Vec::new();
    for import in &graph.imports {
        let krate = &graph.crates[import.krate];
        if krate.package != package {
            continue;
        }
        let Some(context) = context_of(krate.kind, is_test_module(graph, import.module), false)
        else {
            continue;
        };
        if let Some(name) = import.path.first().and_then(|first| wanted(first)) {
            found.push(Site {
                file: path_of(import.file),
                line: (import.line > 0).then_some(import.line),
                context,
                via: "import",
                name,
            });
        }
    }
    for reference in &graph.refs {
        let krate = &graph.crates[reference.krate];
        if krate.package != package {
            continue;
        }
        let test = reference.test_only || is_test_module(graph, reference.module);
        let Some(context) = context_of(krate.kind, test, reference.origin == Origin::Doc) else {
            continue;
        };
        let (name, via) = match &reference.kind {
            RefKind::Path { segments, .. } | RefKind::RecordField { segments, .. } => {
                let Some(name) = segments.first().and_then(|first| wanted(first)) else {
                    continue;
                };
                let via = match reference.origin {
                    Origin::Body | Origin::Signature => "path",
                    Origin::Macro => "macro-tokens",
                    Origin::Attr => "attribute",
                    Origin::Doc => "doc-code",
                    Origin::DocLink => "doc-link",
                };
                (name, via)
            }
            RefKind::Macro(name) if include && name == "include" => (name.clone(), "include"),
            _ => continue,
        };
        found.push(Site {
            file: path_of(reference.file),
            line: (reference.line > 0).then_some(reference.line),
            context,
            via,
            name,
        });
    }
    found.sort();
    found.dedup();
    found
}

/// The usage of each package, keyed by package name. `root` is the
/// workspace root, for the files of the build scripts.
pub fn collect(graph: &Graph, root: &Path) -> BTreeMap<String, PackageUsage> {
    let mut usage: BTreeMap<String, PackageUsage> = BTreeMap::new();
    for krate in &graph.crates {
        usage.entry(krate.package.clone()).or_default();
    }
    for (package, names) in build_script_names(graph, root) {
        usage.entry(package).or_default().generated = Some(names);
    }
    for import in &graph.imports {
        let krate = &graph.crates[import.krate];
        let Some(context) = context_of(krate.kind, is_test_module(graph, import.module), false)
        else {
            continue;
        };
        if let Some(first) = import.path.first() {
            add(&mut usage, &krate.package, first, context);
        }
    }
    for reference in &graph.refs {
        let krate = &graph.crates[reference.krate];
        let test = reference.test_only || is_test_module(graph, reference.module);
        let Some(context) = context_of(krate.kind, test, reference.origin == Origin::Doc) else {
            continue;
        };
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

/// The names that the build script of each package can write into generated
/// code: each `name::` in the text of its files, also in strings. Keyed by
/// package name. A package without a build script has no entry.
pub fn build_script_names(graph: &Graph, root: &Path) -> BTreeMap<String, BTreeSet<String>> {
    let mut names: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
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
        let package = names.entry(krate.package.clone()).or_default();
        for file in files {
            if let Ok(text) = fs::read_to_string(root.join(file)) {
                package.extend(path_prefixes(&text));
            }
        }
    }
    names
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

/// The context of code in a crate of `kind`. `doc` marks a code block in the
/// documentation. rustdoc tests only the code blocks of a library, so the
/// documentation of another crate kind has no context.
const fn context_of(kind: CrateKind, test: bool, doc: bool) -> Option<Context> {
    let context = match kind {
        CrateKind::Lib | CrateKind::ProcMacro if doc => Context::Doctest,
        _ if doc => return None,
        CrateKind::Build => Context::Build,
        CrateKind::Test => Context::Test,
        CrateKind::Bench => Context::Bench,
        CrateKind::Example => Context::Example,
        _ if test => Context::UnitTest,
        CrateKind::Lib => Context::Lib,
        CrateKind::ProcMacro => Context::ProcMacro,
        CrateKind::Bin => Context::Bin,
    };
    Some(context)
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

    fn usage_of(names: &[(&str, Context)]) -> PackageUsage {
        let mut usage = PackageUsage::default();
        for (name, context) in names {
            usage
                .names
                .entry((*name).to_owned())
                .or_default()
                .insert(*context);
        }
        usage
    }

    #[test]
    fn macro_crate_suffixes_give_credit_to_the_runtime_crate() {
        for suffix in [
            "_derive",
            "_derives",
            "_macro",
            "_macros",
            "_impl",
            "_internal",
            "_attributes",
        ] {
            let usage = usage_of(&[(&format!("alpha{suffix}"), Context::Lib)]);
            assert_eq!(
                usage.contexts("alpha"),
                BTreeSet::from([Context::Lib]),
                "{suffix}"
            );
        }
    }

    #[test]
    fn known_macro_crate_pairs_give_credit_to_the_runtime_crate() {
        let usage = usage_of(&[("num_derive", Context::Lib), ("serde_repr", Context::Test)]);
        assert_eq!(usage.contexts("num_traits"), BTreeSet::from([Context::Lib]));
        assert_eq!(usage.contexts("serde"), BTreeSet::from([Context::Test]));
        assert!(usage.contexts("alpha").is_empty());
    }

    #[test]
    fn documentation_of_a_crate_that_rustdoc_does_not_test_gives_no_credit() {
        for kind in [
            CrateKind::Bin,
            CrateKind::Test,
            CrateKind::Bench,
            CrateKind::Example,
            CrateKind::Build,
        ] {
            assert_eq!(context_of(kind, false, true), None, "{kind:?}");
        }
        assert_eq!(
            context_of(CrateKind::Lib, false, true),
            Some(Context::Doctest)
        );
        assert_eq!(
            context_of(CrateKind::Bin, true, false),
            Some(Context::UnitTest)
        );
    }
}
