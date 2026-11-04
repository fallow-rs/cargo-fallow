//! The workspace module graph.
//!
//! The graph holds one crate for each target of each workspace member: the
//! library, the binaries, the tests, the benches, the examples and the build
//! script. Each crate has its module tree from `mod` declarations, its items,
//! its imports and its references. [`resolve`] connects the references to the
//! items.
//!
//! The graph is syntactic. It reads every `cfg` branch, and it does not expand
//! macros. A name in macro tokens counts as a reference to each item with that
//! name, so a gap in the graph hides a finding and does not make one.

pub mod extract;
pub mod resolve;
pub mod tokens;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::thread;

use crate::gates::parse_edition;
use crate::graph::extract::{
    Binding, CodeInclude, FileFacts, ItemFlags, ItemKind, Origin, RawRef, RefKind, Vis,
    doc_refs_of_markdown, extract,
};
use crate::modules::{SkippedFile, TargetRoot, normalize, walk};
use crate::parse::Edition;
use crate::structure::source::{has_generated_marker, is_generated};
use crate::workspace::{LinkKind, Member, Target, Workspace};

/// An index into [`Graph::crates`].
pub type CrateId = usize;
/// An index into [`Graph::modules`].
pub type ModuleId = usize;
/// An index into [`Graph::items`].
pub type ItemId = usize;
/// An index into [`Graph::paths`].
pub type FileId = u32;

/// The kind of a crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CrateKind {
    /// A library.
    Lib,
    /// A proc-macro library.
    ProcMacro,
    /// A binary.
    Bin,
    /// An integration test.
    Test,
    /// A bench.
    Bench,
    /// An example.
    Example,
    /// A build script.
    Build,
}

impl CrateKind {
    /// The name in the report.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lib => "lib",
            Self::ProcMacro => "proc-macro",
            Self::Bin => "bin",
            Self::Test => "test",
            Self::Bench => "bench",
            Self::Example => "example",
            Self::Build => "build-script",
        }
    }

    fn of(target: &Target) -> Self {
        let has = |kind: &str| target.kind.iter().any(|value| value == kind);
        if has("proc-macro") {
            Self::ProcMacro
        } else if has("bin") {
            Self::Bin
        } else if has("test") {
            Self::Test
        } else if has("bench") {
            Self::Bench
        } else if has("example") {
            Self::Example
        } else if has("custom-build") {
            Self::Build
        } else {
            Self::Lib
        }
    }

    /// True for a crate that other crates of the workspace can depend on.
    pub const fn is_library(self) -> bool {
        matches!(self, Self::Lib | Self::ProcMacro)
    }
}

/// The selection of the production roots.
#[derive(Debug, Clone, Default)]
pub struct Production {
    /// The binaries, as `(package, bin)`. Empty means every binary.
    pub bins: Vec<(String, String)>,
}

/// One crate of the workspace.
#[derive(Debug, Clone)]
pub struct Crate {
    /// The package name.
    pub package: String,
    /// The crate name: the target name with `_` for `-`.
    pub name: String,
    /// The kind.
    pub kind: CrateKind,
    /// True for a production binary.
    pub production: bool,
    /// The edition.
    pub edition: Edition,
    /// The root module.
    pub root_module: ModuleId,
    /// The names of other crates that the code can use, with the workspace
    /// crate when the name is a workspace library.
    pub externs: Vec<Extern>,
    /// The features of the package. All of them can be on for the target.
    pub features: Vec<String>,
    /// True when a registry can publish the package.
    pub published: bool,
    /// The features that the target needs.
    pub required_features: Vec<String>,
    /// The names that the code blocks of the documentation can use. rustdoc
    /// compiles them as a crate that depends on the library and on its
    /// dev-dependencies.
    pub doc_externs: Vec<Extern>,
}

/// A crate name that the code can use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extern {
    /// The name in code.
    pub name: String,
    /// The workspace library, or `None` for a crate outside the workspace.
    pub target: Option<CrateId>,
    /// The `cfg` or target triple of a platform-specific dependency.
    pub platform: Option<String>,
    /// True for an optional dependency, which a feature enables.
    pub optional: bool,
}

