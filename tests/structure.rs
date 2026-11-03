//! Black-box tests of the structure checks: `boundaries`, `size`,
//! `complexity` and `dupes`.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// A copy of the `ws-structure` fixture in a temporary directory.
struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        copy_dir(&Path::new(FIXTURES).join("ws-structure"), dir.path());
        Self { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn manifest(&self) -> PathBuf {
        self.root().join("Cargo.toml")
    }

    /// Runs one subcommand with `--format <format>`, from the workspace root.
    fn run(&self, args: &[&str]) -> Run {
        let output = Command::cargo_bin("cargo-fallow")
            .expect("find the binary")
            .current_dir(self.root())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .arg("fallow")
            .args(args)
            .arg("--manifest-path")
            .arg(self.manifest())
            .arg("--quiet")
            .output()
            .expect("run cargo-fallow");
        Run {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        }
    }

    fn json(&self, args: &[&str]) -> (i32, Value) {
        let mut all = args.to_vec();
        all.extend(["--format", "json"]);
        let run = self.run(&all);
        let value = serde_json::from_str(&run.stdout).expect("the output is JSON");
        (run.code, value)
    }

    fn git(&self, args: &[&str]) {
        let status = StdCommand::new("git")
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
        assert!(status.status.success(), "git {args:?} failed: {status:?}");
    }

    fn write(&self, path: &str, text: &str) {
        fs::write(self.root().join(path), text).expect("write a file");
    }
}

