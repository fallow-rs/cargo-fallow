//! The rust-analyzer side of `--precise` mode. Only the worker process runs
//! this code.
//!
//! The engine loads the workspace with build scripts and proc-macros, and then
//! finds the uses of each candidate with name resolution and type inference.
//! The search covers the workspace files up to the size limit. A file over the
//! limit gets a plain text search: when it holds the name, the candidate keeps
//! its visibility, because the engine cannot see what the name means there.

use std::path::Path;
use std::time::{Duration, Instant};

use ra_ap_hir::{Crate, Semantics};
use ra_ap_ide_db::RootDatabase;
use ra_ap_ide_db::base_db::{
    CrateOrigin, EditionedFileId, ProcMacroLoadingError, SourceDatabase, all_crates,
};
use ra_ap_ide_db::defs::{Definition, NameClass};
use ra_ap_ide_db::search::{FileReference, SearchScope};
use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace};
use ra_ap_paths::{AbsPathBuf, Utf8PathBuf};
use ra_ap_project_model::{
    CargoConfig, CargoFeatures, ProjectManifest, ProjectWorkspace, RustLibSource,
    TargetDirectoryConfig,
};
use ra_ap_syntax::{AstNode, SyntaxKind, SyntaxNode, TextSize, ast};
use ra_ap_vfs::{FileId, Vfs, VfsPath};

use super::{Candidate, Decision, Event, EventSink, Fault, KeepReason, LoadProblem, Request};

/// The worker threads of the load. The machine limit of two parallel jobs
/// applies.
const LOAD_THREADS: usize = 2;

/// The files of the search.
struct Files {
    /// The workspace files up to the size limit.
    scope: SearchScope,
    /// The text of each workspace file over the size limit.
    large: Vec<String>,
}

/// Loads the workspace and decides each candidate. Each decision goes to the
/// sink at once.
pub fn run(request: &Request, sink: &mut EventSink) -> Result<(), String> {
    let start = Instant::now();
    let manifest = abs_path(&request.manifest).ok_or_else(|| {
        format!(
            "the manifest path is not absolute UTF-8: {}",
            request.manifest.display()
        )
    })?;
    let fault = Fault::from_env();
    // A test fault points rust-analyzer at an empty folder as the source of
    // the standard library. The folder must live until the load ends.
    let empty_source = match fault {
        Some(Fault::NoStdSource) => {
            Some(tempfile::tempdir().map_err(|err| format!("create a temporary folder: {err}"))?)
        }
        _ => None,
    };
    let cargo = CargoConfig {
        all_targets: true,
        features: CargoFeatures::All,
        sysroot: Some(RustLibSource::Discover),
        sysroot_src: empty_source
            .as_ref()
            .and_then(|folder| abs_path(folder.path())),
        set_test: true,
        // The `cargo check` of the load uses its own target folder, so it does
        // not wait for the build lock of the user's target folder.
        target_dir_config: TargetDirectoryConfig::UseSubdirectory,
        ..CargoConfig::default()
    };
    let load = LoadCargoConfig {
        load_out_dirs_from_check: true,
        with_proc_macro_server: if fault == Some(Fault::NoProcMacroServer) {
            ProcMacroServerChoice::None
        } else {
            ProcMacroServerChoice::Sysroot
        },
        prefill_caches: false,
        num_worker_threads: LOAD_THREADS,
        proc_macro_processes: 1,
    };
    // The steps of `load_workspace_at`, so that the error of `cargo check`
    // stays visible. rust-analyzer only logs it.
    let project = ProjectManifest::discover_single(manifest.as_ref())
        .map_err(|err| format!("find the workspace: {err:#}"))?;
    let mut workspace = ProjectWorkspace::load(project, &cargo, &|_| {})
        .map_err(|err| format!("load the workspace: {err:#}"))?;
    let scripts = workspace
        .run_build_scripts(&cargo, &|_| {})
        .map_err(|err| format!("run the build scripts: {err:#}"))?;
    let build_error = scripts.error().map(str::to_owned);
    workspace.set_build_scripts(scripts);
    let (db, vfs, _server) = load_workspace(workspace, &cargo.extra_env, &load)
        .map_err(|err| format!("load the workspace: {err:#}"))?;
    // A missing part of the load is not an error of the load: rust-analyzer
    // stores it for each crate. Without it, uses stay hidden, and a decision
    // can report an item that code uses.
    if let Some((problem, detail)) = load_problem(&db, build_error.as_deref()) {
        sink.send(&Event::LoadFailed {
            problem,
            detail: detail.clone(),
        })?;
        return Err(detail);
    }
    sink.send(&Event::Loaded {
        ms: millis(start.elapsed()),
    })?;
    let search_start = Instant::now();
    let deadline = start + Duration::from_millis(request.deadline_ms);
    let sema = Semantics::new(&db);
    let files = files(&sema, &db, &vfs, request);
    let large_files = files.large.len();
    // Type inference reads the database through a thread-local handle.
    ra_ap_hir::attach_db(&db, || -> Result<(), String> {
        for candidate in &request.candidates {
            if Instant::now() >= deadline {
                return sink.send(&Event::TimeLimit);
            }
            let decision = decide(&sema, &db, &vfs, &request.root, &files, candidate);
            sink.send(&Event::Decided {
                id: candidate.id,
                decision,
            })?;
        }
        Ok(())
    })?;
    sink.send(&Event::Done {
        ms: millis(search_start.elapsed()),
        large_files,
    })
}