/// One module.
#[derive(Debug, Clone)]
pub struct Module {
    /// The parent module.
    pub parent: Option<ModuleId>,
    /// The module path from the crate root.
    pub path: Vec<String>,
    /// The `mod` item that declares the module. The root has none.
    pub item: Option<ItemId>,
    /// True when an inner attribute of the module file allows the `dead_code`
    /// lint.
    pub allow_dead: bool,
}

/// One item.
#[derive(Debug, Clone)]
pub struct Item {
    /// The crate.
    pub krate: CrateId,
    /// The module that holds the item.
    pub module: ModuleId,
    /// The name.
    pub name: String,
    /// The kind.
    pub kind: ItemKind,
    /// The written visibility.
    pub vis: Vis,
    /// The file, relative to the workspace root.
    pub file: String,
    /// The one-based line of the name.
    pub line: u32,
    /// The one-based column of the name.
    pub column: u32,
    /// The one-based last line.
    pub end_line: u32,
    /// The written visibility keyword, when the item has one.
    pub vis_span: Option<crate::graph::extract::Span>,
    /// The enclosing item.
    pub parent: Option<ItemId>,
    /// The self type path of an `impl` block.
    pub impl_self: Option<Vec<String>>,
    /// True for an `impl Trait for Type` block.
    pub impl_trait: bool,
    /// The module of a `mod` item.
    pub child_module: Option<ModuleId>,
    /// True when the item exists only in test builds.
    pub test_only: bool,
    /// True when a `cfg` attribute is on the item or on its file.
    pub under_cfg: bool,
    /// The root facts.
    pub flags: ItemFlags,
}

/// One import.
#[derive(Debug, Clone)]
pub struct Import {
    /// The crate.
    pub krate: CrateId,
    /// The module that holds the import.
    pub module: ModuleId,
    /// The visibility of the `use` item.
    pub vis: Vis,
    /// The path.
    pub path: Vec<String>,
    /// True when the path starts with `::`.
    pub global: bool,
    /// The binding.
    pub binding: Binding,
    /// The item of a named `pub` re-export.
    pub item: Option<ItemId>,
    /// The file of the `use` item.
    pub file: FileId,
    /// The one-based line of the `use` item.
    pub line: u32,
}

/// One reference.
#[derive(Debug, Clone)]
pub struct Ref {
    /// The crate of the code.
    pub krate: CrateId,
    /// The module of the code.
    pub module: ModuleId,
    /// The item that holds the code.
    pub owner: Option<ItemId>,
    /// The reference.
    pub kind: RefKind,
    /// Where the reference is.
    pub origin: Origin,
    /// True in test code.
    pub test_only: bool,
    /// True when a `cfg` that can be off in a build with all features and
    /// tests is on the code or on a module declaration of its file.
    pub under_cfg: bool,
    /// The file of the code. For the code of an `include!` file, the file of
    /// the call.
    pub file: FileId,
    /// The one-based line of the code, or zero when the place is not known.
    pub line: u32,
}

/// The workspace module graph.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    /// The crates, in the order of the members and their targets.
    pub crates: Vec<Crate>,
    /// The modules.
    pub modules: Vec<Module>,
    /// The items.
    pub items: Vec<Item>,
    /// The imports.
    pub imports: Vec<Import>,
    /// The references.
    pub refs: Vec<Ref>,
    /// The files of the imports and the references, relative to the
    /// workspace root. A file in more than one crate has one entry for each
    /// crate.
    pub paths: Vec<String>,
    /// The number of parsed files, counted once for each crate.
    pub files: u32,
    /// The files that a declaration reached but that did not parse.
    pub skipped: Vec<SkippedFile>,
    /// The `.rs` files under the root folders that no declaration reached.
    pub unreached: u32,
    /// The parsed files with a generator header or an `@generated` comment in
    /// the first lines. A generator writes their code, so no check reports it.
    pub generated_files: BTreeSet<String>,
}

