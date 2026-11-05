//! Black-box tests of `cargo fallow api --base <REF>`.
//!
//! Each test writes a workspace to a temporary git repository with a base
//! commit on `main` and a head commit on a branch, and compares the two.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

const BASE_LIB: &str = "pub mod shapes {
    pub struct Circle {
        pub radius: f64,
    }
    impl Circle {
        pub fn area(&self) -> f64 {
            self.radius * self.radius * 3.0
        }
        pub fn grow(&mut self, by: f64) {
            self.radius += by;
        }
    }
}
mod other {
    pub struct Circle;
    impl Circle {
        pub fn hidden_twin(&self) {}
    }
}
mod inner {
    pub struct Moved;
}
pub use inner::Moved;
pub fn keep(a: u8) -> u8 {
    a
}
pub fn gone() {}
pub fn reshaped(a: u8) -> u8 {
    a
}
#[doc(hidden)]
pub fn secret() {}
#[cfg(unix)]
pub fn per_os() {}
#[cfg(windows)]
pub fn per_os() {}
pub trait Draw {
    fn draw(&self);
}
";

const HEAD_LIB: &str = "pub mod shapes {
    pub struct Circle {
        pub radius: f64,
    }
    impl Circle {
        pub fn area(&self) -> f64 {
            self.radius * self.radius * 3.14
        }
    }
}
mod other {
    pub struct Circle;
    impl Circle {}
}
mod inner {
    pub struct Moved;
}
pub fn keep(a: u8) -> u8 {
    a + 1
}
pub fn reshaped(a: u16) -> u8 {
    a as u8
}
pub fn fresh() {}
#[cfg(unix)]
pub fn per_os() {}
pub trait Draw {
    fn draw(&self);
    fn outline(&self) {}
}
";

struct Repo {
    dir: TempDir,
}

impl Repo {
    /// A repository with the base commit on `main` and the head commit on
    /// `feature`. `head_version` is the version of `demo` at HEAD.
    fn new(head_version: &str) -> Self {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let repo = Self { dir };
        repo.write_workspace(BASE_LIB, "0.3.1", &["serde", "fast"], true);
        repo.git(&["init", "-q"]);
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-q", "-m", "base"]);
        repo.git(&["checkout", "-q", "-b", "feature"]);
        repo.write_workspace(HEAD_LIB, head_version, &["fast"], false);
        fs::create_dir_all(repo.root().join("fresh/src")).expect("create fresh");
        fs::write(
            repo.root().join("fresh/Cargo.toml"),
            "[package]\nname = \"fresh\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("write fresh");
        fs::write(repo.root().join("fresh/src/lib.rs"), "pub fn hello() {}\n").expect("write lib");
        let manifest = repo.root().join("Cargo.toml");
        let text = fs::read_to_string(&manifest).expect("read the manifest");
        fs::write(
            &manifest,
            text.replace("\"internal\"]", "\"internal\", \"fresh\"]"),
        )
        .expect("write the manifest");
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-q", "-m", "head"]);
        repo
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn write_workspace(&self, lib: &str, version: &str, features: &[&str], legacy: bool) {
        let root = self.root();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"demo\", \"legacy\", \"internal\"]\nresolver = \"3\"\n",
        )
        .expect("write the root manifest");
        let mut table = String::from("[features]\n");
        for feature in features {
            let _ = writeln!(table, "{feature} = []");
        }
        package(root, "demo", version, "", &table, lib);
        // `legacy` is published at the base and `publish = false` at HEAD.
        let publish = if legacy { "" } else { "publish = false\n" };
        package(root, "legacy", "1.0.0", publish, "", "pub fn old() {}\n");
        package(
            root,
            "internal",
            "0.1.0",
            "publish = false\n",
            "",
            "pub fn inside() {}\n",
        );
    }

    fn git(&self, args: &[&str]) {
        let output = StdCommand::new("git")
            .current_dir(self.root())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .output()
            .expect("run git");
        assert!(output.status.success(), "git {args:?}: {output:?}");
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        let output = Command::cargo_bin("cargo-fallow")
            .expect("find the binary")
            .current_dir(self.root())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .arg("fallow")
            .args(args)
            .arg("--quiet")
            .output()
            .expect("run cargo-fallow");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )
    }

    fn json(&self, args: &[&str]) -> (i32, Value) {
        let mut all = args.to_vec();
        all.extend(["--format", "json"]);
        let (code, stdout) = self.run(&all);
        let value = serde_json::from_str(&stdout).expect("the output is JSON");
        (code, value)
    }
}

