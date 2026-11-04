//! `deps --trace <dependency>`: each declaration of one dependency, each use
//! site in each target, and its findings.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;

use cargo_metadata::DependencyKind;
use serde::Serialize;

use super::manifest::{self, Section, normalize_platform};
use super::report::{Finding, SkippedPackage};
use super::usage::{self, Context, Site};
use super::{Options, analyze};
use crate::error::{CliError, ErrorCode};
use crate::graph::{self, Graph, Production, crate_name};
use crate::render;
use crate::workspace::{self, Workspace};

/// The `kind` of the trace document.
pub const KIND: &str = "deps-trace";

/// The trace of one dependency. The field order is the wire order.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct DepsTrace {
    /// The version of the trace shape.
    pub schema_version: u32,
    /// The version of cargo-fallow.
    pub version: &'static str,
    /// Always `deps-trace`.
    pub kind: &'static str,
    /// Always `rust`.
    pub language: &'static str,
    /// The value of `--trace`.
    pub package_name: String,
    /// True when code of a package that declares the dependency uses it.
    /// `used` of each declaration tells which entry the code uses.
    pub is_used: bool,
    /// The entries in Cargo.toml that declare the dependency, sorted by
    /// manifest and line.
    pub declarations: Vec<Declaration>,
    /// Each place in code that uses the dependency, sorted by package, file
    /// and line.
    pub uses: Vec<TracedSite>,
    /// The findings of the `deps` check for the dependency.
    pub findings: Vec<Finding>,
    /// The declaring packages that the `deps` check does not judge, with the
    /// reason, for example a file that did not parse.
    pub skipped: Vec<SkippedPackage>,
}

/// One entry in Cargo.toml.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema)]
#[schemars(rename = "DepsTraceDeclaration")]
pub struct Declaration {
    /// The `Cargo.toml` path, relative to the workspace root.
    pub manifest_path: String,
    /// The one-based line of the entry, when the scan found it.
    pub line: Option<u32>,
    /// The package. A `[workspace.dependencies]` entry has none.
    pub package: Option<String>,
    /// The table: `dependencies`, `dev-dependencies`, `build-dependencies` or
    /// `workspace.dependencies`.
    pub section: &'static str,
    /// The platform of a `[target.<platform>.*]` table.
    pub platform: Option<String>,
    /// The key in Cargo.toml: the dependency name or its rename.
    pub key: String,
    /// The crate name in code, when the resolve gives it.
    pub code_name: Option<String>,
    /// True for an optional dependency.
    pub optional: bool,
    /// True when code of the package of the entry uses the dependency: the
    /// build script for a build entry, another part for another entry. A
    /// `[workspace.dependencies]` entry is used when a member inherits it.
    pub used: bool,
}

/// One place in code.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema)]
pub struct TracedSite {
    /// The package of the code.
    pub package: String,
    /// The file, relative to the workspace root.
    pub file: String,
    /// The one-based line, or `None` when the place is not known. For macro
    /// tokens, the line where the macro call starts. For an attribute, the
    /// line of the attribute. For a documentation code block, the first code
    /// line of the block.
    pub line: Option<u32>,
    /// The part of the package, as in the `used_by` list of a finding.
    #[schemars(extend("enum" = [
        "lib", "proc-macro", "bin", "unit-test", "doctest", "test", "bench",
        "example", "build-script"
    ]))]
    pub context: &'static str,
    /// How the code uses the dependency.
    #[schemars(extend("enum" = [
        "import", "path", "macro-tokens", "attribute", "doc-code", "macro-crate",
        "include"
    ]))]
    pub via: &'static str,
    /// The crate name in the code. For `macro-crate`, the name of the macro
    /// crate whose output uses the dependency.
    pub name: String,
}

/// Runs `deps --trace` and writes the trace.
pub fn run(options: &Options, target: &str) -> Result<u8, CliError> {
    crate::trace::check_format(options.format, "deps")?;
    let progress = render::Progress::new(options.quiet);
    progress.step("Resolving the workspace with cargo metadata");
    let (workspace, metadata) =
        workspace::load_resolved(options.manifest_path.as_deref(), &options.cargo_flags)?;
    progress.step("Building the module graph");
    let module_graph = graph::build(&workspace, &Production::default());
    progress.step("Tracing the dependency");
    let traced = trace(&workspace, &metadata, &module_graph, target)?;
    crate::trace::write(options.format, &traced.human(), &traced)
}

