//! The `files` check: the `.rs` files in a package that no target compiles.
//!
//! The walk starts at each target root and follows the `mod` declarations,
//! the `#[path]` attributes, the `path` values in `cfg_attr` attributes, the
//! `mod name;` tokens in macro calls and the literal `include!`,
//! `include_str!` and `include_bytes!` calls. A file that the walk does not
//! reach is unused. These limits keep a guess from giving a finding:
//!
//! - The check reads only the target folders of a package: `src`, `tests`,
//!   `benches`, `examples` and the folder of each target root.
//! - A folder with no reached `.rs` file is test data, for example the inputs
//!   of a compile test. The check skips it, and each folder with a fixture name.
//! - The check skips a folder with its own `Cargo.toml`, a generated file, and
//!   each file of a package whose code the syntax cannot follow: a computed
//!   `include!` path or a macro that declares a module from its input.
//!
//! The check cannot see a module that a macro of another crate declares. The
//! `exclude` list of `[workspace.metadata.fallow.files]` covers such a file.

mod scan;

pub use scan::{Scan, scan as scan_file};

/// The scan of each file that the module graph parsed, by the path and the
/// edition of the parse.
pub type FileScans = rustc_hash::FxHashMap<(String, crate::parse::Edition), Scan>;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use clap::Args;
use serde::{Deserialize, Serialize};

use crate::error::CliError;
use crate::finding::{self as shared, Category, Draft, Impact, Location, Severity};
use crate::gates::RustGateName as Gate;
use crate::gates::parse_edition;
use crate::modules::{TargetRoot, normalize, walk};
use crate::parse::with_parsed;
use crate::render::Progress;
use crate::scope::Scope;
use crate::section::{Section, to_value};
use crate::structure::CommonArgs;
use crate::structure::config::{self, matches_prefix};
use crate::structure::report::{Action, Report, count_gate, gates_text, plural, shared_actions};
use crate::structure::sarif;
use crate::structure::source::{has_generated_marker, is_generated, parse_reason};
use crate::workspace::{self, Workspace};

/// The rule of each finding.
pub const RULE: &str = "unused-file";

/// The standard target folders of a package.
const TARGET_FOLDERS: [&str; 4] = ["src", "tests", "benches", "examples"];

/// The folder names that hold test data, not code of a target.
const FIXTURE_FOLDERS: [&str; 14] = [
    "fixture",
    "fixtures",
    "testdata",
    "test-data",
    "test_data",
    "data",
    "snapshots",
    "ui",
    "compile-fail",
    "compile_fail",
    "corpus",
    "samples",
    "inputs",
    "resources",
];

/// The flags of `files`.
#[derive(Debug, Clone, Args)]
pub struct FilesArgs {
    #[command(flatten)]
    common: CommonArgs,
    /// Exit 1 when the run finds an unused file.
    #[arg(long)]
    fail_on_issues: bool,
}

impl FilesArgs {
    /// The output format.
    pub const fn format(&self) -> crate::cli::Format {
        self.common.format
    }
}

/// `[workspace.metadata.fallow.files]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct Settings {
    /// The folders and files that the check skips.
    pub exclude: Vec<String>,
}

/// One unused file.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[schemars(rename = "FilesFinding")]
pub struct Finding {
    /// Always `unused-file`.
    #[schemars(extend("const" = "unused-file"))]
    pub rule: &'static str,
    /// The package of the file.
    pub package: String,
    /// The file, relative to the workspace root.
    pub path: String,
    /// The message.
    pub message: String,
    /// The next steps.
    pub actions: Vec<Action>,
}

/// A package that the check skips, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema)]
#[schemars(rename = "FilesSkippedPackage")]
pub struct SkippedPackage {
    /// The package name.
    pub package: String,
    /// Why the syntax cannot decide the files of the package.
    pub reason: String,
}

/// The summary of a `files` run.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[schemars(rename = "FilesSummary")]
pub struct Summary {
    /// The workspace members.
    pub packages: usize,
    /// The `.rs` files in the target folders.
    pub files: usize,
    /// The files that a target compiles.
    pub reached_files: usize,
    /// The packages that the check skips.
    pub skipped_packages: Vec<SkippedPackage>,
}