/// The parsed files of one crate.
struct CrateFiles {
    files: Vec<ParsedFile>,
    skipped: Vec<SkippedFile>,
    unreached: u32,
}

struct ParsedFile {
    path: String,
    module_path: Vec<String>,
    under_cfg: bool,
    cfg_off: bool,
    generated: bool,
    facts: FileFacts,
}

/// Builds the graph of a workspace.
pub fn build(workspace: &Workspace, production: &Production) -> Graph {
    let root = workspace.root.as_std_path();
    let targets: Vec<(&Member, &Target)> = workspace
        .members
        .iter()
        .flat_map(|member| member.targets.iter().map(move |target| (member, target)))
        .collect();
    let parsed = parse_all(root, &targets);
    let mut graph = Graph::default();
    let libs: BTreeMap<&str, CrateId> = targets
        .iter()
        .enumerate()
        .filter(|(_, (_, target))| CrateKind::of(target).is_library())
        .map(|(id, (member, _))| (member.name.as_str(), id))
        .collect();
    for (id, ((member, target), files)) in targets.iter().zip(parsed).enumerate() {
        let kind = CrateKind::of(target);
        let edition = parse_edition(&target.edition);
        let production = kind == CrateKind::Bin
            && (production.bins.is_empty()
                || production
                    .bins
                    .iter()
                    .any(|(package, bin)| *package == member.name && *bin == target.name));
        let lib = libs
            .get(member.name.as_str())
            .copied()
            .filter(|lib| *lib != id);
        let externs = externs_of(member, kind, lib, &libs, workspace);
        let doc_externs = if kind.is_library() && target.doctest {
            let mut names = externs_of(member, CrateKind::Test, Some(id), &libs, workspace);
            names.retain(|name| name.target != Some(id) || name.name == crate_name(&target.name));
            names
        } else {
            Vec::new()
        };
        let root_module = graph.modules.len();
        graph.modules.push(Module {
            parent: None,
            path: Vec::new(),
            item: None,
            allow_dead: false,
        });
        graph.crates.push(Crate {
            package: member.name.clone(),
            name: crate_name(&target.name),
            kind,
            production,
            edition,
            root_module,
            externs,
            features: member.features.clone(),
            published: member.published,
            required_features: target.required_features.clone(),
            doc_externs,
        });
        let generated = files.files.iter().filter(|file| file.generated);
        graph
            .generated_files
            .extend(generated.map(|file| file.path.clone()));
        add_files(
            &mut graph,
            root,
            id,
            files,
            kind.is_library() && target.doctest,
        );
    }
    graph
}

/// Parses the files of each target. The targets run on parallel threads.
fn parse_all(root: &Path, targets: &[(&Member, &Target)]) -> Vec<CrateFiles> {
    let workers = thread::available_parallelism()
        .map_or(1, usize::from)
        .clamp(1, 8);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: std::sync::Mutex<Vec<Option<CrateFiles>>> = std::sync::Mutex::new(
        std::iter::repeat_with(|| None)
            .take(targets.len())
            .collect(),
    );
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((_, target)) = targets.get(index) else {
                        break;
                    };
                    let files = parse_target(root, target);
                    if let Ok(mut slots) = results.lock() {
                        slots[index] = Some(files);
                    }
                }
            });
        }
    });
    results
        .into_inner()
        .unwrap_or_default()
        .into_iter()
        .map(|files| {
            files.unwrap_or(CrateFiles {
                files: Vec::new(),
                skipped: Vec::new(),
                unreached: 0,
            })
        })
        .collect()
}