/// The first part of the load that is missing: the source of the standard
/// library, or the expansion of a proc-macro crate. `build_error` is the error
/// of the `cargo check` that builds the proc-macro crates and runs the build
/// scripts.
fn load_problem(db: &RootDatabase, build_error: Option<&str>) -> Option<(LoadProblem, String)> {
    let crates = all_crates(db);
    let has_std_source = crates
        .iter()
        .any(|krate| matches!(krate.data(db).origin, CrateOrigin::Lang(_)));
    if !has_std_source {
        return Some((
            LoadProblem::MissingStdSource,
            "rust-analyzer found no source of the standard library".to_owned(),
        ));
    }
    crates.iter().find_map(|&krate| {
        if !krate.data(db).is_proc_macro {
            return None;
        }
        let error = ra_ap_hir::ProcMacros::get_for_crate(db, krate)
            .as_ref()
            .and_then(|macros| macros.get_error())
            // A successful `cargo check` builds each proc-macro crate that the
            // build needs, so a crate that it did not build expands no code.
            // After a failed `cargo check`, a needed crate can be missing.
            .filter(|error| {
                !matches!(error, ProcMacroLoadingError::NotYetBuilt) || build_error.is_some()
            })?;
        let name = krate
            .extra_data(db)
            .display_name
            .as_ref()
            .map_or_else(|| "a crate".to_owned(), |name| format!("`{name}`"));
        let cause = build_error.map_or_else(String::new, |text| {
            format!(". `cargo check --all-targets --all-features` failed: {text}")
        });
        Some((
            LoadProblem::ProcMacro,
            format!("rust-analyzer cannot expand the proc-macro crate {name}: {error}{cause}"),
        ))
    })
}