/// Runs the `files` command.
pub fn run(args: &FilesArgs) -> Result<Section, CliError> {
    let progress = Progress::new(args.common.quiet);
    progress.step("Reading cargo metadata");
    let workspace = workspace::load(
        args.common.manifest_path.as_deref(),
        &args.common.cargo.flags(),
    )?;
    let scope = Scope::load(&workspace, &args.common.scope)?;
    scope.annotate(section(
        &workspace,
        args.fail_on_issues,
        &scope,
        progress,
        None,
    )?)
}

/// Finds the unused files of the workspace. `enforced` is true when a finding
/// fails the run. `scans` holds the scans of an earlier parse, for example of
/// the module graph. The walk parses each other file itself.
pub fn section(
    workspace: &Workspace,
    enforced: bool,
    scope: &Scope,
    progress: Progress,
    scans: Option<FileScans>,
) -> Result<Section, CliError> {
    let settings: Settings = config::section(workspace, "files")?;
    let _phase = progress.phase("Walking the module trees");
    let root = workspace.root.as_std_path();
    let packages = Packages::new(workspace);
    let reached = reach(root, workspace, &packages, scans.unwrap_or_default());
    let candidates = candidates(root, workspace);
    let reached_folders: BTreeSet<&str> = reached.files.iter().map(|path| parent(path)).collect();
    let mut findings = Vec::new();
    for (path, package) in &candidates {
        let unused = !reached.files.contains(path)
            && reached_folders.contains(parent(path))
            && !reached.skipped.contains_key(package.as_str())
            && !matches_prefix(path, &settings.exclude)
            && scope.keeps(path)
            && !generated(root, path);
        if unused {
            findings.push(Finding {
                rule: RULE,
                package: package.clone(),
                path: path.clone(),
                message: "No target compiles this file. No `mod` item, `#[path]` attribute or \
                          `include!` call reaches it."
                    .to_owned(),
                actions: vec![Action::manual(
                    "delete-file",
                    "Delete the file, or declare it with a `mod` item",
                )],
            });
        }
    }
    let summary = Summary {
        packages: workspace.members.len(),
        files: candidates.len(),
        reached_files: reached.files.len(),
        skipped_packages: reached
            .skipped
            .into_iter()
            .map(|(package, reason)| SkippedPackage { package, reason })
            .collect(),
    };
    let mut report = Report::new("files", summary, findings);
    let mut findings = shared_findings(&report);
    report.retain_baseline(scope, &mut findings);
    report.gate_outcomes.insert(
        Gate::UnusedFiles,
        count_gate(
            if scope.rules().counts(&[RULE], true) {
                report.findings.len()
            } else {
                0
            },
            enforced,
        ),
    );
    let rules = BTreeMap::from([(RULE.to_owned(), "No target compiles the file.".to_owned())]);
    Ok(Section {
        human: human(&report),
        json: to_value(&report)?,
        sarif: sarif::log(&rules, &findings, &report.gate_outcomes),
        findings,
        gates: report.gate_outcomes,
    })
}

/// The folder of each member, to find the package of a path.
struct Packages<'a> {
    /// The folder and the name of each member, the deepest folder first.
    folders: Vec<(String, &'a str)>,
}

impl<'a> Packages<'a> {
    fn new(workspace: &'a Workspace) -> Self {
        let mut folders: Vec<(String, &'a str)> = workspace
            .members
            .iter()
            .map(|member| {
                (
                    parent(&member.manifest_path).to_owned(),
                    member.name.as_str(),
                )
            })
            .collect();
        folders.sort_by_key(|(folder, _)| std::cmp::Reverse(folder.len()));
        Self { folders }
    }

    /// The package whose folder holds `path`.
    fn of(&self, path: &str) -> Option<&'a str> {
        self.folders
            .iter()
            .find(|(folder, _)| {
                folder.is_empty() || matches_prefix(path, std::slice::from_ref(folder))
            })
            .map(|(_, name)| *name)
    }
}

/// The files that a target reaches, and the packages that the check skips.
struct Reached {
    files: BTreeSet<String>,
    skipped: BTreeMap<String, String>,
}