/// Traces the dependency `target`: a key in Cargo.toml or a package name.
pub fn trace(
    workspace: &Workspace,
    metadata: &cargo_metadata::Metadata,
    module_graph: &Graph,
    target: &str,
) -> Result<DepsTrace, CliError> {
    let mut usage = usage::collect(module_graph, workspace.root.as_std_path());
    analyze::add_codegen_names(metadata, &mut usage);
    let matches = |key: &str, name: &str| {
        key == target || name == target || crate_name(key) == crate_name(target)
    };
    let inputs = Inputs {
        metadata,
        module_graph,
        usage: &usage,
    };
    let mut declarations = Vec::new();
    let mut uses = Vec::new();
    let mut inherited: BTreeSet<String> = BTreeSet::new();
    for package in metadata.workspace_packages() {
        let manifest_path = workspace::relative_path(&workspace.root, &package.manifest_path);
        let entries = fs::read_to_string(&package.manifest_path)
            .map(|text| manifest::entries(&text))
            .unwrap_or_default();
        for dependency in &package.dependencies {
            let key = dependency.rename.as_deref().unwrap_or(&dependency.name);
            if !matches(key, &dependency.name) {
                continue;
            }
            let (declaration, sites) =
                declaration_of(&inputs, package, dependency, &entries, &manifest_path);
            uses.extend(sites);
            declarations.push(declaration);
        }
        inherited.extend(
            entries
                .iter()
                .filter(|entry| entry.inherits)
                .map(|entry| entry.key.clone()),
        );
    }
    declarations.extend(workspace_declarations(
        workspace,
        &|key| matches(key, key),
        &inherited,
    ));
    if declarations.is_empty() {
        return Err(CliError::new(
            ErrorCode::InvalidArgument,
            format!("no Cargo.toml of the workspace declares `{target}`"),
        )
        .with_help("Give the key of the dependency in Cargo.toml or its package name."));
    }
    declarations.sort();
    declarations.dedup();
    uses.sort();
    uses.dedup();
    let analysis = analyze::analyze(workspace, metadata, module_graph);
    let declaring: BTreeSet<&str> = declarations
        .iter()
        .filter_map(|entry| entry.package.as_deref())
        .collect();
    let skipped = analysis
        .skipped
        .iter()
        .filter(|skipped| declaring.contains(skipped.package.as_str()))
        .cloned()
        .collect();
    let mut findings: Vec<Finding> = analysis
        .findings
        .into_iter()
        .filter(|finding| {
            matches(
                &finding.dependency,
                finding
                    .package_name
                    .as_deref()
                    .unwrap_or(&finding.dependency),
            )
        })
        .collect();
    findings.sort_by(|a, b| {
        (&a.manifest_path, a.line, a.rule).cmp(&(&b.manifest_path, b.line, b.rule))
    });
    Ok(DepsTrace {
        schema_version: crate::trace::SCHEMA_VERSION,
        version: crate::report::TOOL_VERSION,
        kind: KIND,
        language: "rust",
        package_name: target.to_owned(),
        is_used: !uses.is_empty(),
        declarations,
        uses,
        findings,
        skipped,
    })
}

/// The `[workspace.dependencies]` entries of the root manifest whose key
/// `matches` accepts.
fn workspace_declarations(
    workspace: &Workspace,
    matches: &dyn Fn(&str) -> bool,
    inherited: &BTreeSet<String>,
) -> Vec<Declaration> {
    let root_manifest = workspace.root.join("Cargo.toml");
    let Ok(text) = fs::read_to_string(&root_manifest) else {
        return Vec::new();
    };
    let manifest_path = workspace::relative_path(&workspace.root, &root_manifest);
    manifest::entries(&text)
        .into_iter()
        .filter(|entry| entry.section == Section::Workspace && matches(&entry.key))
        .map(|entry| Declaration {
            manifest_path: manifest_path.clone(),
            line: Some(entry.line),
            package: None,
            section: entry.section.label(),
            platform: None,
            used: inherited.contains(&entry.key),
            key: entry.key,
            code_name: None,
            optional: false,
        })
        .collect()
}

/// The facts of the workspace that each declaration reads.
struct Inputs<'a> {
    metadata: &'a cargo_metadata::Metadata,
    module_graph: &'a Graph,
    usage: &'a BTreeMap<String, usage::PackageUsage>,
}