/// The path as an absolute UTF-8 path, or `None`.
fn abs_path(path: &Path) -> Option<AbsPathBuf> {
    let utf8 = Utf8PathBuf::from_path_buf(path.to_path_buf()).ok()?;
    AbsPathBuf::try_from(utf8).ok()
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The workspace files: the source files under the root, outside the target
/// folder.
fn files(
    sema: &Semantics<'_, RootDatabase>,
    db: &RootDatabase,
    vfs: &Vfs,
    request: &Request,
) -> Files {
    let mut small: Vec<EditionedFileId> = Vec::new();
    let mut large = Vec::new();
    for (file_id, path) in vfs.iter() {
        let Some(path) = path.as_path() else {
            continue;
        };
        let path: &Path = path.as_ref();
        if !path.starts_with(&request.root) || path.starts_with(&request.target_dir) {
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let text = db.file_text(file_id).text(db);
        if text.len() as u64 > request.file_limit_bytes {
            large.push(text.to_string());
        } else {
            small.push(sema.attach_first_edition(file_id));
        }
    }
    Files {
        scope: SearchScope::files(&small),
        large,
    }
}

/// The decision for one candidate.
fn decide<'db>(
    sema: &Semantics<'db, RootDatabase>,
    db: &'db RootDatabase,
    vfs: &Vfs,
    root: &Path,
    files: &Files,
    candidate: &Candidate,
) -> Decision {
    let path = root.join(&candidate.file);
    let Some(definition) = definition_at(sema, db, vfs, &path, candidate.line, candidate.column)
    else {
        return Decision::Unresolved;
    };
    let Some(home) = definition.krate(db) else {
        return Decision::Unresolved;
    };
    let restricted = candidate.kind == "unnecessary-restricted-visibility";
    let same_crate = match scan_usages(sema, db, &definition, home, &files.scope, restricted) {
        Ok(same_crate) => same_crate,
        Err(reason) => return Decision::Keep { reason },
    };
    if files
        .large
        .iter()
        .any(|text| contains_word(text, &candidate.name))
    {
        return Decision::Keep {
            reason: KeepReason::LargeFile,
        };
    }
    let kind = if restricted {
        candidate.kind.clone()
    } else if same_crate {
        "unnecessary-public".to_owned()
    } else {
        "dead-public".to_owned()
    };
    Decision::Report { kind }
}

/// Scans the uses of the definition in the search scope. The result is the
/// reason to keep the visibility, or else true when the home crate uses the
/// definition.
fn scan_usages<'db>(
    sema: &Semantics<'db, RootDatabase>,
    db: &'db RootDatabase,
    definition: &Definition<'db>,
    home: Crate,
    scope: &SearchScope,
    restricted: bool,
) -> Result<bool, KeepReason> {
    let home_module = definition.module(db);
    let mut same_crate = false;
    for (file, references) in definition.usages(sema).in_scope(scope).all() {
        if references.is_empty() {
            continue;
        }
        let mut crates = sema
            .file_to_module_defs(file.file_id(db))
            .map(|module| module.krate(db));
        if crates.any(|krate| krate != home) {
            return Err(KeepReason::OtherCrate);
        }
        same_crate = true;
        for reference in &references {
            if let Some(reason) = reference_keep(sema, db, reference, restricted, home_module) {
                return Err(reason);
            }
        }
    }
    Ok(same_crate)
}

/// The reason to keep the visibility because of one reference, if any.
fn reference_keep(
    sema: &Semantics<'_, RootDatabase>,
    db: &RootDatabase,
    reference: &FileReference,
    restricted: bool,
    home_module: Option<ra_ap_hir::Module>,
) -> Option<KeepReason> {
    let node = reference.name.syntax().parent()?;
    if !restricted {
        return in_public_signature(&node).then_some(KeepReason::PublicSignature);
    }
    let scope = sema.scope(&node)?;
    let home_module = home_module?;
    (!within(db, scope.module(), home_module)).then_some(KeepReason::OutsideModule)
}

/// True when `name` is in `text` as a whole identifier.
fn contains_word(text: &str, name: &str) -> bool {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(name).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + name.len()..].chars().next();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}

/// True when `module` is `home` or a module below it.
fn within(db: &RootDatabase, module: ra_ap_hir::Module, home: ra_ap_hir::Module) -> bool {
    let mut current = Some(module);
    while let Some(next) = current {
        if next == home {
            return true;
        }
        current = next.parent(db);
    }
    false
}