fn parse_target(root: &Path, target: &Target) -> CrateFiles {
    let edition = parse_edition(&target.edition);
    let roots = [TargetRoot {
        path: target.src_path.clone(),
        edition,
    }];
    let walked = walk(root, &roots, |visit| {
        let source = fs::read_to_string(root.join(&visit.path))
            .map_err(|err| format!("cannot read the file: {err}"))?;
        let facts = extract(&source, visit.edition)
            .map_err(|outcome| format!("the parse failed: {outcome:?}"))?;
        let modules = facts.modules.clone();
        Ok((
            ParsedFile {
                path: visit.path.clone(),
                module_path: visit.module_path.clone(),
                under_cfg: visit.under_cfg,
                cfg_off: visit.cfg_off,
                generated: is_generated(&source) || has_generated_marker(&source),
                facts,
            },
            modules,
        ))
    });
    CrateFiles {
        files: walked.files,
        skipped: walked.skipped,
        unreached: walked.unreached,
    }
}

/// The crate name of a target name.
pub fn crate_name(target: &str) -> String {
    target.replace('-', "_")
}

fn externs_of(
    member: &Member,
    kind: CrateKind,
    own_lib: Option<CrateId>,
    libs: &BTreeMap<&str, CrateId>,
    workspace: &Workspace,
) -> Vec<Extern> {
    let mut names = Vec::new();
    if let (Some(lib), true) = (
        own_lib,
        kind != CrateKind::Lib && kind != CrateKind::ProcMacro,
    ) {
        let lib_name = member
            .targets
            .iter()
            .find(|target| CrateKind::of(target).is_library())
            .map_or_else(
                || crate_name(&member.name),
                |target| crate_name(&target.name),
            );
        names.push(Extern {
            name: lib_name,
            target: Some(lib),
            platform: None,
            optional: false,
        });
    }
    let wanted = |link_kind: LinkKind| match kind {
        CrateKind::Build => link_kind == LinkKind::Build,
        // The unit tests of a library or a binary can use the dev-dependencies.
        _ => link_kind != LinkKind::Build,
    };
    for link in member.links.iter().filter(|link| wanted(link.kind)) {
        let target = libs.get(link.package.as_str()).copied();
        let lib_name = target
            .and_then(|_| {
                workspace
                    .members
                    .iter()
                    .find(|other| other.name == link.package)
            })
            .and_then(|other| {
                other
                    .targets
                    .iter()
                    .find(|target| CrateKind::of(target).is_library())
            })
            .map_or_else(
                || crate_name(&link.package),
                |target| crate_name(&target.name),
            );
        let name = link.rename.as_deref().map_or(lib_name, crate_name);
        names.push(Extern {
            name,
            target,
            platform: link.platform.clone(),
            optional: link.optional,
        });
    }
    names.sort_by(|a, b| a.name.cmp(&b.name).then(a.target.cmp(&b.target)));
    names.dedup();
    names
}

