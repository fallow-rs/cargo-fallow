//! `visibility --trace <crate>::<path>`: each use and each credit that keeps
//! one item, and the verdict of the fast mode.

use std::fmt::Write as _;

use serde::Serialize;

use super::{Finding, Options, Surface, analyze, skip_generated};
use crate::error::{CliError, ErrorCode};
use crate::graph::extract::{Binding, ItemKind, Origin, RefKind};
use crate::graph::resolve::{Class, Credit, Resolver, Source};
use crate::graph::{CrateKind, Graph, ItemId, crate_name};

/// The `kind` of the trace document.
pub const KIND: &str = "visibility-trace";

/// The trace of one item path. The field order is the wire order.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct VisibilityTrace {
    /// The version of the trace shape.
    pub schema_version: u32,
    /// The version of cargo-fallow.
    pub version: &'static str,
    /// Always `visibility-trace`.
    pub kind: &'static str,
    /// Always `rust`.
    pub language: &'static str,
    /// The value of `--trace`.
    pub trace: String,
    /// The items with this path. A path can name more than one item, for
    /// example a library and a binary with the same crate name, or two items
    /// under different `cfg` conditions.
    pub items: Vec<TracedItem>,
}

/// One traced item.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct TracedItem {
    /// The item path from the crate root.
    pub path: String,
    /// The crate name.
    #[serde(rename = "crate")]
    pub krate: String,
    /// The package.
    pub package: String,
    /// The target kind of the crate.
    pub crate_kind: &'static str,
    /// The item kind.
    pub item_kind: &'static str,
    /// The written visibility. An empty text means private.
    pub visibility: String,
    /// The file, relative to the workspace root.
    pub file: String,
    /// The one-based line of the name.
    pub line: u32,
    /// True when another crate can name the item.
    pub exported: bool,
    /// True when a use or a hold keeps the item.
    pub is_used: bool,
    /// The finding kinds of the item in the fast mode.
    pub findings: Vec<&'static str>,
    /// The finding kinds that only an untyped credit stops. `--precise` mode
    /// decides them.
    pub undecided: Vec<&'static str>,
    /// The facts other than a use that keep the item or its visibility, for
    /// example `glob-reexport` or `trait-impl`.
    pub holds: Vec<&'static str>,
    /// Each place in code that gives the item a credit, sorted by file and
    /// line. A use inside the item itself and the `use` item of a re-export
    /// are not in the list.
    pub uses: Vec<TracedUse>,
    /// A one-line summary of the verdict.
    pub reason: String,
}

/// One place in code that gives a credit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema)]
pub struct TracedUse {
    /// The file, relative to the workspace root.
    pub file: String,
    /// The one-based line, or `None` when the place is not known, for a code
    /// block of an included Markdown file. For macro tokens, the line where
    /// the macro call starts. For an attribute, the line of the attribute.
    /// For a documentation code block, the first code line of the block.
    pub line: Option<u32>,
    /// How the code names the item.
    #[schemars(extend("enum" = [
        "import", "glob-import", "path", "name-match", "method-name",
        "field-name", "member-name", "record-field", "macro-call",
        "macro-tokens", "attribute", "doc-code"
    ]))]
    pub credit: &'static str,
    /// How sure the credit is: `exact`, `fallback` (the path did not resolve,
    /// so each item with the name gets credit) or `untyped` (the syntax cannot
    /// decide the type).
    #[schemars(extend("enum" = ["exact", "fallback", "untyped"]))]
    pub class: &'static str,
    /// The crate name of the code.
    #[serde(rename = "crate")]
    pub krate: String,
    /// The target kind of the crate of the code.
    pub crate_kind: &'static str,
    /// True in test code.
    pub test_only: bool,
    /// True when a `cfg` that can be off is on the code.
    pub under_cfg: bool,
}

