//! The published JSON Schema: `docs/output-schema.json` equals the output of
//! `cargo fallow schema`, and each JSON document of the tool validates against
//! the schema of its kind.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::Command;
use serde_json::{Value, json};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
const PUBLISHED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/output-schema.json");
/// Set this variable to write the generated schema to the published file.
const UPDATE: &str = "CARGO_FALLOW_UPDATE_SCHEMA";

fn binary() -> Command {
    let mut command = Command::cargo_bin("cargo-fallow").expect("find the binary");
    command.arg("fallow");
    command
}

fn generated() -> String {
    let output = binary()
        .arg("schema")
        .output()
        .expect("run the schema command");
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).expect("UTF-8")
}

fn schema() -> Value {
    serde_json::from_str(&generated()).expect("the schema is JSON")
}

#[test]
fn the_published_schema_equals_the_generated_schema() {
    // The file is the exact output of `cargo fallow schema`.
    let text = generated();
    if std::env::var_os(UPDATE).is_some() {
        fs::write(PUBLISHED, &text).expect("write the published schema");
    }
    let published = fs::read_to_string(PUBLISHED).expect("read docs/output-schema.json");
    assert!(
        published == text,
        "docs/output-schema.json differs from `cargo fallow schema`. Run the tests with {UPDATE}=1 to write it."
    );
}

#[test]
fn each_definition_has_a_stable_name() {
    let schema = schema();
    let names: Vec<&String> = schema["$defs"].as_object().expect("defs").keys().collect();
    // A name with a number at the end means that two types share a name, and
    // the number changes when a type is added.
    let numbered: Vec<&&String> = names
        .iter()
        .filter(|name| name.ends_with(|c: char| c.is_ascii_digit()))
        .collect();
    assert!(numbered.is_empty(), "{numbered:?}");
}

/// The validator of the definition `document-<kind>`.
fn validator(schema: &Value, kind: &str) -> jsonschema::Validator {
    let one = json!({
        "$schema": schema["$schema"],
        "$ref": format!("#/$defs/document-{kind}"),
        "$defs": schema["$defs"],
    });
    jsonschema::draft202012::new(&one).expect("the schema compiles")
}

/// Validates `document` against the definition `document-<kind>`.
fn assert_valid(schema: &Value, kind: &str, document: &Value) {
    let validator = validator(schema, kind);
    let errors: Vec<String> = validator
        .iter_errors(document)
        .map(|error| {
            let mut text = error.to_string();
            text.truncate(200);
            format!("{text} at {}", error.instance_path())
        })
        .collect();
    assert!(errors.is_empty(), "{kind}: {errors:#?}");
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create the fixture directory");
    for entry in fs::read_dir(from).expect("read the fixture directory") {
        let entry = entry.expect("read a fixture entry");
        if entry.file_name() == "target" {
            continue;
        }
        let target = to.join(entry.file_name());
        if entry.file_type().expect("read the entry type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy a fixture file");
        }
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = StdCommand::new("git")
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
    assert!(status.status.success(), "git {args:?}: {status:?}");
}

fn json_run(root: &Path, args: &[&str]) -> Value {
    let output = binary()
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(args)
        .args(["--format", "json", "--quiet"])
        .output()
        .expect("run cargo-fallow");
    serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .unwrap_or_else(|err| panic!("{args:?}: not JSON: {err}: {output:?}"))
}

#[test]
fn each_report_of_the_structure_fixture_validates() {
    let schema = schema();
    let dir = tempfile::tempdir().expect("temporary directory");
    let root = dir.path();
    copy_dir(&Path::new(FIXTURES).join("ws-structure"), root);
    git(root, &["init", "--quiet"]);
    git(root, &["add", "--all"]);
    git(root, &["commit", "--quiet", "-m", "base"]);
    let core = fs::read_to_string(root.join("crates/core/src/lib.rs")).expect("read");
    fs::write(
        root.join("crates/core/src/lib.rs"),
        format!("{core}\n// More.\n"),
    )
    .expect("write");
    fs::write(
        root.join("crates/app/src/orphan.rs"),
        "pub fn orphan() {}\n",
    )
    .expect("write");

    let baseline = root.join("baseline.json");
    let baseline = baseline.to_str().expect("UTF-8");
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("check", vec!["check"]),
        (
            "check",
            vec!["check", "--skip", "deps", "--changed-since", "main"],
        ),
        ("check", vec!["check", "--save-baseline", baseline]),
        ("check", vec!["check", "--baseline", baseline]),
        ("deps", vec!["deps"]),
        ("fix", vec!["fix", "--dry-run"]),
        ("visibility", vec!["visibility"]),
        (
            "visibility-trace",
            vec!["visibility", "--trace", "core::branchy"],
        ),
        ("deps-trace", vec!["deps", "--trace", "core"]),
        ("files", vec!["files"]),
        ("boundaries", vec!["boundaries", "--fail-on-issues"]),
        ("size", vec!["size", "--base", "main"]),
        ("size", vec!["size", "--staged"]),
        ("complexity", vec!["complexity", "--changed-since", "main"]),
        (
            "dupes",
            vec!["dupes", "--min-tokens", "30", "--threshold", "1"],
        ),
        ("dupes", vec!["dupes", "--baseline", baseline]),
        ("error", vec!["size", "--base", "no-such-ref"]),
        ("error", vec!["check", "--bogus"]),
    ];
    for (kind, args) in cases {
        let document = json_run(root, &args);
        if kind != "error" {
            assert_eq!(document["kind"], kind, "{args:?}: {document:#}");
        }
        assert_valid(&schema, kind, &document);
    }
    let dupes = json_run(root, &["dupes", "--min-tokens", "30"]);
    let fingerprint = dupes["findings"][0]["fingerprint"]
        .as_str()
        .unwrap_or_else(|| panic!("the fixture has a clone group: {dupes:#}"));
    let traced = json_run(
        root,
        &["dupes", "--min-tokens", "30", "--trace", fingerprint],
    );
    assert_eq!(traced["kind"], "dupes-trace", "{traced:#}");
    assert_valid(&schema, "dupes-trace", &traced);
    let saved: Value =
        serde_json::from_str(&fs::read_to_string(baseline).expect("read")).expect("JSON");
    assert_valid(&schema, "baseline", &saved);
}

#[test]
fn a_mutants_list_validates() {
    let schema = schema();
    let dir = tempfile::tempdir().expect("temporary directory");
    copy_dir(&Path::new(FIXTURES).join("action-mutants"), dir.path());
    let document = json_run(dir.path(), &["mutants", "--list"]);
    assert_eq!(document["kind"], "mutants", "{document:#}");
    assert_valid(&schema, "mutants", &document);
}

#[test]
fn a_document_of_another_kind_does_not_validate() {
    let schema = schema();
    assert!(!validator(&schema, "deps").is_valid(&json!({ "schema_version": 1, "kind": "deps" })));
}