fn add_files(graph: &mut Graph, root: &Path, krate: CrateId, files: CrateFiles, doctest: bool) {
    graph.skipped.extend(files.skipped);
    graph.unreached += files.unreached;
    let root_module = graph.crates[krate].root_module;
    let edition = graph.crates[krate].edition;
    let mut by_path: BTreeMap<Vec<String>, ModuleId> = BTreeMap::new();
    by_path.insert(Vec::new(), root_module);
    for file in files.files {
        graph.files += 1;
        let file_id = FileId::try_from(graph.paths.len()).unwrap_or(FileId::MAX);
        graph.paths.push(file.path.clone());
        let base = file.module_path.clone();
        if file.facts.allow_dead {
            let module = module_id(graph, &mut by_path, &base);
            graph.modules[module].allow_dead = true;
        }
        let mut ids = Vec::with_capacity(file.facts.items.len());
        for raw in &file.facts.items {
            let mut path = base.clone();
            path.extend(raw.inline.iter().cloned());
            let module = module_id(graph, &mut by_path, &path);
            let id = graph.items.len();
            ids.push(id);
            let child_module = (raw.kind == ItemKind::Module).then(|| {
                let mut child = path.clone();
                child.push(raw.name.clone());
                let child_id = module_id(graph, &mut by_path, &child);
                graph.modules[child_id].item = Some(id);
                child_id
            });
            graph.items.push(Item {
                krate,
                module,
                name: raw.name.clone(),
                kind: raw.kind,
                vis: raw.vis.clone(),
                file: file.path.clone(),
                line: raw.line,
                column: raw.column,
                end_line: raw.end_line,
                vis_span: raw.vis_span,
                parent: None,
                impl_self: raw.impl_self.clone(),
                impl_trait: raw.impl_trait.is_some(),
                child_module,
                test_only: raw.test_only || module_is_test(graph, module),
                under_cfg: raw.under_cfg || file.under_cfg,
                flags: raw.flags,
            });
        }
        for (raw, id) in file.facts.items.iter().zip(&ids) {
            graph.items[*id].parent = raw.parent.map(|parent| ids[parent]);
        }
        for raw in file.facts.uses {
            let mut path = base.clone();
            path.extend(raw.inline.iter().cloned());
            let module = module_id(graph, &mut by_path, &path);
            graph
                .imports
                .push(import_of(raw, krate, module, &ids, file_id));
        }
        let mut refs = file.facts.refs;
        let mut budget = MAX_INCLUDED_FILES;
        refs.extend(included_code(
            root,
            &mut vec![file.path.clone()],
            file.facts.code_includes,
            edition,
            &mut budget,
        ));
        if doctest {
            refs.extend(included_docs(
                root,
                &file.path,
                &file.facts.doc_includes,
                edition,
            ));
        }
        for raw in refs {
            if raw.origin == Origin::Doc && !doctest {
                continue;
            }
            let mut path = base.clone();
            path.extend(raw.inline.iter().cloned());
            let module = module_id(graph, &mut by_path, &path);
            graph.refs.push(Ref {
                krate,
                module,
                owner: raw.owner.map(|index| ids[index]),
                kind: unraw_kind(raw.kind),
                origin: raw.origin,
                test_only: raw.test_only,
                under_cfg: raw.under_cfg || file.cfg_off,
                file: file_id,
                line: raw.line,
            });
        }
    }
}

fn import_of(
    raw: crate::graph::extract::RawUse,
    krate: CrateId,
    module: ModuleId,
    ids: &[ItemId],
    file: FileId,
) -> Import {
    Import {
        krate,
        module,
        vis: raw.vis,
        path: raw.path.into_iter().map(unraw).collect(),
        global: raw.global,
        binding: raw.binding,
        item: raw.item.map(|index| ids[index]),
        file,
        line: raw.line,
    }
}

/// The name without the `r#` prefix of a raw identifier. The items have their
/// names in this form.
fn unraw(name: String) -> String {
    match name.strip_prefix("r#") {
        Some(plain) => plain.to_owned(),
        None => name,
    }
}

fn unraw_kind(kind: RefKind) -> RefKind {
    let all = |segments: Vec<String>| segments.into_iter().map(unraw).collect();
    match kind {
        RefKind::Path { segments, global } => RefKind::Path {
            segments: all(segments),
            global,
        },
        RefKind::Method(name) => RefKind::Method(unraw(name)),
        RefKind::Field(name) => RefKind::Field(unraw(name)),
        RefKind::RecordField {
            segments,
            global,
            field,
        } => RefKind::RecordField {
            segments: all(segments),
            global,
            field: unraw(field),
        },
        RefKind::Macro(name) => RefKind::Macro(unraw(name)),
        RefKind::Member(name) => RefKind::Member(unraw(name)),
    }
}

