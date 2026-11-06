//! Black-box tests of the item rules and the glob re-export rules of
//! `cargo fallow boundaries`.
//!
//! Each test writes a small workspace to a temporary directory: an `engine`
//! crate with a denied function, a `session` module that re-exports it, a
//! `types` crate, an `api` facade and a `cli` crate that uses them.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

const ENGINE: &str = "pub mod dead_code {
    pub fn analyze() {}
    pub struct T;
    impl T {
        pub fn new() -> Self {
            T
        }
    }
}
pub mod session {
    pub use crate::dead_code::analyze;
    pub fn run() {
        crate::dead_code::analyze();
    }
}
#[macro_export]
macro_rules! shout {
    () => {};
}
";

const TYPES: &str = "pub mod extract {
    pub struct Item;
}
pub struct Root;
";

const API: &str = "pub use types::*;
use types as t;
pub use t::extract::*;
use types::extract as private_glob;
use private_glob::*;
pub use other::*;
mod other {
    pub struct Local;
}
";

const CLI: &str = "use engine::dead_code;
use engine as core_alias;
use engine::dead_code::analyze as run_it;

pub fn direct() {
    engine::dead_code::analyze();
}

pub fn via_module() {
    dead_code::analyze();
}

pub fn renamed() {
    run_it();
}

pub fn via_session() {
    engine::session::analyze();
    engine::session::run();
}

mod hop {
    use engine::session::analyze;
    pub fn call() {
        analyze();
    }
}

pub fn aliased() {
    core_alias::dead_code::analyze();
}

/// See [`engine::dead_code::analyze`].
pub fn documented() {}

pub trait Maker {
    fn new() -> Self;
}

pub fn make<T: Maker>() -> T {
    T::new()
}

pub struct Runner;
impl Runner {
    pub fn analyze(&self) {}
}

pub fn method(runner: &Runner) {
    runner.analyze();
}

macro_rules! call {
    () => {
        engine::dead_code::analyze()
    };
}

pub fn macro_user() {
    call!();
}

#[cfg(test)]
mod tests {
    #[test]
    fn calls() {
        engine::dead_code::analyze();
    }
}
";

const RULES: &str = r#"
[[workspace.metadata.fallow.boundaries.items]]
rule = "cli-runs-dead-code-through-the-session"
from = ["cli"]
deny = ["engine::dead_code::analyze", "engine::dead_code::T"]
allow-via = ["engine::session"]

[[workspace.metadata.fallow.boundaries.reexports]]
rule = "no-glob-reexport-of-types"
from = ["api"]
deny = ["types"]
"#;

struct Demo {
    dir: TempDir,
}

impl Demo {
    fn new(rules: &str) -> Self {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let root = dir.path();
        fs::write(
            root.join("Cargo.toml"),
            format!(
                "[workspace]\nmembers = [\"engine\", \"types\", \"api\", \"cli\"]\nresolver = \"3\"\n{rules}"
            ),
        )
        .expect("write the root manifest");
        crate_at(root, "engine", &[], ENGINE);
        crate_at(root, "types", &[], TYPES);
        crate_at(root, "api", &["types"], API);
        crate_at(root, "cli", &["engine"], CLI);
        Self { dir }
    }

    fn json(&self, args: &[&str]) -> (i32, Value) {
        let output = Command::cargo_bin("cargo-fallow")
            .expect("find the binary")
            .arg("fallow")
            .args(args)
            .arg("--manifest-path")
            .arg(self.dir.path().join("Cargo.toml"))
            .args(["--quiet", "--format", "json"])
            .output()
            .expect("run cargo-fallow");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let value = serde_json::from_str(&stdout).expect("the output is JSON");
        (output.status.code().unwrap_or(-1), value)
    }
}

fn crate_at(root: &Path, name: &str, deps: &[&str], lib: &str) {
    let dir = root.join(name);
    fs::create_dir_all(dir.join("src")).expect("create src");
    let mut manifest = format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\n"
    );
    for dep in deps {
        let _ = writeln!(manifest, "{dep} = {{ path = \"../{dep}\" }}");
    }
    fs::write(dir.join("Cargo.toml"), manifest).expect("write a manifest");
    fs::write(dir.join("src/lib.rs"), lib).expect("write lib.rs");
}