/// The declaration of one dependency entry of `package`, and the places in
/// the code of the package that use it.
fn declaration_of(
    inputs: &Inputs<'_>,
    package: &cargo_metadata::Package,
    dependency: &cargo_metadata::Dependency,
    entries: &[manifest::Entry],
    manifest_path: &str,
) -> (Declaration, Vec<TracedSite>) {
    let key = dependency.rename.as_deref().unwrap_or(&dependency.name);
    let section = match dependency.kind {
        DependencyKind::Development => Section::Dev,
        DependencyKind::Build => Section::Build,
        _ => Section::Normal,
    };
    let platform = dependency.target.as_ref().map(ToString::to_string);
    // The check judges only a dependency that the resolve has.
    let code_name =
        analyze::resolved_packages(inputs.metadata, package, &dependency.name).map(|resolved| {
            match &dependency.rename {
                Some(rename) => crate_name(rename),
                None => {
                    analyze::lib_name(&resolved).unwrap_or_else(|| crate_name(&dependency.name))
                }
            }
        });
    let mut declaration = Declaration {
        manifest_path: manifest_path.to_owned(),
        line: line_of(entries, section, platform.as_deref(), key),
        package: Some(package.name.to_string()),
        section: section.label(),
        platform,
        key: key.to_owned(),
        code_name: code_name.clone(),
        optional: dependency.optional,
        used: false,
    };
    let Some(code_name) = code_name else {
        return (declaration, Vec::new());
    };
    let mut names = vec![code_name.clone()];
    names.extend(usage::macro_crates(&code_name));
    let include = inputs
        .usage
        .get(package.name.as_str())
        .is_some_and(|used| used.can_generate(&code_name));
    let sites = usage::sites(inputs.module_graph, &package.name, &names, include);
    // A build entry serves the build script. Another entry serves the other
    // parts.
    declaration.used = sites
        .iter()
        .any(|site| (site.context == Context::Build) == (section == Section::Build));
    let traced = sites
        .into_iter()
        .map(|site| traced_site(&package.name, &code_name, site))
        .collect();
    (declaration, traced)
}

fn traced_site(package: &str, code_name: &str, site: Site) -> TracedSite {
    let via = if site.name == code_name || site.via == "include" {
        site.via
    } else {
        "macro-crate"
    };
    TracedSite {
        package: package.to_owned(),
        file: site.file,
        line: site.line,
        context: site.context.label(),
        via,
        name: site.name,
    }
}

fn line_of(
    entries: &[manifest::Entry],
    section: Section,
    platform: Option<&str>,
    key: &str,
) -> Option<u32> {
    let platform = platform.map(normalize_platform);
    entries
        .iter()
        .find(|entry| {
            entry.section == section
                && entry.key == key
                && entry.platform.as_deref().map(normalize_platform) == platform
        })
        .map(|entry| entry.line)
}

impl DepsTrace {
    /// The trace as text for a person.
    pub fn human(&self) -> String {
        let mut text = String::new();
        let _ = writeln!(text, "cargo fallow deps --trace {}", self.package_name);
        let _ = writeln!(text, "\ndeclarations:");
        for entry in &self.declarations {
            let owner = entry
                .package
                .as_deref()
                .map_or_else(|| "workspace".to_owned(), |package| format!("`{package}`"));
            let _ = writeln!(
                text,
                "  {}:{} [{}] `{}` in {}{}",
                entry.manifest_path,
                crate::trace::line_text(entry.line),
                entry.section,
                entry.key,
                owner,
                if entry.optional { " (optional)" } else { "" }
            );
        }
        let _ = writeln!(text, "\nuses:");
        if self.uses.is_empty() {
            let _ = writeln!(text, "  no code of a declaring package uses it");
        }
        for site in &self.uses {
            let _ = writeln!(
                text,
                "  {}:{} {} in {} of `{}`",
                site.file,
                crate::trace::line_text(site.line),
                site.via,
                site.context,
                site.package
            );
        }
        for skipped in &self.skipped {
            let _ = writeln!(
                text,
                "\nthe check does not judge `{}`: {}",
                skipped.package, skipped.reason
            );
        }
        if !self.findings.is_empty() {
            let _ = writeln!(text, "\nfindings:");
            for finding in &self.findings {
                let _ = writeln!(
                    text,
                    "  {}:{} {}",
                    finding.manifest_path,
                    crate::trace::line_text(finding.line),
                    finding.rule.as_str()
                );
            }
        }
        text
    }
}
