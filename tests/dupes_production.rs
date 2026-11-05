//! Black-box tests of `dupes.production` and `dupes --production`.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// A function body that is long enough to be a clone, with the name `name`.
fn body(name: &str) -> String {
    format!(
        "pub fn {name}(items: &[u32]) -> u32 {{\n    let mut total = 0;\n    for item in items {{\n        if *item > 10 {{\n            total += item * 2;\n        }} else {{\n            total += item + 1;\n        }}\n    }}\n    let extra = items.iter().filter(|x| **x % 3 == 0).count() as u32;\n    total + extra * 7\n}}\n"
    )
}

/// Writes a crate with one copy in `src/lib.rs`, one in `src/other.rs`, one
/// in a `#[cfg(test)]` module, one in a `#[test]` function file module, one
/// in a `#[cfg(test)]` file module and one in the `tests/it.rs` test target.
fn workspace(settings: &str) -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        &format!(
            "[package]\nname = \"copies\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[workspace]\n\n[workspace.metadata.fallow.dupes]\nmin-tokens = 30\nmin-lines = 3\n{settings}"
        ),
    );
    write(
        root,
        "src/lib.rs",
        &format!(
            "pub mod other;\n#[cfg(test)]\nmod support;\n\n{}\n#[cfg(test)]\nmod tests {{\n    {}\n}}\n",
            body("first"),
            body("in_tests").replace('\n', "\n    ")
        ),
    );
    write(
        root,
        "src/other.rs",
        &format!(
            "{}\n#[test]\nfn check() {{\n    let items: &[u32] = &[1, 2, 3];\n    let mut total = 0;\n    for item in items {{\n        if *item > 10 {{\n            total += item * 2;\n        }} else {{\n            total += item + 1;\n        }}\n    }}\n    let extra = items.iter().filter(|x| **x % 3 == 0).count() as u32;\n    assert_eq!(total + extra * 7, 16);\n}}\n",
            body("second")
        ),
    );
    write(root, "src/support.rs", &body("in_support"));
    write(root, "tests/it.rs", &body("in_target"));
    dir
}

/// The files with a copy, sorted.
const ALL_FILES: [&str; 4] = [
    "src/lib.rs",
    "src/other.rs",
    "src/support.rs",
    "tests/it.rs",
];

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("a file path has a parent")).expect("create a folder");
    fs::write(full, text).expect("write a file");
}

fn dupes(dir: &TempDir, args: &[&str]) -> (i32, Value) {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args(["fallow", "dupes", "--format", "json", "--quiet"])
        .args(args)
        .output()
        .expect("run the binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report = serde_json::from_str(&stdout)
        .unwrap_or_else(|err| panic!("stdout is JSON ({err}): {stdout}"));
    (output.status.code().unwrap_or(-1), report)
}

/// The sorted files of the instances of each group.
fn instance_files(report: &Value) -> Vec<Vec<String>> {
    report["findings"]
        .as_array()
        .expect("the report has findings")
        .iter()
        .map(|group| {
            let mut files: Vec<String> = group["instances"]
                .as_array()
                .expect("a group has instances")
                .iter()
                .map(|instance| instance["file"].as_str().unwrap_or_default().to_owned())
                .collect();
            files.sort();
            files.dedup();
            files
        })
        .collect()
}

fn total_lines(report: &Value) -> u64 {
    report["summary"]["stats"]["total_lines"]
        .as_u64()
        .unwrap_or_default()
}

#[test]
fn by_default_the_test_copies_are_clones_too() {
    let dir = workspace("");
    let (code, report) = dupes(&dir, &[]);
    assert_eq!(code, 0, "{report}");
    let files = instance_files(&report);
    assert!(
        files.contains(&ALL_FILES.map(str::to_owned).to_vec()),
        "each file has a copy: {files:?}"
    );
}

#[test]
fn production_leaves_out_test_items_test_modules_and_test_targets() {
    let dir = workspace("production = true\n");
    let (code, report) = dupes(&dir, &[]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(
        instance_files(&report),
        [["src/lib.rs", "src/other.rs"]],
        "{report:#}"
    );
    let (_, all) = dupes(&workspace(""), &[]);
    assert!(
        total_lines(&report) < total_lines(&all),
        "the line count leaves out test code: {} and {}",
        total_lines(&report),
        total_lines(&all)
    );
    assert_eq!(report["summary"]["production"], true, "{report}");
}

#[test]
fn the_production_flag_overrides_the_table() {
    let dir = workspace("");
    let (_, report) = dupes(&dir, &["--production"]);
    assert_eq!(
        instance_files(&report),
        [["src/lib.rs", "src/other.rs"]],
        "{report:#}"
    );
    let dir = workspace("production = true\n");
    let (_, report) = dupes(&dir, &["--no-production"]);
    assert!(
        instance_files(&report).contains(&ALL_FILES.map(str::to_owned).to_vec()),
        "{report:#}"
    );
}

#[test]
fn production_changes_the_percentage_that_the_threshold_reads() {
    let (_, report) = dupes(&workspace("production = true\n"), &[]);
    let (_, all) = dupes(&workspace(""), &[]);
    let percentage = |report: &Value| {
        report["summary"]["stats"]["duplication_percentage"]
            .as_f64()
            .unwrap_or_default()
    };
    assert!(percentage(&report) > 0.0, "{report:#}");
    assert!(
        (percentage(&report) - percentage(&all)).abs() > f64::EPSILON,
        "{report:#}"
    );
}

#[test]
fn production_leaves_out_a_file_below_an_inner_test_attribute() {
    let dir = workspace("production = true\n");
    let lib = fs::read_to_string(dir.path().join("src/lib.rs")).expect("read a file");
    write(dir.path(), "src/lib.rs", &format!("mod fixtures;\n{lib}"));
    write(dir.path(), "src/fixtures.rs", "#![cfg(test)]\nmod data;\n");
    write(dir.path(), "src/fixtures/data.rs", &body("in_data"));
    let (_, report) = dupes(&dir, &[]);
    assert_eq!(
        instance_files(&report),
        [["src/lib.rs", "src/other.rs"]],
        "{report:#}"
    );
}

#[test]
fn a_file_that_production_code_also_reaches_counts_as_production_code() {
    let dir = workspace("production = true\n");
    // `src/support.rs` is a test module of the library, and a plain module of
    // the binary.
    write(dir.path(), "src/main.rs", "mod support;\nfn main() {}\n");
    let (_, report) = dupes(&dir, &[]);
    assert_eq!(
        instance_files(&report),
        [["src/lib.rs", "src/other.rs", "src/support.rs"]],
        "{report:#}"
    );
}

#[test]
fn check_reads_the_production_setting_of_the_dupes_table() {
    let dir = workspace("production = true\n");
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args([
            "fallow", "check", "--only", "dupes", "--format", "json", "--quiet",
        ])
        .output()
        .expect("run the binary");
    let report: Value = serde_json::from_slice(&output.stdout).expect("the output is JSON");
    let section = &report["sections"]["dupes"];
    assert_eq!(section["summary"]["production"], true, "{report:#}");
    assert_eq!(
        instance_files(section),
        [["src/lib.rs", "src/other.rs"]],
        "{report:#}"
    );
}
