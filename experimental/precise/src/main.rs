//! The `--precise` prototype of the M2 gap measurement.
//!
//! The fast check of `cargo fallow visibility` lists in `undecided` the
//! findings that only an untyped credit stops: a method call, a field access,
//! or a name in macro tokens or in an attribute. This program loads the
//! workspace into rust-analyzer, with build scripts and proc-macros, and finds
//! the uses of each undecided item with name resolution and type inference.
//!
//! Usage: `cargo-fallow-precise <workspace Cargo.toml> <fast report JSON>`
//!
//! The output is JSON on stdout: the undecided rows that rust-analyzer
//! confirms as findings, with their kind, and the time of each phase.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use ra_ap_hir::{Crate, Semantics};
use ra_ap_ide_db::RootDatabase;
use ra_ap_ide_db::base_db::SourceDatabase;
use ra_ap_ide_db::defs::{Definition, NameClass};
use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace_at};
use ra_ap_paths::AbsPathBuf;
use ra_ap_project_model::{CargoConfig, CargoFeatures, RustLibSource};
use ra_ap_syntax::{AstNode, SyntaxKind, TextSize, ast};
use ra_ap_vfs::{Vfs, VfsPath};
use serde::{Deserialize, Serialize};

/// One row of the fast report.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct Row {
    kind: String,
    package: String,
    #[serde(rename = "crate")]
    krate: String,
    item: String,
    item_kind: String,
    file: String,
    line: u32,
    column: u32,
    end_line: u32,
    test_only: bool,
}

#[derive(Debug, Deserialize)]
struct FastReport {
    undecided: Vec<Row>,
}

#[derive(Debug, Serialize)]
struct Output {
    /// The undecided rows that rust-analyzer confirms, with the kind that its
    /// uses give.
    decided: Vec<Row>,
    /// The undecided rows that another crate uses, so they stay `pub`.
    kept: usize,
    /// The rows that rust-analyzer cannot find in its database.
    unresolved: Vec<String>,
    /// The seconds to load the workspace, with build scripts and proc-macros.
    load_seconds: f64,
    /// The seconds to search the uses.
    search_seconds: f64,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, manifest, report] = args.as_slice() else {
        bail!("usage: cargo-fallow-precise <Cargo.toml> <fast report JSON>");
    };
    let manifest = std::fs::canonicalize(manifest).context("read the manifest path")?;
    let root = manifest
        .parent()
        .context("the manifest has a folder")?
        .to_path_buf();
    let fast: FastReport = serde_json::from_str(&std::fs::read_to_string(report)?)?;

    let start = Instant::now();
    let cargo = CargoConfig {
        all_targets: true,
        features: CargoFeatures::All,
        sysroot: Some(RustLibSource::Discover),
        set_test: true,
        ..CargoConfig::default()
    };
    let load = LoadCargoConfig {
        load_out_dirs_from_check: true,
        with_proc_macro_server: ProcMacroServerChoice::Sysroot,
        prefill_caches: false,
        num_worker_threads: 2,
        proc_macro_processes: 1,
    };
    let (db, vfs, _server) = load_workspace_at(&manifest, &cargo, &load, &|_| {})?;
    let load_seconds = start.elapsed().as_secs_f64();

    let start = Instant::now();
    let sema = Semantics::new(&db);
    let mut output = Output {
        decided: Vec::new(),
        kept: 0,
        unresolved: Vec::new(),
        load_seconds,
        search_seconds: 0.0,
    };
    // Type inference reads the database through a thread-local handle.
    ra_ap_hir::attach_db(&db, || {
        for row in fast.undecided {
            match decide(&sema, &db, &vfs, &root, &row) {
                Some(Some(kind)) => output.decided.push(Row { kind, ..row }),
                Some(None) => output.kept += 1,
                None => output
                    .unresolved
                    .push(format!("{}:{} {}", row.file, row.line, row.item)),
            }
        }
    });
    output.search_seconds = start.elapsed().as_secs_f64();
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// The finding kind that the uses give, `Some(None)` when another crate uses
/// the item, or `None` when the item is not in the database.
fn decide<'db>(
    sema: &Semantics<'db, RootDatabase>,
    db: &'db RootDatabase,
    vfs: &Vfs,
    root: &Path,
    row: &Row,
) -> Option<Option<String>> {
    let definition = definition_at(sema, db, vfs, &root.join(&row.file), row.line, row.column)?;
    let home = definition.krate(db)?;
    let mut same_crate = false;
    for (file, references) in definition.usages(sema).all() {
        let crates: Vec<Crate> = sema
            .file_to_module_defs(file.file_id(db))
            .map(|module| module.krate(db))
            .collect();
        if references.is_empty() {
            continue;
        }
        if crates.iter().any(|krate| *krate != home) {
            return Some(None);
        }
        same_crate = true;
    }
    let restricted = row.kind == "unnecessary_restricted_visibility";
    Some(Some(if restricted {
        row.kind.clone()
    } else if same_crate {
        "unnecessary_public".to_owned()
    } else {
        "dead_public".to_owned()
    }))
}

/// The definition whose name starts at `line` and `column` (one-based, in
/// characters).
fn definition_at<'db>(
    sema: &Semantics<'db, RootDatabase>,
    db: &'db RootDatabase,
    vfs: &Vfs,
    path: &PathBuf,
    line: u32,
    column: u32,
) -> Option<Definition<'db>> {
    let abs = AbsPathBuf::assert_utf8(path.clone());
    let (file_id, _) = vfs.file_id(&VfsPath::from(abs))?;
    let text = db.file_text(file_id).text(db);
    let offset = offset_of(&text, line, column)?;
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