fn reach(
    root: &Path,
    workspace: &Workspace,
    packages: &Packages<'_>,
    mut scans: FileScans,
) -> Reached {
    let roots: Vec<TargetRoot> = workspace
        .members
        .iter()
        .flat_map(|member| &member.targets)
        .map(|target| TargetRoot {
            path: target.src_path.clone(),
            edition: parse_edition(&target.edition),
            test_only: false,
        })
        .collect();
    let mut skipped = BTreeMap::new();
    let mut files: BTreeSet<String> = BTreeSet::new();
    let walked = walk(root, &roots, |visit| {
        // A scan from an earlier parse needs no read of the file.
        let found = match scans.remove(&(visit.path.clone(), visit.edition)) {
            Some(found) => found,
            None => scan_text(root, &visit.path, visit.edition)?,
        };
        let modules = found.modules.clone();
        Ok(((visit.path.clone(), found), modules))
    });
    let mut workspace_wide = None;
    for (path, found) in &walked.files {
        files.insert(path.clone());
        files.extend(found.includes.iter().cloned());
        files.extend(found.credited.iter().cloned());
        let reason = found.undecided.or_else(|| {
            found
                .code_includes
                .iter()
                .find_map(|file| included_reach(root, file))
        });
        if let (Some(reason), Some(package)) = (reason, packages.of(path)) {
            skipped
                .entry(package.to_owned())
                .or_insert_with(|| reason.to_owned());
        }
        if found.workspace_wide {
            workspace_wide = workspace_wide.or(found.undecided);
        }
    }
    // A macro can run in each crate of the workspace, so its reason skips
    // each package.
    if let Some(reason) = workspace_wide {
        for member in &workspace.members {
            skipped
                .entry(member.name.clone())
                .or_insert_with(|| reason.to_owned());
        }
    }
    for file in &walked.skipped {
        files.insert(file.path.clone());
        if let Some(package) = packages.of(&file.path) {
            skipped
                .entry(package.to_owned())
                .or_insert_with(|| format!("`{}` does not parse", file.path));
        }
    }
    Reached { files, skipped }
}

/// The scan of one file from its text.
fn scan_text(root: &Path, path: &str, edition: crate::parse::Edition) -> Result<Scan, String> {
    let source = fs::read_to_string(root.join(path))
        .map_err(|err| format!("cannot read the file: {err}"))?;
    if skips_the_parse(
        &source,
        is_generated(&source) || has_generated_marker(&source),
    ) {
        return Ok(Scan::default());
    }
    with_parsed(&source, edition, |file, _| scan::scan(file, path)).map_err(parse_reason)
}

/// True when the scan of `source` finds nothing without a parse. A generator
/// can write a file with millions of tokens. When its text cannot hold a
/// module, an include or a `.rs` literal, the scan finds nothing, so the walk
/// does not parse it. `generated` tells if the text has a generated marker.
pub fn skips_the_parse(source: &str, generated: bool) -> bool {
    generated && scan::reaches_nothing(source)
}

/// The reason when an included `.rs` file reaches more files: its modules and
/// includes resolve from the including file, and the walk does not follow
/// them.
fn included_reach(root: &Path, path: &str) -> Option<&'static str> {
    if !is_rust_file(path) {
        return None;
    }
    let source = fs::read_to_string(root.join(path)).ok()?;
    let edition = crate::parse::Edition::CURRENT;
    let found = with_parsed(&source, edition, |file, _| scan::scan(file, path));
    match found {
        Ok(found) if found.modules.is_empty() && found.includes.is_empty() => found.undecided,
        Ok(_) => Some("an included file declares a module or includes a file"),
        Err(_) => Some("an included file does not parse"),
    }
}

/// The `.rs` files in the target folders of each member, with their package.
pub fn target_files(root: &Path, workspace: &Workspace) -> BTreeMap<String, String> {
    candidates(root, workspace)
}

/// The `.rs` files in the target folders of each member, with their package.
fn candidates(root: &Path, workspace: &Workspace) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for member in &workspace.members {
        let package = parent(&member.manifest_path);
        let mut folders: BTreeSet<String> = TARGET_FOLDERS
            .iter()
            .filter_map(|folder| normalize(&Path::new(package).join(folder)))
            .collect();
        // A target root in the package folder itself, for example `build.rs`,
        // adds no folder: the package folder holds other packages and tools.
        folders.extend(
            member
                .targets
                .iter()
                .map(|target| parent(&target.src_path).to_owned())
                .filter(|folder| {
                    folder != package && matches_prefix(folder, &[package.to_owned()])
                }),
        );
        let mut files = BTreeSet::new();
        for folder in &folders {
            list(root, folder, &mut files);
        }
        found.extend(files.into_iter().map(|path| (path, member.name.clone())));
    }
    found
}