fn package(root: &Path, name: &str, version: &str, publish: &str, features: &str, lib: &str) {
    let dir = root.join(name);
    fs::create_dir_all(dir.join("src")).expect("create src");
    fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"{version}\"\nedition = \"2024\"\n{publish}\n{features}"
        ),
    )
    .expect("write a manifest");
    fs::write(dir.join("src/lib.rs"), lib).expect("write lib.rs");
}

/// `(crate, change, path)` of each finding, sorted.
fn changes(report: &Value) -> Vec<(String, String, String)> {
    let mut found: Vec<(String, String, String)> = report["findings"]
        .as_array()
        .expect("a findings list")
        .iter()
        .map(|finding| {
            let text = |key: &str| finding[key].as_str().unwrap_or_default().to_owned();
            (text("crate"), text("change"), text("path"))
        })
        .collect();
    found.sort();
    found
}

fn entry(krate: &str, change: &str, path: &str) -> (String, String, String) {
    (krate.to_owned(), change.to_owned(), path.to_owned())
}

#[test]
fn api_reports_the_changes_of_each_published_library() {
    let repo = Repo::new("0.3.2");
    let (code, report) = repo.json(&["api", "--base", "main"]);
    assert_eq!(code, 0, "{report:#}");
    let mut expected = vec![
        entry("demo", "added", "demo::Draw::outline"),
        entry("demo", "added", "demo::fresh"),
        entry("demo", "changed", "demo::per_os"),
        entry("demo", "changed", "demo::reshaped"),
        entry("demo", "feature-removed", "serde"),
        entry("demo", "removed", "demo::Moved"),
        entry("demo", "removed", "demo::gone"),
        entry("demo", "removed", "demo::shapes::Circle::grow"),
        entry("fresh", "crate-added", "fresh"),
        entry("legacy", "crate-removed", "legacy"),
    ];
    expected.sort();
    assert_eq!(changes(&report), expected, "{report:#}");
    let reshaped = report["findings"]
        .as_array()
        .and_then(|all| all.iter().find(|f| f["path"] == "demo::reshaped"))
        .expect("the reshaped finding");
    assert_eq!(reshaped["item_kind"], "function");
    assert_eq!(reshaped["before"], "pub fn reshaped ( u8 ) -> u8");
    assert_eq!(reshaped["after"], "pub fn reshaped ( u16 ) -> u8");
    assert_eq!(reshaped["file"], "demo/src/lib.rs");
    assert_eq!(reshaped["version_base"], "0.3.1");
    assert_eq!(reshaped["version_head"], "0.3.2");
    let summary = &report["summary"];
    assert_eq!(summary["base"], "main");
    assert_eq!(summary["crates"], 3);
    assert_eq!(summary["added"], 3);
    assert_eq!(summary["removed"], 5);
    assert_eq!(summary["changed"], 2);
    assert_eq!(summary["merge_base"].as_str().map(str::len), Some(40));
    // The breaking part of 0.3.x is the minor version, which did not change.
    let gate = &report["gate_outcomes"]["api-removals"];
    assert_eq!(gate["status"], "fail", "{report:#}");
    assert_eq!(gate["observed"], 5.0, "{report:#}");
    let (code, _) = repo.json(&["api", "--base", "main", "--fail-on-issues"]);
    assert_eq!(code, 1);
}

#[test]
fn a_breaking_version_bump_lets_the_removals_pass_the_gate() {
    let repo = Repo::new("0.4.0");
    let (code, report) = repo.json(&["api", "--base", "main", "--fail-on-issues"]);
    // `legacy` is gone from the published crates, which no bump covers.
    assert_eq!(
        report["gate_outcomes"]["api-removals"]["observed"], 1.0,
        "{report:#}"
    );
    assert_eq!(code, 1);
    let (code, report) = repo.json(&["api", "--base", "main", "-p", "demo", "--fail-on-issues"]);
    assert_eq!(code, 0, "{report:#}");
}

#[test]
fn a_missing_base_exits_2_and_check_does_not_run_api() {
    let repo = Repo::new("0.3.2");
    let (code, report) = repo.json(&["api", "--base", "nothing"]);
    assert_eq!(code, 2);
    assert_eq!(report["code"], "CARGO_FALLOW_INVALID_ARGUMENT");
    let (_, report) = repo.json(&["check", "--skip", "deps"]);
    assert!(report["sections"].get("api").is_none(), "{report:#}");
}