fn module_is_test(graph: &Graph, module: ModuleId) -> bool {
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

fn module_id(
    graph: &mut Graph,
    by_path: &mut BTreeMap<Vec<String>, ModuleId>,
    path: &[String],
) -> ModuleId {
    if let Some(id) = by_path.get(path) {
        return *id;
    }
    let parent = module_id(graph, by_path, &path[..path.len() - 1]);
    let id = graph.modules.len();
    graph.modules.push(Module {
        parent: Some(parent),
        path: path.to_vec(),
        item: None,
        allow_dead: false,
    });
    by_path.insert(path.to_vec(), id);
    id
}

/// The maximum number of `include!` files that the graph reads for one file.
/// A file over this budget keeps the `include` reference at the call.
const MAX_INCLUDED_FILES: u32 = 64;

/// The maximum depth of nested `include!` files.
const MAX_INCLUDE_DEPTH: usize = 8;

/// The references of the files that `include!("path")` reads. The code of the
/// file is part of the module of the call, so each reference gets the inline
/// modules, the owner and the test state of the call. An import of the file
/// becomes a path reference, because the graph does not add the items of the
/// file. `chain` holds the file of the call and the files that include it.
/// When the file cannot be read or parsed, is on the chain, is too deep or is
/// over the budget, the `include` reference at the call stays.
fn included_code(
    root: &Path,
    chain: &mut Vec<String>,
    includes: Vec<CodeInclude>,
    edition: Edition,
    budget: &mut u32,
) -> Vec<RawRef> {
    let Some(file) = chain.last().cloned() else {
        return Vec::new();
    };
    let folder = Path::new(&file).parent().unwrap_or_else(|| Path::new(""));
    let mut refs = Vec::new();
    for include in includes {
        let path = normalize(&folder.join(&include.path)).filter(|path| {
            !chain.contains(path) && chain.len() <= MAX_INCLUDE_DEPTH && *budget > 0
        });
        let facts = path.as_ref().and_then(|path| {
            *budget -= 1;
            let source = fs::read_to_string(root.join(path)).ok()?;
            extract(&source, edition).ok()
        });
        let (Some(path), Some(facts)) = (path, facts) else {
            refs.push(include.site);
            continue;
        };
        let site = include.site;
        let at_site = |inline: Vec<String>, kind: RefKind, origin: Origin, test_only: bool| {
            let mut full = site.inline.clone();
            full.extend(inline);
            RawRef {
                inline: full,
                owner: site.owner,
                kind,
                origin,
                test_only: test_only || site.test_only,
                under_cfg: site.under_cfg,
                line: site.line,
            }
        };
        for import in facts.uses {
            let kind = RefKind::Path {
                segments: import.path,
                global: import.global,
            };
            refs.push(at_site(import.inline, kind, Origin::Body, false));
        }
        for raw in facts.refs {
            refs.push(at_site(raw.inline, raw.kind, raw.origin, raw.test_only));
        }
        let nested = facts
            .code_includes
            .into_iter()
            .map(|nested| CodeInclude {
                path: nested.path,
                site: at_site(
                    nested.site.inline,
                    nested.site.kind,
                    nested.site.origin,
                    nested.site.test_only,
                ),
            })
            .collect();
        chain.push(path);
        refs.extend(included_code(root, chain, nested, edition, budget));
        chain.pop();
    }
    refs
}

fn included_docs(root: &Path, file: &str, includes: &[String], edition: Edition) -> Vec<RawRef> {
    let folder = Path::new(file).parent().unwrap_or_else(|| Path::new(""));
    includes
        .iter()
        .filter_map(|include| normalize(&folder.join(include)))
        .filter_map(|path| fs::read_to_string(root.join(path)).ok())
        .flat_map(|markdown| doc_refs_of_markdown(&markdown, edition))
        .collect()
}

impl Graph {
    /// The module path of an item, with the item name, as hawk prints it:
    /// `module::Type::method`.
    pub fn item_path(&self, item: ItemId) -> String {
        let mut parts = self.modules[self.items[item].module].path.clone();
        let mut chain = Vec::new();
        let mut current = Some(item);
        while let Some(id) = current {
            let entry = &self.items[id];
            match entry.kind {
                ItemKind::Impl => {
                    if let Some(self_ty) = entry.impl_self.as_ref().and_then(|path| path.last()) {
                        chain.push(self_ty.clone());
                    }
                }
                ItemKind::Module if id != item => {}
                _ => chain.push(entry.name.clone()),
            }
            current = entry.parent;
        }
        chain.reverse();
        parts.extend(chain);
        parts.join("::")
    }
}

#[cfg(test)]
mod tests;