/// Lists the `.rs` files under `folder`, without the folders that hold
/// another package, test data, build output or hidden files.
fn list(root: &Path, folder: &str, into: &mut BTreeSet<String>) {
    let Ok(entries) = fs::read_dir(root.join(folder)) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(path) = normalize(&Path::new(folder).join(&name)) else {
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            let skip = name.starts_with('.')
                || name == "target"
                || FIXTURE_FOLDERS.contains(&name.as_str())
                || root.join(&path).join("Cargo.toml").is_file();
            if !skip {
                list(root, &path, into);
            }
        } else if !kind.is_symlink() && is_rust_file(&name) {
            into.insert(path);
        }
    }
}

/// True for a `.rs` file name. Cargo and rustc use the lower-case extension
/// only.
fn is_rust_file(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|extension| extension == "rs")
}

/// True when a generator wrote the file.
fn generated(root: &Path, path: &str) -> bool {
    fs::read_to_string(root.join(path))
        .is_ok_and(|text| is_generated(&text) || has_generated_marker(&text))
}

/// The folder of a path with `/` separators, or `""` at the top.
fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(folder, _)| folder)
}

fn shared_findings(report: &Report<Summary, Finding>) -> Vec<shared::Finding> {
    report
        .findings
        .iter()
        .map(|finding| {
            shared::Finding::new(Draft {
                check: "files",
                rule: finding.rule,
                severity: Severity::Error,
                message: finding.message.clone(),
                location: Location {
                    path: finding.path.clone(),
                    line: None,
                    column: None,
                    end_line: None,
                },
                identity: &[&finding.path],
                category: Category::Clarity,
                impact: Impact::Minor,
            })
            .with_actions(shared_actions(&finding.actions))
        })
        .collect()
}