/// Traces the items that `target` names. `target` is the crate name, `::`
/// and the item path, as the `crate` and `item` fields of a finding give them.
/// `kinds` are the finding kinds of the run: the trace gives no verdict of
/// another kind. An empty list keeps each kind.
pub fn trace(
    graph: &Graph,
    options: &Options,
    kinds: &[super::FindingKind],
    target: &str,
) -> Result<VisibilityTrace, CliError> {
    let items = matching_items(graph, target)?;
    let mut analysis = analyze(graph, options);
    skip_generated(graph, &mut analysis);
    analysis.retain_kinds(kinds);
    let resolver = Resolver::new(graph);
    let credits = resolver.credits();
    let surface = Surface::new(graph, &resolver, options);
    let kinds = |list: &[Finding], item: ItemId| -> Vec<&'static str> {
        list.iter()
            .filter(|finding| finding.item == item)
            .map(|finding| finding.kind.label())
            .collect()
    };
    let items = items
        .into_iter()
        .map(|item| {
            let entry = &graph.items[item];
            let krate = &graph.crates[entry.krate];
            let uses = uses_of(graph, &credits, item);
            let holds = holds_of(graph, &surface, item, &analysis.opaque);
            let findings = kinds(&analysis.findings, item);
            let undecided = kinds(&analysis.undecided, item);
            let candidate =
                surface.public_candidate(graph, item) || super::restricted_candidate(entry);
            let reason = reason(&findings, &undecided, candidate, uses.len(), &holds);
            TracedItem {
                path: graph.item_path(item),
                krate: krate.name.clone(),
                package: krate.package.clone(),
                crate_kind: krate.kind.label(),
                item_kind: entry.kind.label(),
                visibility: super::report::written(&entry.vis, entry.kind),
                file: entry.file.clone(),
                line: entry.line,
                exported: surface.exported(item),
                is_used: !uses.is_empty() || !holds.is_empty(),
                findings,
                undecided,
                holds,
                uses,
                reason,
            }
        })
        .collect();
    Ok(VisibilityTrace {
        schema_version: crate::trace::SCHEMA_VERSION,
        version: crate::report::TOOL_VERSION,
        kind: KIND,
        language: "rust",
        trace: target.to_owned(),
        items,
    })
}

/// The items that `target` names, or an input error when it names none.
fn matching_items(graph: &Graph, target: &str) -> Result<Vec<ItemId>, CliError> {
    let not_found = || {
        CliError::new(
            ErrorCode::InvalidArgument,
            format!("`{target}` names no item of the workspace"),
        )
        .with_help(
            "Give the crate name, `::` and the item path, for example \
             `my_crate::module::Type::method`. The `crate` and `item` fields of a \
             finding give them.",
        )
    };
    let Some((krate, path)) = target.split_once("::") else {
        return Err(not_found());
    };
    let krate = crate_name(krate);
    let items: Vec<ItemId> = (0..graph.items.len())
        .filter(|&item| {
            let entry = &graph.items[item];
            entry.kind != ItemKind::Impl
                && graph.crates[entry.krate].name == krate
                && graph.item_path(item) == path
        })
        .collect();
    if items.is_empty() {
        return Err(not_found());
    }
    Ok(items)
}

/// The places that give `item` a credit, without duplicates.
fn uses_of(graph: &Graph, credits: &[Credit], item: ItemId) -> Vec<TracedUse> {
    let mut uses: Vec<TracedUse> = credits
        .iter()
        .filter(|credit| credit.item == item && credit.owner != Some(item))
        // A named `pub use` resolves to its own item too. That is not a use.
        .filter(|credit| {
            !matches!(credit.source, Source::Import(index) if graph.imports[index].item == Some(item))
        })
        .map(|credit| {
            let (file, line) = place(graph, credit.source);
            let krate = &graph.crates[credit.krate];
            TracedUse {
                file,
                line,
                credit: credit_label(graph, credit),
                class: class_label(credit.class),
                krate: krate.name.clone(),
                crate_kind: krate.kind.label(),
                test_only: credit.test_only || module_is_test(graph, credit),
                under_cfg: credit.under_cfg,
            }
        })
        .collect();
    uses.sort();
    uses.dedup();
    uses
}

/// True when the code of the credit is in a test module or a test crate.
fn module_is_test(graph: &Graph, credit: &Credit) -> bool {
    if matches!(
        graph.crates[credit.krate].kind,
        CrateKind::Test | CrateKind::Bench | CrateKind::Example
    ) {
        return true;
    }
    let mut current = Some(credit.module);
    while let Some(module) = current {
        if graph.modules[module]
            .item
            .is_some_and(|item| graph.items[item].test_only)
        {
            return true;
        }
        current = graph.modules[module].parent;
    }
    credit
        .owner
        .is_some_and(|owner| graph.items[owner].test_only || graph.items[owner].flags.test_fn)
}

/// The file and the line of the code of a credit.
fn place(graph: &Graph, source: Source) -> (String, Option<u32>) {
    let (file, line) = match source {
        Source::Import(index) => (graph.imports[index].file, graph.imports[index].line),
        Source::Ref(index) => (graph.refs[index].file, graph.refs[index].line),
    };
    let path = usize::try_from(file)
        .ok()
        .and_then(|file| graph.paths.get(file))
        .cloned()
        .unwrap_or_default();
    (path, (line > 0).then_some(line))
}

/// How the code of a credit names the item.
fn credit_label(graph: &Graph, credit: &Credit) -> &'static str {
    let reference = match credit.source {
        Source::Import(index) => {
            return if graph.imports[index].binding == Binding::Glob {
                "glob-import"
            } else {
                "import"
            };
        }
        Source::Ref(index) => &graph.refs[index],
    };
    match reference.origin {
        Origin::Doc => return "doc-code",
        Origin::Attr => return "attribute",
        Origin::Macro if !matches!(reference.kind, RefKind::Macro(_)) => return "macro-tokens",
        _ => {}
    }
    match &reference.kind {
        RefKind::Path { .. } if credit.class == Class::Exact => "path",
        RefKind::Path { .. } => "name-match",
        RefKind::Method(_) => "method-name",
        RefKind::Field(_) => "field-name",
        RefKind::Member(_) => "member-name",
        RefKind::RecordField { .. } => "record-field",
        RefKind::Macro(_) => "macro-call",
    }
}