/// The 1-based line of the first line of `text` that contains `needle`,
/// after the line that contains `after`.
fn line_of(text: &str, after: &str, needle: &str) -> u64 {
    let start = text
        .lines()
        .position(|line| line.contains(after))
        .expect("find the anchor");
    let index = text
        .lines()
        .enumerate()
        .skip(start)
        .find(|(_, line)| line.contains(needle))
        .map(|(index, _)| index)
        .expect("find the line");
    u64::try_from(index + 1).expect("a small line")
}

fn findings<'a>(report: &'a Value, kind: &str) -> Vec<&'a Value> {
    report["findings"]
        .as_array()
        .expect("a findings list")
        .iter()
        .filter(|finding| finding["kind"] == kind)
        .collect()
}

#[test]
fn item_rules_report_each_exact_use_of_a_denied_item() {
    let demo = Demo::new(RULES);
    let (code, report) = demo.json(&["boundaries"]);
    assert_eq!(code, 0, "{report:#}");
    let mut found: Vec<(u64, String, String)> = findings(&report, "item-path")
        .iter()
        .map(|finding| {
            assert_eq!(finding["path"], "cli/src/lib.rs");
            assert_eq!(finding["rule"], "cli-runs-dead-code-through-the-session");
            assert_eq!(finding["from"], "cli");
            (
                finding["line"].as_u64().unwrap_or_default(),
                finding["to"].as_str().unwrap_or_default().to_owned(),
                finding["origin"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    found.sort();
    let deny = "engine::dead_code::analyze".to_owned();
    let mut expected = vec![
        (
            line_of(CLI, "analyze as run_it", "run_it"),
            deny.clone(),
            "code".to_owned(),
        ),
        (
            line_of(CLI, "fn direct", "analyze"),
            deny.clone(),
            "code".to_owned(),
        ),
        (
            line_of(CLI, "fn via_module", "analyze"),
            deny.clone(),
            "code".to_owned(),
        ),
        (
            line_of(CLI, "fn aliased", "analyze"),
            deny.clone(),
            "code".to_owned(),
        ),
        (
            line_of(CLI, "macro_rules! call", "macro_rules! call"),
            deny.clone(),
            "macro".to_owned(),
        ),
        (line_of(CLI, "fn calls", "analyze"), deny, "code".to_owned()),
    ];
    expected.sort();
    assert_eq!(found, expected, "{report:#}");
    let summary = &report["summary"];
    assert_eq!(summary["item_rules"], 1);
    assert_eq!(summary["reexport_rules"], 1);
    // The generic `T` in `-> T` and in `T::new()`, and `runner.analyze()`, do
    // not resolve. They name a denied item, so they count but give no finding.
    assert_eq!(summary["unresolved"], 3, "{report:#}");
    assert_eq!(
        report["gate_outcomes"]["boundary-violations"]["enforced"],
        false
    );
    let (code, _) = demo.json(&["boundaries", "--fail-on-issues"]);
    assert_eq!(code, 1);
}

#[test]
fn a_glob_reexport_rule_follows_an_alias_and_skips_a_private_glob() {
    let demo = Demo::new(RULES);
    let (_, report) = demo.json(&["boundaries"]);
    let mut lines: Vec<u64> = findings(&report, "glob-reexport")
        .iter()
        .map(|finding| {
            assert_eq!(finding["path"], "api/src/lib.rs");
            assert_eq!(finding["to"], "types");
            finding["line"].as_u64().unwrap_or_default()
        })
        .collect();
    lines.sort_unstable();
    assert_eq!(lines, [1, 3], "{report:#}");
}

#[test]
fn a_deny_path_outside_the_workspace_or_to_a_macro_is_an_invalid_config() {
    for deny in ["serde::Serialize", "engine::shout"] {
        let rules = format!(
            "\n[[workspace.metadata.fallow.boundaries.items]]\nrule = \"r\"\nfrom = [\"cli\"]\ndeny = [\"{deny}\"]\n"
        );
        let (code, report) = Demo::new(&rules).json(&["boundaries"]);
        assert_eq!(code, 2, "{deny}: {report:#}");
        assert_eq!(report["code"], "CARGO_FALLOW_INVALID_CONFIG", "{deny}");
    }
}

#[test]
fn a_rule_without_allow_via_reports_the_session_path_too() {
    let rules = "\n[[workspace.metadata.fallow.boundaries.items]]\nrule = \"r\"\nfrom = [\"cli\"]\ndeny = [\"engine::dead_code::analyze\"]\n";
    let (_, report) = Demo::new(rules).json(&["boundaries"]);
    let lines: Vec<u64> = findings(&report, "item-path")
        .iter()
        .filter_map(|finding| finding["line"].as_u64())
        .collect();
    assert!(
        lines.contains(&line_of(CLI, "fn via_session", "analyze")),
        "{report:#}"
    );
    assert!(
        lines.contains(&line_of(CLI, "mod hop", "use engine::session")),
        "{report:#}"
    );
}

#[test]
fn a_removed_deny_item_gives_a_warning_and_no_error() {
    let rules = "\n[[workspace.metadata.fallow.boundaries.items]]\nrule = \"r\"\nfrom = [\"cli\"]\ndeny = [\"engine::nothing::here\"]\n";
    let (code, report) = Demo::new(rules).json(&["boundaries", "--fail-on-issues"]);
    assert_eq!(code, 0, "{report:#}");
    assert!(findings(&report, "item-path").is_empty(), "{report:#}");
}

#[test]
fn a_name_from_a_glob_of_an_allowed_import_stays_allowed() {
    let cli = format!(
        "{CLI}\npub mod facade {{\n    pub use engine::session::analyze;\n}}\nmod glob_user {{\n    use crate::facade::*;\n    pub fn call() {{\n        analyze();\n    }}\n}}\nmod nested {{\n    use engine::session::analyze;\n    #[cfg(test)]\n    mod tests {{\n        use super::*;\n        fn call() {{\n            analyze();\n        }}\n    }}\n}}\n"
    );
    let demo = Demo::new(RULES);
    fs::write(demo.dir.path().join("cli/src/lib.rs"), &cli).expect("write lib.rs");
    let (_, report) = demo.json(&["boundaries"]);
    let lines: Vec<u64> = findings(&report, "item-path")
        .iter()
        .filter_map(|finding| finding["line"].as_u64())
        .collect();
    let first_new = u64::try_from(CLI.lines().count()).expect("a small count");
    assert!(lines.iter().all(|line| *line <= first_new), "{report:#}");
}

/// Writes `cli/src/lib.rs` and returns the item-path findings as
/// `(line, to)`, with the report.
fn cli_findings(rules: &str, lib: &str) -> (Vec<u64>, Value) {
    let demo = Demo::new(rules);
    fs::write(demo.dir.path().join("cli/src/lib.rs"), lib).expect("write lib.rs");
    let (code, report) = demo.json(&["boundaries"]);
    assert_eq!(code, 0, "{report:#}");
    let lines = findings(&report, "item-path")
        .iter()
        .filter_map(|finding| finding["line"].as_u64())
        .collect();
    (lines, report)
}

#[test]
fn a_local_binding_that_shadows_a_glob_name_gives_no_finding() {
    let lib = "pub mod c {\n    use engine::dead_code::*;\n    pub fn f() {\n        let analyze = 1;\n        let _ = analyze;\n    }\n    pub fn g<T>() -> T {\n        T::new()\n    }\n}\n";
    let (lines, report) = cli_findings(RULES, lib);
    assert!(lines.is_empty(), "{report:#}");
    assert!(
        report["summary"]["unresolved"].as_u64() >= Some(2),
        "{report:#}"
    );
}

#[test]
fn a_glob_clique_ends_in_time() {
    let mut lib = String::new();
    for i in 0..8 {
        let _ = writeln!(lib, "pub mod m{i} {{");
        for j in 0..8 {
            if i != j {
                let _ = writeln!(lib, "    pub use crate::m{j}::*;");
            }
        }
        lib.push_str("    pub fn f() { analyze(); }\n}\n");
    }
    let started = std::time::Instant::now();
    let (lines, report) = cli_findings(RULES, &lib);
    assert!(lines.is_empty(), "{report:#}");
    assert!(started.elapsed() < std::time::Duration::from_secs(30));
}

#[test]
fn allow_via_names_a_module_only() {
    let rules = "\n[[workspace.metadata.fallow.boundaries.items]]\nrule = \"r\"\nfrom = [\"cli\"]\ndeny = [\"engine::dead_code::analyze\"]\nallow-via = [\"engine::session::analyze\"]\n";
    let (code, report) = Demo::new(rules).json(&["boundaries"]);
    assert_eq!(code, 2, "{report:#}");
    assert_eq!(report["code"], "CARGO_FALLOW_INVALID_CONFIG");
}

#[test]
fn a_glob_reexport_in_a_private_module_is_no_public_reexport() {
    let rules = "\n[[workspace.metadata.fallow.boundaries.reexports]]\nrule = \"g\"\nfrom = [\"api\"]\ndeny = [\"types\"]\n";
    let demo = Demo::new(rules);
    fs::write(
        demo.dir.path().join("api/src/lib.rs"),
        "mod private {\n    pub use types::*;\n}\npub mod open {\n    pub use types::extract::*;\n}\n",
    )
    .expect("write lib.rs");
    let (_, report) = demo.json(&["boundaries"]);
    let lines: Vec<u64> = findings(&report, "glob-reexport")
        .iter()
        .filter_map(|finding| finding["line"].as_u64())
        .collect();
    assert_eq!(lines, [5], "{report:#}");
}

/// A cfg rule and a crate with a path that the Miri build does not compile.
const CFG_RULE: &str = "
[[workspace.metadata.fallow.boundaries.cfg]]
rule = \"miri-build\"
builds = [[\"test\"], [\"test\", \"miri\"]]
";

const MIRI_GAP: &str = "#[cfg(all(test, not(miri)))]
mod helpers {
    pub fn parse_ts() {}
}
#[cfg(test)]
fn uses() {
    crate::helpers::parse_ts();
}
";

/// In `check`, the item, re-export and cfg rules read the module graph of the
/// graph thread, which selects the production binaries of `visibility`. The
/// section is the same as the section of `boundaries`, with and without a
/// selection that leaves a binary out.
#[test]
fn the_boundary_section_of_check_is_the_section_of_boundaries() {
    let selection = "\n[workspace.metadata.fallow.visibility]\nproduction = [\"cli:cli\"]\n";
    for production in ["", selection] {
        let demo = Demo::new(&format!("{RULES}{CFG_RULE}{production}"));
        let root = demo.dir.path();
        fs::write(root.join("cli/src/main.rs"), "fn main() {}\n").expect("write a binary");
        fs::write(root.join("types/src/main.rs"), "fn main() {}\n").expect("write a binary");
        let types = fs::read_to_string(root.join("types/src/lib.rs")).expect("read lib.rs");
        fs::write(root.join("types/src/lib.rs"), format!("{types}{MIRI_GAP}"))
            .expect("write lib.rs");
        let (_, single) = demo.json(&["boundaries"]);
        let (_, combined) = demo.json(&["check"]);
        let rules: Vec<&str> = single["findings"]
            .as_array()
            .expect("the findings")
            .iter()
            .filter_map(|finding| finding["rule"].as_str())
            .collect();
        for rule in ["cli-runs-dead-code-through-the-session", "miri-build"] {
            assert!(rules.contains(&rule), "{rule}: {single:#}");
        }
        assert_eq!(combined["sections"]["boundaries"], single, "{production}");
    }
}