struct Run {
    code: i32,
    stdout: String,
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

fn strings(value: &Value, key: &str) -> Vec<String> {
    value["findings"]
        .as_array()
        .expect("findings is an array")
        .iter()
        .map(|finding| finding[key].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn boundaries_report_each_rule_kind_and_gate_only_with_the_flag() {
    let fixture = Fixture::new();
    let (code, report) = fixture.json(&["boundaries"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["kind"], "boundaries");
    assert_eq!(report["schema_version"], 1);
    let rules = strings(&report, "rule");
    assert_eq!(
        rules,
        [
            "process-stays-a-leaf",
            "process-tree-in-process",
            "render-does-not-run",
            "render-does-not-run",
        ],
        "{report:#}"
    );
    let findings = &report["findings"];
    assert_eq!(findings[0]["from"], "process");
    assert_eq!(findings[0]["to"], "core");
    assert_eq!(findings[0]["path"], "crates/process/Cargo.toml");
    assert_eq!(findings[0]["line"], 8);
    assert_eq!(findings[0]["dependency_kinds"][0], "normal");
    assert_eq!(findings[1]["path"], "crates/app/src/lib.rs");
    assert_eq!(findings[1]["line"], 9);
    assert_eq!(findings[1]["pattern"], "process_group(0)");
    assert_eq!(findings[2]["to"], "crate::engine::run");
    assert_eq!(findings[3]["to"], "crate::engine::stop");
    assert_eq!(findings[3]["from"], "crate::render");
    assert_eq!(findings[0]["actions"][0]["type"], "remove-dependency");
    let gate = &report["gate_outcomes"]["boundary-violations"];
    assert_eq!(
        (gate["status"].as_str(), gate["enforced"].as_bool()),
        (Some("fail"), Some(false))
    );

    let (code, report) = fixture.json(&["boundaries", "--fail-on-violations"]);
    assert_eq!(code, 1);
    assert_eq!(
        report["gate_outcomes"]["boundary-violations"]["enforced"],
        true
    );
}

#[test]
fn boundaries_sarif_has_one_result_per_finding_and_a_gate_notification() {
    let fixture = Fixture::new();
    let run = fixture.run(&["boundaries", "--format", "sarif", "--fail-on-violations"]);
    assert_eq!(run.code, 1);
    let log: Value = serde_json::from_str(&run.stdout).expect("SARIF is JSON");
    let results = log["runs"][0]["results"].as_array().expect("results");
    assert_eq!(results.len(), 4);
    assert_eq!(results[0]["level"], "error");
    let notes = &log["runs"][0]["invocations"][0]["toolExecutionNotifications"];
    assert_eq!(notes[0]["descriptor"]["id"], "boundary-violations");
}

#[test]
fn a_rule_with_an_unknown_group_is_an_invalid_config_error() {
    let fixture = Fixture::new();
    let manifest = fs::read_to_string(fixture.manifest()).expect("read the manifest");
    fixture.write("Cargo.toml", &manifest.replace("\"@leaf\"", "\"@missing\""));
    let (code, report) = fixture.json(&["boundaries"]);
    assert_eq!(code, 2);
    assert_eq!(report["code"], "CARGO_FALLOW_INVALID_CONFIG");
    assert!(
        report["message"]
            .as_str()
            .unwrap_or_default()
            .contains("@missing")
    );
}

#[test]
fn an_unknown_config_key_is_an_error_for_its_check_only() {
    let fixture = Fixture::new();
    let manifest = fs::read_to_string(fixture.manifest()).expect("read the manifest");
    fixture.write(
        "Cargo.toml",
        &manifest.replace("max-cyclomatic = 5", "max-cyclomatic = 5\nmax-crap = 1"),
    );
    let (code, report) = fixture.json(&["complexity"]);
    assert_eq!(code, 2);
    assert_eq!(report["code"], "CARGO_FALLOW_INVALID_CONFIG");
    let (code, _) = fixture.json(&["boundaries"]);
    assert_eq!(code, 0);
}

#[test]
fn complexity_reports_the_function_over_the_limits_and_honors_the_suppression() {
    let fixture = Fixture::new();
    let (code, report) = fixture.json(&["complexity"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(strings(&report, "name"), ["branchy"], "{report:#}");
    let finding = &report["findings"][0];
    assert_eq!(finding["path"], "crates/core/src/lib.rs");
    assert_eq!(
        (finding["line"].as_u64(), finding["col"].as_u64()),
        (Some(4), Some(8))
    );
    assert_eq!(
        (
            finding["cyclomatic"].as_u64(),
            finding["cognitive"].as_u64()
        ),
        (Some(7), Some(10))
    );
    assert_eq!(finding["exceeded"], "both");
    assert_eq!(finding["severity"], "moderate");
    assert_eq!(
        finding["actions"][1]["comment"],
        "// fallow-ignore-next-line complexity"
    );
    assert_eq!(report["summary"]["suppressed"], 1);

    let (code, _) = fixture.json(&["complexity", "--fail-on-complexity"]);
    assert_eq!(code, 1);
    let (code, report) = fixture.json(&[
        "complexity",
        "--max-cyclomatic",
        "9",
        "--max-cognitive",
        "10",
        "--fail-on-complexity",
    ]);
    assert_eq!(code, 0, "{report}");
}

#[test]
fn dupes_find_the_copied_function_across_packages_and_gate_on_the_flags() {
    let fixture = Fixture::new();
    let (code, report) = fixture.json(&["dupes", "--min-tokens", "30", "--min-lines", "5"]);
    assert_eq!(code, 0, "{report}");
    let across: Vec<&Value> = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter(|group| {
            let files: Vec<&str> = group["instances"]
                .as_array()
                .map(|all| all.iter().filter_map(|i| i["file"].as_str()).collect())
                .unwrap_or_default();
            files == ["crates/core/src/lib.rs", "crates/process/src/lib.rs"]
        })
        .collect();
    assert_eq!(across.len(), 1, "{report:#}");
    assert_eq!(across[0]["instances"][1]["start_line"], 4);
    assert!(
        report["summary"]["stats"]["duplication_percentage"]
            .as_f64()
            .unwrap_or(0.0)
            > 1.0
    );

    let (code, _) = fixture.json(&["dupes", "--min-tokens", "30", "--fail-on-clones"]);
    assert_eq!(code, 1);
    let (code, report) = fixture.json(&["dupes", "--min-tokens", "30", "--threshold", "99"]);
    assert_eq!(code, 0);
    assert_eq!(
        report["gate_outcomes"]["duplication-threshold"]["status"],
        "pass"
    );
    let (code, _) = fixture.json(&["dupes", "--min-tokens", "30", "--threshold", "1"]);
    assert_eq!(code, 1);
}

#[test]
fn size_without_a_base_lists_hotspots_and_skips_the_growth_gate() {
    let fixture = Fixture::new();
    let (code, report) = fixture.json(&["size"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(
        report["summary"]["large_files"][0]["path"],
        "crates/core/src/lib.rs"
    );
    let names: Vec<&str> = report["summary"]["large_functions"]
        .as_array()
        .map(|all| all.iter().filter_map(|f| f["name"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(names, ["branchy", "quiet"]);
    // The doc comment and the attributes are not part of the function length.
    assert_eq!(report["summary"]["large_functions"][0]["line_count"], 16);
    assert_eq!(report["gate_outcomes"]["file-growth"]["status"], "skipped");

    let (code, report) = fixture.json(&["size", "--fail-on-growth"]);
    assert_eq!(code, 2);
    assert_eq!(report["code"], "CARGO_FALLOW_INVALID_ARGUMENT");
}

#[test]
fn size_against_a_base_fails_a_grown_large_file_and_a_new_large_file() {
    let fixture = Fixture::new();
    fixture.git(&["init", "--quiet"]);
    fixture.git(&["add", "--all"]);
    fixture.git(&["commit", "--quiet", "-m", "base"]);
    fixture.git(&["checkout", "--quiet", "-b", "topic"]);
    let core = fs::read_to_string(fixture.root().join("crates/core/src/lib.rs")).expect("read");
    fixture.write(
        "crates/core/src/lib.rs",
        &format!("{core}\n/// More.\npub fn more() {{}}\n"),
    );
    fixture.write(
        "crates/app/src/engine.rs",
        &format!("{}\n", "// x\n".repeat(60)),
    );
    fixture.write("crates/app/src/tests.rs", &"// x\n".repeat(80));
    fixture.git(&["commit", "--quiet", "--all", "-m", "grow"]);
    let (code, report) = fixture.json(&["size", "--base", "main", "--fail-on-growth"]);
    assert_eq!(code, 1, "{report:#}");
    let messages = strings(&report, "message");
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(
        messages[0].starts_with("crates/app/src/engine.rs: 61 lines, merge base "),
        "{messages:?}"
    );
    assert!(
        messages[1].starts_with("crates/core/src/lib.rs: 51 lines, merge base "),
        "{messages:?}"
    );
    assert!(
        messages[1].ends_with("has 48. A file over 40 lines must not grow."),
        "{messages:?}"
    );

    // A shrink passes. A new file over the limit fails.
    fixture.write("crates/core/src/lib.rs", &core);
    fixture.write(
        "crates/app/src/engine.rs",
        "pub fn run() {}\npub fn stop() {}\npub mod shared { pub fn helper() {} }\n",
    );
    fixture.write("crates/app/src/big.rs", &"// x\n".repeat(41));
    fixture.git(&["add", "--all"]);
    let (code, report) = fixture.json(&["size", "--staged", "--fail-on-growth"]);
    assert_eq!(code, 1, "{report:#}");
    assert_eq!(strings(&report, "kind"), ["new-large-file"]);
    assert_eq!(report["summary"]["compared_with"], "HEAD");
}

#[test]
fn size_with_an_unknown_base_is_an_error() {
    let fixture = Fixture::new();
    fixture.git(&["init", "--quiet"]);
    fixture.git(&["add", "--all"]);
    fixture.git(&["commit", "--quiet", "-m", "base"]);
    let (code, report) = fixture.json(&["size", "--base", "no-such-ref"]);
    assert_eq!(code, 2);
    assert!(
        report["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no-such-ref"),
        "{report}"
    );
}

#[test]
fn each_structure_command_writes_human_output_with_its_gate() {
    let fixture = Fixture::new();
    for (command, gate) in [
        ("boundaries", "boundary-violations"),
        ("size", "file-growth"),
        ("complexity", "health-findings"),
        ("dupes", "duplication-findings"),
    ] {
        let run = fixture.run(&[command]);
        assert_eq!(run.code, 0, "{command}");
        assert!(
            run.stdout.starts_with(&format!("cargo fallow {command}:")),
            "{}",
            run.stdout
        );
        assert!(run.stdout.contains(gate), "{}", run.stdout);
    }
}