const fn class_label(class: Class) -> &'static str {
    match class {
        Class::Exact => "exact",
        Class::Fallback => "fallback",
        Class::Untyped => "untyped",
    }
}

/// The facts other than a use that keep the item or its visibility.
fn holds_of(
    graph: &Graph,
    surface: &Surface,
    item: ItemId,
    opaque: &[Finding],
) -> Vec<&'static str> {
    let entry = &graph.items[item];
    let krate = &graph.crates[entry.krate];
    let main = entry.name == "main"
        && entry.kind == ItemKind::Function
        && entry.parent.is_none()
        && entry.module == krate.root_module;
    let derived = entry.kind == ItemKind::Field
        && entry
            .parent
            .is_some_and(|parent| super::derived(graph, parent));
    let reexports = entry.kind == ItemKind::Module
        && entry
            .child_module
            .is_some_and(|module| surface.modules_with_reexports.contains(&module));
    let facts = [
        (surface.excluded.contains(&entry.krate), "excluded-crate"),
        (
            !surface.excluded.contains(&entry.krate) && surface.api.contains(&entry.krate),
            "published-crate",
        ),
        (surface.glob_exported.contains(&item), "glob-reexport"),
        (reexports, "module-with-reexports"),
        (entry.flags.macro_export, "macro-export"),
        (surface.macro_covered[item], "attribute-macro"),
        (surface.dead_allowed[item], "allow-dead-code"),
        (entry.flags.external, "external-symbol"),
        (entry.flags.trait_impl, "trait-impl"),
        (entry.flags.trait_item, "trait-item"),
        (entry.flags.proc_macro, "proc-macro"),
        (entry.flags.test_fn, "test-function"),
        (derived, "derive"),
        (main, "main-function"),
        (
            graph.generated_files.contains(&entry.file),
            "generated-file",
        ),
        (
            opaque.iter().any(|finding| finding.item == item),
            "opaque-include",
        ),
    ];
    facts
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, label)| label)
        .collect()
}

fn reason(
    findings: &[&'static str],
    undecided: &[&'static str],
    candidate: bool,
    uses: usize,
    holds: &[&'static str],
) -> String {
    match findings.first() {
        Some(&"dead_public") => return "dead_public: no code reaches the item".to_owned(),
        Some(kind) => return format!("{kind}: the item can have a smaller visibility"),
        None => {}
    }
    if let Some(kind) = undecided.first() {
        return format!(
            "undecided {kind}: only an untyped credit keeps the item. Run `--precise` to decide it"
        );
    }
    if !candidate && holds.is_empty() {
        return "no rule judges the item: other crates cannot name it, and its visibility is \
                not restricted"
            .to_owned();
    }
    match (uses, holds.first()) {
        (0, None) => "kept: the item is not a candidate of a rule".to_owned(),
        (0, Some(hold)) => format!("kept: {hold}"),
        (1, _) => "kept: 1 use".to_owned(),
        (count, _) => format!("kept: {count} uses"),
    }
}

impl VisibilityTrace {
    /// The trace as text for a person.
    pub fn human(&self) -> String {
        let mut text = String::new();
        let _ = writeln!(text, "cargo fallow visibility --trace {}", self.trace);
        for item in &self.items {
            let visibility = if item.visibility.is_empty() {
                "private"
            } else {
                item.visibility.as_str()
            };
            let _ = writeln!(
                text,
                "\n{}::{} ({} {} in {} of `{}`) at {}:{}",
                item.krate,
                item.path,
                visibility,
                item.item_kind,
                item.crate_kind,
                item.package,
                item.file,
                item.line
            );
            let _ = writeln!(text, "  {}", item.reason);
            if !item.holds.is_empty() {
                let _ = writeln!(text, "  holds: {}", item.holds.join(", "));
            }
            if item.uses.is_empty() {
                let _ = writeln!(text, "  no use in the workspace");
            }
            for entry in &item.uses {
                let mut notes = vec![entry.class];
                if entry.test_only {
                    notes.push("test");
                }
                if entry.under_cfg {
                    notes.push("cfg");
                }
                let _ = writeln!(
                    text,
                    "  {}:{} {} in {} `{}` ({})",
                    entry.file,
                    crate::trace::line_text(entry.line),
                    entry.credit,
                    entry.crate_kind,
                    entry.krate,
                    notes.join(", ")
                );
            }
        }
        text
    }
}
