//! Black-box tests of the file scores, the hotspots, the snapshots and the
//! trend of `cargo fallow check`.
//!
//! Each test writes a small workspace to a temporary git repository with a
//! known commit history.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// A function with many branches.
const BUSY: &str = "pub fn busy(a: i32, b: i32) -> i32 {\n    if a > 0 && b > 0 {\n        1\n    } else if a < 0 || b < 0 {\n        2\n    } else if a == b {\n        3\n    } else {\n        match a {\n            0 => 4,\n            1 => 5,\n            _ => 6,\n        }\n    }\n}\n";
/// A function with no branch.
const CALM: &str = "pub fn calm(a: i32) -> i32 {\n    a + 1\n}\n";

fn git(dir: &Path, args: &[&str]) {
    let output = StdCommand::new("git")
        .current_dir(dir)
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

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create a folder");
    }
    fs::write(path, text).expect("write a file");
}

fn append(root: &Path, path: &str, text: &str) {
    let old = fs::read_to_string(root.join(path)).expect("read a file");
    write(root, path, &format!("{old}{text}"));
}

fn commit(root: &Path, message: &str) {
    git(root, &["add", "--all"]);
    git(root, &["commit", "--quiet", "-m", message]);
}

/// A workspace where `busy.rs` and `calm.rs` change in four commits, and
/// `once.rs` in one.
fn repository() -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\"]\nresolver = \"3\"\n",
    );
    write(
        root,
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    );
    write(
        root,
        "app/src/lib.rs",
        "pub mod busy;\npub mod calm;\npub mod once;\n",
    );
    write(root, "app/src/busy.rs", BUSY);
    write(root, "app/src/calm.rs", CALM);
    write(root, "app/src/once.rs", &BUSY.replace("busy", "once"));
    git(root, &["init", "--quiet"]);
    commit(root, "start");
    for round in 1..=3 {
        append(root, "app/src/busy.rs", &format!("// Change {round}.\n"));
        append(root, "app/src/calm.rs", &format!("// Change {round}.\n"));
        commit(root, &format!("change {round}"));
    }
    dir
}

/// Runs `cargo fallow check` in `root` and returns the exit code, the JSON
/// report and the stderr text.
fn check(root: &Path, args: &[&str]) -> (Option<i32>, Value, String) {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("find the binary")
        .current_dir(root)
        .args(["check", "--format", "json", "--quiet"])
        .args(args)
        .output()
        .expect("run the binary");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let json = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "the output is JSON: {err}\n{}\n{stderr}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    (output.status.code(), json, stderr)
}

fn paths(list: &Value) -> Vec<&str> {
    list.as_array()
        .expect("a list")
        .iter()
        .filter_map(|entry| entry["path"].as_str())
        .collect()
}

#[test]
fn hotspots_rank_the_complex_files_that_change_most() {
    let dir = repository();
    let (code, report, stderr) = check(dir.path(), &["--only", "complexity", "--hotspots"]);
    assert_eq!(code, Some(0), "{stderr}");
    // `once.rs` has one commit, under the default `--min-commits` of 3.
    assert_eq!(
        paths(&report["hotspots"]),
        ["app/src/busy.rs", "app/src/calm.rs"],
        "{report:#}"
    );
    let busy = &report["hotspots"][0];
    assert_eq!(busy["commits"], 4);
    assert_eq!(busy["score"], 100.0);
    assert_eq!(busy["lines_added"], 18);
    let calm = report["hotspots"][1]["score"].as_f64().expect("a score");
    assert!(calm > 0.0 && calm < 100.0, "{report:#}");
    let summary = &report["hotspot_summary"];
    assert_eq!(summary["since"], "6m");
    assert_eq!(summary["min_commits"], 3);
    assert_eq!(summary["shallow_clone"], false);

    let (_, report, _) = check(
        dir.path(),
        &["--only", "complexity", "--hotspots", "--min-commits", "1"],
    );
    // `lib.rs` has no function, so it is not a hotspot.
    let mut all = paths(&report["hotspots"]);
    all.sort_unstable();
    assert_eq!(
        all,
        ["app/src/busy.rs", "app/src/calm.rs", "app/src/once.rs"]
    );
}