#[test]
fn a_shallow_clone_without_the_merge_base_names_the_fetch_depth() {
    let repo = Repo::new("0.3.2");
    let clone = tempfile::tempdir().expect("create a temporary directory");
    let url = format!("file://{}", repo.root().display());
    let output = StdCommand::new("git")
        .args(["clone", "-q", "--depth", "1", "--no-single-branch", &url])
        .arg(clone.path().join("c"))
        .output()
        .expect("run git clone");
    assert!(output.status.success(), "{output:?}");
    let output = Command::cargo_bin("cargo-fallow")
        .expect("find the binary")
        .current_dir(clone.path().join("c"))
        .args([
            "fallow",
            "api",
            "--base",
            "origin/main",
            "--format",
            "json",
            "--quiet",
        ])
        .output()
        .expect("run cargo-fallow");
    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(output.status.code(), Some(2), "{report:#}");
    assert!(
        report["help"]
            .as_str()
            .is_some_and(|help| help.contains("fetch-depth: 0")),
        "{report:#}"
    );
}

#[test]
fn sarif_has_no_added_result_and_a_removed_item_points_to_its_module() {
    let repo = Repo::new("0.3.2");
    let (_, sarif) = repo.run(&["api", "--base", "main", "--format", "sarif"]);
    let sarif: Value = serde_json::from_str(&sarif).expect("SARIF is JSON");
    let results = sarif["runs"][0]["results"].as_array().expect("results");
    assert_eq!(results.len(), 7, "{sarif:#}");
    let gone = results
        .iter()
        .find(|result| {
            result["message"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("demo::gone"))
        })
        .expect("the removed function");
    let location = &gone["locations"][0]["physicalLocation"]["artifactLocation"]["uri"];
    assert_eq!(location, "demo/src/lib.rs");
    let legacy = results
        .iter()
        .find(|result| {
            result["message"]["text"]
                .as_str()
                .is_some_and(|text| text.contains("legacy"))
        })
        .expect("the removed crate");
    let location = &legacy["locations"][0]["physicalLocation"]["artifactLocation"]["uri"];
    assert_eq!(location, "legacy/Cargo.toml");
    let (_, human) = repo.run(&["api", "--base", "main"]);
    assert!(human.contains("3 added"), "{human}");
}

#[test]
fn a_glob_cycle_a_hidden_reexport_and_a_rename_give_no_extra_finding() {
    let repo = Repo::new("0.3.2");
    let lib = repo.root().join("demo/src/lib.rs");
    let head = fs::read_to_string(&lib).expect("read lib.rs");
    // The base of this branch is the HEAD of the fixture.
    repo.git(&["checkout", "-q", "-b", "second"]);
    let base = format!(
        "{head}pub mod cycle {{\n    pub use super::*;\n    pub fn inner<T>(value: T) -> T {{\n        value\n    }}\n}}\n#[doc(hidden)]\npub use inner::Moved as HiddenMoved;\n"
    );
    fs::write(&lib, &base).expect("write lib.rs");
    repo.git(&["commit", "-q", "-am", "base 2"]);
    repo.git(&["checkout", "-q", "-b", "third"]);
    let renamed = base
        .replace(
            "pub fn inner<T>(value: T) -> T",
            "pub fn inner<U>(other: U) -> U",
        )
        .replace("pub fn fresh() {}\n", "")
        .replace("#[doc(hidden)]\npub use inner::Moved as HiddenMoved;\n", "");
    fs::write(&lib, renamed).expect("write lib.rs");
    repo.git(&["commit", "-q", "-am", "head 2"]);
    let (_, report) = repo.json(&["api", "--base", "second"]);
    // Only `fresh` is gone. A glob that points back up the walk adds no path,
    // so the removal counts one time.
    assert_eq!(
        changes(&report),
        [entry("demo", "removed", "demo::fresh")],
        "{report:#}"
    );
}

#[test]
fn a_baseline_flag_is_an_error() {
    let repo = Repo::new("0.3.2");
    let (code, report) = repo.json(&["api", "--base", "main", "--save-baseline", "b.json"]);
    assert_eq!(code, 2, "{report:#}");
    assert_eq!(report["code"], "CARGO_FALLOW_INVALID_ARGUMENT");
}

#[test]
fn a_rule_set_to_off_removes_its_findings() {
    let repo = Repo::new("0.3.2");
    let manifest = repo.root().join("Cargo.toml");
    let text = fs::read_to_string(&manifest).expect("read the manifest");
    fs::write(
        &manifest,
        format!("{text}\n[workspace.metadata.fallow.rules]\napi-addition = \"off\"\napi-change = \"off\"\n"),
    )
    .expect("write the manifest");
    let (_, report) = repo.json(&["api", "--base", "main"]);
    let kinds: BTreeSet<String> = changes(&report)
        .into_iter()
        .map(|(_, change, _)| change)
        .collect();
    assert!(
        !kinds.contains("added") && !kinds.contains("changed"),
        "{report:#}"
    );
    assert!(kinds.contains("removed"), "{report:#}");
}