fn human(report: &Report<Summary, Finding>) -> String {
    let summary = &report.summary;
    let mut text = format!(
        "cargo fallow files: {} {}, {} {} in the target folders, {} unused\n",
        summary.packages,
        plural(summary.packages, "package", "packages"),
        summary.files,
        plural(summary.files, "file", "files"),
        report.findings.len(),
    );
    if report.findings.is_empty() {
        text.push_str("No unused files\n");
    }
    for finding in &report.findings {
        let _ = writeln!(text, "  {}: {}", finding.rule, finding.path);
    }
    for skipped in &summary.skipped_packages {
        let _ = writeln!(
            text,
            "  skipped package {}: {}",
            skipped.package, skipped.reason
        );
    }
    text.push_str(&gates_text(&report.gate_outcomes));
    text
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use super::*;
    use crate::graph::{self, Production};
    use crate::modules::ModuleDecl;
    use crate::parse::Edition;

    fn ok<T, E: Debug>(result: Result<T, E>, what: &str) -> T {
        match result {
            Ok(value) => value,
            Err(err) => panic!("{what}: {err:?}"),
        }
    }

    /// A package with `files` written into a temporary folder.
    fn package(files: &[(&str, &str)]) -> (tempfile::TempDir, Workspace) {
        let dir = ok(tempfile::tempdir(), "a temporary directory");
        let manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                        publish = false\n\n[workspace]\n";
        for (path, text) in std::iter::once(&("Cargo.toml", manifest)).chain(files) {
            let full = dir.path().join(path);
            if let Some(parent) = full.parent() {
                ok(fs::create_dir_all(parent), "a folder");
            }
            ok(fs::write(full, text), "a file");
        }
        let workspace = ok(
            workspace::load(Some(&dir.path().join("Cargo.toml")), &[]),
            "the workspace",
        );
        (dir, workspace)
    }

    #[test]
    fn the_graph_parse_gives_the_scan_of_each_file() {
        let lib = "mod a;\nmacro_rules! m { () => {} }\nm! { mod b; }\n\
                   const T: &str = include_str!(\"t.txt\");\n";
        let (_dir, workspace) = package(&[
            ("src/lib.rs", lib),
            ("src/a.rs", "pub fn f() {}\n"),
            ("src/t.txt", ""),
        ]);
        let (_, scans) = graph::build_with_scans(&workspace, &Production::default());
        let edition = Edition::Edition2024;
        let mut paths: Vec<&str> = scans.keys().map(|(path, _)| path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(paths, ["src/a.rs", "src/lib.rs"]);
        let own = ok(
            with_parsed(lib, edition, |file, _| scan_file(file, "src/lib.rs")),
            "the parse",
        );
        assert_eq!(scans.get(&("src/lib.rs".to_owned(), edition)), Some(&own));
        assert_eq!(own.includes, ["src/t.txt"]);
        assert!(own.modules.iter().any(|module| module.name == "b"));
    }

    #[test]
    fn the_walk_finds_the_stored_scans_of_the_graph_parse() {
        // The bin target has another edition and reaches `shared.rs` too.
        let manifest_bin =
            "[[bin]]\nname = \"tool\"\npath = \"src/bin/tool.rs\"\nedition = \"2018\"\n";
        let (dir, _) = package(&[
            ("src/lib.rs", "mod shared;\nmod tables;\n"),
            ("src/shared.rs", ""),
            ("src/extra.rs", ""),
            (
                "src/tables.rs",
                "// @generated by a tool\npub static W: &[&str] = &[\"a\"];\n",
            ),
            (
                "src/bin/tool.rs",
                "#[path = \"../shared.rs\"]\nmod shared;\n",
            ),
        ]);
        let manifest = dir.path().join("Cargo.toml");
        let text = ok(fs::read_to_string(&manifest), "the manifest");
        ok(
            fs::write(&manifest, format!("{text}{manifest_bin}")),
            "the manifest",
        );
        let workspace = ok(workspace::load(Some(&manifest), &[]), "the workspace");
        let (_, mut scans) = graph::build_with_scans(&workspace, &Production::default());
        for edition in [Edition::Edition2024, Edition::Edition2018] {
            assert!(
                scans.contains_key(&("src/shared.rs".to_owned(), edition)),
                "{edition:?}: {:?}",
                scans.keys()
            );
        }
        // The walk gives no parse to a generated file that reaches nothing.
        let generated = ("src/tables.rs".to_owned(), Edition::Edition2024);
        assert_eq!(scans.get(&generated), Some(&Scan::default()));
        // A stored scan with one more module reaches `extra.rs`, so the walk
        // reads the stored scans by their real keys.
        let lib = ("src/lib.rs".to_owned(), Edition::Edition2024);
        let Some(scan) = scans.get_mut(&lib) else {
            panic!("no scan of lib.rs: {:?}", scans.keys());
        };
        scan.modules.push(ModuleDecl {
            name: "extra".to_owned(),
            inline_path: Vec::new(),
            path_attr: None,
            under_cfg: false,
            cfg_off: false,
            test_only: false,
        });
        let reached = reach(dir.path(), &workspace, &Packages::new(&workspace), scans);
        assert!(
            reached.files.contains("src/extra.rs"),
            "{:?}",
            reached.files
        );
    }

    #[test]
    fn the_walk_uses_a_given_scan_and_does_not_parse_the_file() {
        // The text of `lib.rs` declares no module. The given scan does, so the
        // walk reaches `extra.rs` only when it reads the scan.
        let (dir, workspace) = package(&[("src/lib.rs", ""), ("src/extra.rs", "")]);
        let scan = Scan {
            modules: vec![ModuleDecl {
                name: "extra".to_owned(),
                inline_path: Vec::new(),
                path_attr: None,
                under_cfg: false,
                cfg_off: false,
                test_only: false,
            }],
            ..Scan::default()
        };
        let scans = FileScans::from_iter([(("src/lib.rs".to_owned(), Edition::Edition2024), scan)]);
        let packages = Packages::new(&workspace);
        let reached = reach(dir.path(), &workspace, &packages, scans);
        assert!(
            reached.files.contains("src/extra.rs"),
            "{:?}",
            reached.files
        );
        let reached = reach(dir.path(), &workspace, &packages, FileScans::default());
        assert!(
            !reached.files.contains("src/extra.rs"),
            "{:?}",
            reached.files
        );
    }
}