/// True when the node is in the signature of an item with a `pub` keyword: a
/// field type, a parameter, a return type or a bound, not a body. A field of an
/// enum variant has the visibility of the enum. An item of a trait impl counts
/// as public, because the compiler checks it against the type and the trait.
fn in_public_signature(node: &SyntaxNode) -> bool {
    let mut in_variant = false;
    for ancestor in node.ancestors() {
        match ancestor.kind() {
            SyntaxKind::BLOCK_EXPR | SyntaxKind::STMT_LIST => return false,
            SyntaxKind::VARIANT => in_variant = true,
            SyntaxKind::RECORD_FIELD | SyntaxKind::TUPLE_FIELD if !in_variant => {
                return written_pub(&ancestor);
            }
            SyntaxKind::ENUM if in_variant => return written_pub(&ancestor),
            SyntaxKind::FN | SyntaxKind::TYPE_ALIAS | SyntaxKind::CONST
                if in_trait_impl(&ancestor) =>
            {
                return true;
            }
            SyntaxKind::FN
            | SyntaxKind::TYPE_ALIAS
            | SyntaxKind::CONST
            | SyntaxKind::STATIC
            | SyntaxKind::TRAIT => return written_pub(&ancestor),
            SyntaxKind::IMPL => {
                return ast::Impl::cast(ancestor).is_some_and(|block| block.trait_().is_some());
            }
            _ => {}
        }
    }
    false
}

fn written_pub(node: &SyntaxNode) -> bool {
    node.children()
        .find(|child| child.kind() == SyntaxKind::VISIBILITY)
        .is_some_and(|vis| vis.text() == "pub")
}

/// True for an item in the body of an `impl Trait for Type` block.
fn in_trait_impl(item: &SyntaxNode) -> bool {
    item.parent()
        .filter(|list| list.kind() == SyntaxKind::ASSOC_ITEM_LIST)
        .and_then(|list| list.parent())
        .and_then(ast::Impl::cast)
        .is_some_and(|block| block.trait_().is_some())
}

/// The definition whose name starts at `line` and `column` (one-based, in
/// characters).
fn definition_at<'db>(
    sema: &Semantics<'db, RootDatabase>,
    db: &'db RootDatabase,
    vfs: &Vfs,
    path: &Path,
    line: u32,
    column: u32,
) -> Option<Definition<'db>> {
    let abs = abs_path(path)?;
    let (file_id, _) = vfs.file_id(&VfsPath::from(abs))?;
    let offset = offset_of(db.file_text(file_id).text(db), line, column)?;
    name_at(sema, file_id, offset)
}

fn name_at<'db>(
    sema: &Semantics<'db, RootDatabase>,
    file_id: FileId,
    offset: usize,
) -> Option<Definition<'db>> {
    let file = sema.parse_guess_edition(file_id);
    let token = file
        .syntax()
        .token_at_offset(TextSize::from(u32::try_from(offset).ok()?))
        .find(|token| token.kind() == SyntaxKind::IDENT)?;
    let name = token.parent_ancestors().find_map(ast::Name::cast)?;
    match NameClass::classify(sema, &name)? {
        NameClass::Definition(definition) => Some(definition),
        _ => None,
    }
}

/// The byte offset of a one-based line and column in characters.
fn offset_of(text: &str, line: u32, column: u32) -> Option<usize> {
    let start: usize = text
        .split_inclusive('\n')
        .take(line.checked_sub(1)? as usize)
        .map(str::len)
        .sum();
    let rest = text.get(start..)?;
    let within: usize = rest
        .chars()
        .take(column.checked_sub(1)? as usize)
        .map(char::len_utf8)
        .sum();
    Some(start + within)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_match_needs_a_boundary_on_each_side() {
        assert!(contains_word("let a = word;", "word"));
        assert!(contains_word("word", "word"));
        assert!(!contains_word("words wordy _word", "word"));
        assert!(contains_word("\"é\" word", "word"));
    }

    #[test]
    fn an_offset_counts_characters_in_the_line() {
        assert_eq!(offset_of("ab\nçd\n", 2, 2), Some(5));
        assert_eq!(offset_of("ab\n", 0, 1), None);
    }
}