#[test]
fn a_shallow_clone_gives_a_warning_and_no_hotspots() {
    let dir = repository();
    let clone = tempfile::tempdir().expect("create a temporary directory");
    let source = format!("file://{}", dir.path().display());
    git(
        clone.path(),
        &["clone", "--quiet", "--depth", "1", &source, "shallow"],
    );
    let root = clone.path().join("shallow");
    let (code, report, stderr) = check(&root, &["--only", "complexity", "--hotspots"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(report["hotspots"], Value::Array(Vec::new()), "{report:#}");
    assert_eq!(report["hotspot_summary"]["shallow_clone"], true);
    assert!(stderr.contains("shallow"), "{stderr}");
}

#[test]
fn file_scores_put_the_least_maintainable_file_first() {
    let dir = repository();
    let (code, report, stderr) = check(dir.path(), &["--only", "complexity", "--file-scores"]);
    assert_eq!(code, Some(0), "{stderr}");
    let scores = &report["file_scores"];
    let order = paths(scores);
    assert_eq!(order.len(), 4, "{report:#}");
    assert_eq!(order.last(), Some(&"app/src/lib.rs"), "{report:#}");
    let calm = scores
        .as_array()
        .expect("a list")
        .iter()
        .find(|entry| entry["path"] == "app/src/calm.rs")
        .expect("calm.rs has a score");
    assert_eq!(calm["function_count"], 1);
    assert_eq!(calm["total_cyclomatic"], 1);
    assert!(report.get("hotspots").is_none(), "{report:#}");
}

#[test]
fn hotspots_need_the_complexity_check() {
    let dir = repository();
    let (code, report, _) = check(dir.path(), &["--only", "size", "--hotspots"]);
    assert_eq!(code, Some(2), "{report:#}");
    assert_eq!(report["error"], true);
}

#[test]
fn an_invalid_since_value_is_an_input_error() {
    let dir = repository();
    let (code, report, _) = check(
        dir.path(),
        &["--only", "complexity", "--hotspots", "--since", "6x"],
    );
    assert_eq!(code, Some(2), "{report:#}");
}

#[test]
fn a_trend_compares_the_run_with_a_saved_snapshot() {
    let dir = repository();
    let root = dir.path();
    let snapshot = root.join("snapshot.json");
    let snapshot = snapshot.to_str().expect("UTF-8");
    let (code, _, stderr) = check(
        root,
        &["--only", "complexity,size", "--save-snapshot", snapshot],
    );
    assert_eq!(code, Some(0), "{stderr}");
    let saved: Value =
        serde_json::from_str(&fs::read_to_string(snapshot).expect("read the snapshot"))
            .expect("the snapshot is JSON");
    assert_eq!(saved["kind"], "health-snapshot", "{saved:#}");
    assert!(saved["git_sha"].is_string(), "{saved:#}");

    let mut long = String::new();
    for line in 0..70 {
        writeln!(long, "    let _x{line} = {line};").expect("write to a string");
    }
    append(
        root,
        "app/src/calm.rs",
        &format!("pub fn long() {{\n{long}}}\n"),
    );
    let (_, report, stderr) = check(
        root,
        &["--only", "complexity,size", "--trend-from", snapshot],
    );
    let trend = &report["health_trend"];
    assert_eq!(trend["snapshots_loaded"], 1, "{report:#}\n{stderr}");
    let unit_size = trend["metrics"]
        .as_array()
        .expect("metrics")
        .iter()
        .find(|metric| metric["name"] == "unit_size")
        .unwrap_or_else(|| panic!("a unit_size metric: {trend:#}"));
    assert_eq!(unit_size["direction"], "declining", "{trend:#}");
    assert!(unit_size["delta"].as_f64().is_some_and(|delta| delta > 0.0));
}

#[test]
fn a_default_snapshot_is_the_base_of_the_next_trend() {
    let dir = repository();
    let root = dir.path();
    let (code, _, stderr) = check(root, &["--only", "complexity", "--save-snapshot"]);
    assert_eq!(code, Some(0), "{stderr}");
    let saved: Vec<_> = fs::read_dir(root.join(".fallow/snapshots"))
        .expect("the snapshot folder")
        .collect();
    assert_eq!(saved.len(), 1);
    let (_, report, stderr) = check(root, &["--only", "complexity", "--trend"]);
    assert_eq!(
        report["health_trend"]["snapshots_loaded"], 1,
        "{report:#}\n{stderr}"
    );
    assert_eq!(report["health_trend"]["overall_direction"], "stable");
}

#[test]
fn a_trend_without_a_snapshot_gives_a_warning() {
    let dir = repository();
    let (code, report, stderr) = check(dir.path(), &["--only", "complexity", "--trend"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(report.get("health_trend").is_none(), "{report:#}");
    assert!(stderr.contains("snapshot"), "{stderr}");
}

#[test]
fn the_human_text_lists_the_hotspots_the_file_scores_and_the_trend() {
    let dir = repository();
    let root = dir.path();
    let snapshot = root.join("snapshot.json");
    let snapshot = snapshot.to_str().expect("UTF-8");
    let (code, _, stderr) = check(root, &["--only", "complexity", "--save-snapshot", snapshot]);
    assert_eq!(code, Some(0), "{stderr}");
    let output = Command::cargo_bin("cargo-fallow")
        .expect("find the binary")
        .current_dir(root)
        .args([
            "check",
            "--format",
            "human",
            "--quiet",
            "--only",
            "complexity",
        ])
        .args(["--hotspots", "--file-scores", "--trend-from", snapshot])
        .output()
        .expect("run the binary");
    let text = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{text}");
    assert!(
        text.contains(
            "Hotspots (since 6m, 3 or more commits): 2 files\n  100.0  app/src/busy.rs  4 commits, density 0.44, "
        ),
        "{text}"
    );
    let scores = text
        .split_once("File scores, the least maintainable first:\n")
        .map_or_else(|| panic!("a list of file scores: {text}"), |(_, rest)| rest);
    let order: Vec<&str> = scores
        .lines()
        .take_while(|line| line.starts_with("  "))
        .filter_map(|line| line.split_whitespace().nth(1))
        .collect();
    assert_eq!(order.len(), 4, "{text}");
    assert_eq!(order.last(), Some(&"app/src/lib.rs"), "{text}");
    assert!(text.contains("Trend against the snapshot of "), "{text}");
    assert!(
        text.contains("  complexity 0 -> 0 (+0, stable)\n"),
        "{text}"
    );
}
