//! Black-box tests of the `cargo-fallow` binary.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// A copy of one fixture in a temporary directory.
struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        copy_dir(&Path::new(FIXTURES).join(name), dir.path());
        Self { dir }
    }

    /// A single-package workspace with the given files. Each path is relative
    /// to the workspace root.
    fn package(files: &[(&str, &[u8])]) -> Self {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let manifest = "[package]\nname = \"single\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[workspace]\n";
        fs::write(dir.path().join("Cargo.toml"), manifest).expect("write the manifest");
        for (path, bytes) in files {
            let path = dir.path().join(path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create the file directory");
            }
            fs::write(path, bytes).expect("write a fixture file");
        }
        Self { dir }
    }

    fn manifest(&self) -> PathBuf {
        self.dir.path().join("Cargo.toml")
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create the fixture directory");
    for entry in fs::read_dir(from).expect("read the fixture directory") {
        let entry = entry.expect("read a fixture entry");
        // A local `cargo` run in a fixture leaves a build directory there.
        // It is not part of the fixture.
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

/// The binary, called the way Cargo calls it: with `fallow` as argv[1].
fn cargo_fallow() -> Command {
    let mut command = Command::cargo_bin("cargo-fallow").expect("find the binary");
    command.arg("fallow");
    command
}

/// The binary, called directly.
fn direct() -> Command {
    Command::cargo_bin("cargo-fallow").expect("find the binary")
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).expect("stdout is JSON")
    }
}

fn run(mut command: Command, args: &[&str]) -> Run {
    let output = command.args(args).output().expect("run the binary");
    Run {
        code: output.status.code().expect("the process exits with a code"),
        stdout: String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    }
}

fn check(fixture: &Fixture, extra: &[&str]) -> Run {
    let manifest = fixture.manifest();
    let manifest = manifest.to_str().expect("the path is UTF-8");
    let mut args = vec!["check", "--manifest-path", manifest];
    args.extend_from_slice(extra);
    run(cargo_fallow(), &args)
}

fn assert_ok_report(report: &Value) {
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["kind"], "check");
    assert_eq!(report["language"], "rust");
    assert_eq!(report["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        report["checks"],
        serde_json::json!([
            "parse",
            "deps",
            "files",
            "visibility",
            "boundaries",
            "size",
            "complexity",
            "dupes"
        ])
    );
    // The fixture has one `pub` function that no code reaches.
    assert_eq!(report["total_issues"], 1);
    let finding = &report["findings"][0];
    assert_eq!(finding["check"], "visibility");
    assert_eq!(finding["rule"], "dead-public");
    assert_eq!(finding["location"]["path"], "crates/alpha-lib/src/lib.rs");
    assert_eq!(finding["id"].as_str().map(str::len), Some(16));
    assert_eq!(report["health"]["language"], "rust");
    assert_eq!(report["health"]["penalties"]["dead_public"], 15.0);
    assert_eq!(report["health"]["score"], 85.0);
    assert_eq!(report["health"]["grade"], "A");
    let sections: Vec<&String> = report["sections"]
        .as_object()
        .expect("sections is an object")
        .keys()
        .collect();
    assert_eq!(
        sections,
        [
            "deps",
            "files",
            "visibility",
            "boundaries",
            "size",
            "complexity",
            "dupes"
        ]
    );
    assert_eq!(report["sections"]["visibility"]["kind"], "visibility");

    let members = report["workspace"]["members"]
        .as_array()
        .expect("members is an array");
    let names: Vec<&str> = members
        .iter()
        .map(|member| member["name"].as_str().expect("name is a string"))
        .collect();
    assert_eq!(names, ["alpha-lib", "zeta-cli"]);
    assert_eq!(members[0]["manifest_path"], "crates/alpha-lib/Cargo.toml");
    assert_eq!(members[1]["version"], "0.2.0");

    let targets = members[1]["targets"]
        .as_array()
        .expect("targets is an array");
    let pairs: Vec<(String, String, String)> = targets
        .iter()
        .map(|target| {
            (
                target["kind"][0].as_str().expect("kind").to_owned(),
                target["name"].as_str().expect("name").to_owned(),
                target["src_path"].as_str().expect("src_path").to_owned(),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        [
            (
                "bin".to_owned(),
                "zeta-cli".to_owned(),
                "crates/zeta-cli/src/main.rs".to_owned()
            ),
            (
                "test".to_owned(),
                "smoke".to_owned(),
                "crates/zeta-cli/tests/smoke.rs".to_owned()
            ),
        ]
    );
    assert_eq!(targets[0]["edition"], "2021");

    let gate = &report["gate_outcomes"]["error-severity-findings"];
    assert_eq!(gate["status"], "pass");
    assert_eq!(gate["enforced"], false);
    assert_eq!(gate["observed"], 0.0);
    assert_eq!(gate["threshold_label"], "error");
    assert_eq!(report["gate_outcomes"]["parse-error"]["status"], "pass");
    let visibility = &report["gate_outcomes"]["visibility-findings"];
    assert_eq!(visibility["status"], "fail");
    assert_eq!(visibility["enforced"], false);
}

#[test]
fn json_report_for_a_clean_workspace() {
    let fixture = Fixture::new("ws-ok");
    let result = check(&fixture, &["--format", "json", "--quiet"]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    assert_ok_report(&result.json());
    assert_eq!(result.stderr, "");
}

#[test]
fn direct_call_without_the_cargo_argument() {
    let fixture = Fixture::new("ws-ok");
    let manifest = fixture.manifest();
    let manifest = manifest.to_str().expect("the path is UTF-8");
    let result = run(
        direct(),
        &[
            "check",
            "--manifest-path",
            manifest,
            "--format",
            "json",
            "--quiet",
        ],
    );
    assert_eq!(result.code, 0, "{}", result.stderr);
    assert_ok_report(&result.json());
}

#[test]
fn check_is_the_default_command() {
    let fixture = Fixture::new("ws-ok");
    let manifest = fixture.manifest();
    let manifest = manifest.to_str().expect("the path is UTF-8");
    let result = run(
        cargo_fallow(),
        &["--manifest-path", manifest, "--format=json", "--quiet"],
    );
    assert_eq!(result.code, 0, "{}", result.stderr);
    assert_ok_report(&result.json());
}

#[test]
fn human_summary_and_progress() {
    let fixture = Fixture::new("ws-ok");
    let loud = check(&fixture, &[]);
    assert_eq!(loud.code, 0, "{}", loud.stderr);
    assert!(
        loud.stdout.contains("2 members, 3 targets"),
        "{}",
        loud.stdout
    );
    assert!(loud.stdout.contains("1 issue\n"), "{}", loud.stdout);
    assert!(
        loud.stdout.contains("Health (rust): 85.0 (A)"),
        "{}",
        loud.stdout
    );
    assert!(loud.stdout.contains("\n== dupes ==\n"), "{}", loud.stdout);
    // One gate block for the run, not one for each check.
    assert_eq!(loud.stdout.matches("Gates:").count(), 1, "{}", loud.stdout);
    assert!(
        loud.stdout.contains("error-severity-findings: pass"),
        "{}",
        loud.stdout
    );
    assert!(
        loud.stderr
            .contains("Resolving the workspace with cargo metadata"),
        "{}",
        loud.stderr
    );

    let quiet = check(&fixture, &["--quiet"]);
    assert_eq!(quiet.code, 0);
    assert_eq!(quiet.stdout, loud.stdout);
    assert_eq!(quiet.stderr, "");
}

/// The checks of `check` run in parallel threads, so a start line does not
/// show when the phase before it ended. Each phase writes its time when it
/// ends (fallow-rs/cargo-fallow#147).
#[test]
fn each_phase_of_check_writes_its_time_to_stderr() {
    let fixture = Fixture::new("ws-structure");
    let run = check(&fixture, &["--format", "json"]);
    for phase in [
        "Resolving the workspace with cargo metadata",
        "Parsing target root files",
        "Building the module graph",
        "Checking the dependencies",
        "Walking the module trees",
        "Checking the visibility",
        "Checking the boundary rules",
        "Reading the source files",
        "Measuring the files",
        "Measuring the complexity",
        "Finding clones",
    ] {
        let done = format!("{phase}: ");
        let line = run
            .stderr
            .lines()
            .find(|line| line.starts_with(&done))
            .unwrap_or_else(|| panic!("no time for `{phase}`:\n{}", run.stderr));
        let time = line
            .strip_prefix(&done)
            .and_then(|rest| rest.strip_suffix(" s"))
            .and_then(|seconds| seconds.parse::<f64>().ok());
        assert!(time.is_some(), "{line}");
    }
    // The time lines go to stderr, so the report stays the same.
    let quiet = check(&fixture, &["--format", "json", "--quiet"]);
    assert_eq!(quiet.stdout, run.stdout);
}

#[test]
fn a_gate_finding_is_an_error_in_json_and_in_sarif() {
    let fixture = Fixture::new("ws-ok");
    let json = check(&fixture, &["--format", "json", "--quiet"]).json();
    let findings = json["findings"].as_array().expect("an array");
    assert!(!findings.is_empty());
    assert!(
        findings
            .iter()
            .all(|finding| finding["severity"] == "error")
    );
    // The SARIF level follows the severity, also when no gate is enforced.
    let sarif = check(&fixture, &["--format", "sarif", "--quiet"]).json();
    assert_eq!(sarif["runs"][0]["results"][0]["level"], "error");
}

/// The `(rule, level)` pairs of the SARIF results, sorted.
fn sarif_levels(log: &Value) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = log["runs"][0]["results"]
        .as_array()
        .expect("the results")
        .iter()
        .map(|result| {
            let text = |key: &str| result[key].as_str().unwrap_or_default().to_owned();
            (text("ruleId"), text("level"))
        })
        .collect();
    pairs.sort();
    pairs
}

#[test]
fn each_sarif_level_follows_the_severity_of_its_finding() {
    let fixture = Fixture::new("ws-structure");
    let json = check(&fixture, &["--format", "json", "--quiet"]).json();
    let mut wanted: Vec<(String, String)> = json["findings"]
        .as_array()
        .expect("the findings")
        .iter()
        .map(|finding| {
            let level = match finding["severity"].as_str() {
                Some("error") => "error",
                _ => "warning",
            };
            let rule = finding["rule"].as_str().unwrap_or_default();
            (rule.to_owned(), level.to_owned())
        })
        .collect();
    wanted.sort();
    assert!(wanted.iter().any(|(_, level)| level == "error"), "{json:#}");
    // No gate flag: each gate is not enforced.
    let sarif = check(&fixture, &["--format", "sarif", "--quiet"]).json();
    assert_eq!(sarif_levels(&sarif), wanted, "{sarif:#}");
    // An enforced gate does not change the level of a finding.
    let enforced = check(
        &fixture,
        &["--format", "sarif", "--quiet", "--fail-on-issues"],
    )
    .json();
    assert_eq!(sarif_levels(&enforced), wanted, "{enforced:#}");
}

#[test]
fn sarif_log_for_a_clean_workspace() {
    let fixture = Fixture::new("ws-ok");
    let result = check(&fixture, &["--format", "sarif", "--quiet"]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    let log = result.json();
    assert_eq!(log["version"], "2.1.0");
    assert!(
        log["$schema"]
            .as_str()
            .is_some_and(|uri| uri.contains("sarif"))
    );
    let run = &log["runs"][0];
    assert_eq!(run["tool"]["driver"]["name"], "cargo-fallow");
    assert_eq!(run["tool"]["driver"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(run["tool"]["driver"]["rules"][0]["id"], "dead-public");
    assert_eq!(run["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(run["results"][0]["level"], "error");
    assert_eq!(run["invocations"][0]["executionSuccessful"], true);
    let notes = &run["invocations"][0]["toolExecutionNotifications"];
    assert_eq!(notes[0]["descriptor"]["id"], "visibility-findings");
}

#[test]
fn fail_on_issues_enforces_the_gate_of_each_check_that_ran() {
    let fixture = Fixture::new("ws-ok");
    let result = check(
        &fixture,
        &["--format", "json", "--quiet", "--fail-on-issues"],
    );
    assert_eq!(result.code, 1, "{}", result.stderr);
    let gates = result.json()["gate_outcomes"].clone();
    for name in [
        "boundary-violations",
        "duplication-findings",
        "error-severity-findings",
        "health-findings",
        "parse-error",
        "visibility-findings",
    ] {
        assert_eq!(gates[name]["enforced"], true, "{name}");
    }
    // `size` has no old version to compare with in a `check` run.
    assert_eq!(gates["file-growth"]["status"], "skipped");
}

#[test]
fn skip_leaves_a_check_and_its_gate_out() {
    let fixture = Fixture::new("ws-ok");
    let result = check(
        &fixture,
        &[
            "--format",
            "json",
            "--quiet",
            "--fail-on-issues",
            "--skip",
            "visibility",
        ],
    );
    assert_eq!(result.code, 0, "{}", result.stderr);
    let report = result.json();
    assert!(report["gate_outcomes"]["visibility-findings"].is_null());
    assert!(report["sections"]["visibility"].is_null());
    assert_eq!(report["total_issues"], 0);
    assert!(report["health"]["penalties"]["dead_public"].is_null());
}

#[test]
fn only_runs_the_named_checks() {
    let fixture = Fixture::new("ws-ok");
    let result = check(
        &fixture,
        &["--format", "json", "--quiet", "--only", "parse,dupes"],
    );
    assert_eq!(result.code, 0, "{}", result.stderr);
    let report = result.json();
    assert_eq!(report["checks"], serde_json::json!(["parse", "dupes"]));
    let gates: Vec<&String> = report["gate_outcomes"]
        .as_object()
        .expect("gate_outcomes is an object")
        .keys()
        .collect();
    assert_eq!(gates, ["duplication-findings", "parse-error"]);
    assert_eq!(
        report["health"]["penalties"],
        serde_json::json!({ "duplication": 0.0 })
    );
}

#[test]
fn a_partial_run_has_no_score_and_names_the_missing_checks() {
    let fixture = Fixture::new("ws-ok");
    let report = check(
        &fixture,
        &[
            "--format",
            "json",
            "--quiet",
            "--offline",
            "--only",
            "deps,visibility",
        ],
    )
    .json();
    let health = &report["health"];
    assert!(health.get("score").is_none(), "{health}");
    assert!(health.get("grade").is_none(), "{health}");
    assert_eq!(
        health["missing_checks"],
        serde_json::json!(["boundaries", "size", "complexity", "dupes"])
    );
    assert_eq!(health["penalties"]["dead_public"], 15.0);
    let human = check(
        &fixture,
        &["--quiet", "--offline", "--only", "deps,visibility"],
    )
    .stdout;
    assert!(
        human.contains(
            "Health (rust): no score. The run did not include boundaries, size, complexity, dupes."
        ),
        "{human}"
    );
}

#[test]
fn a_skipped_check_takes_the_score_and_a_run_without_penalties_has_no_health() {
    let fixture = Fixture::new("ws-ok");
    let report = check(
        &fixture,
        &[
            "--format",
            "json",
            "--quiet",
            "--offline",
            "--skip",
            "dupes",
        ],
    )
    .json();
    let health = &report["health"];
    assert!(health.get("score").is_none(), "{health}");
    assert_eq!(health["missing_checks"], serde_json::json!(["dupes"]));
    let report = check(
        &fixture,
        &["--format", "json", "--quiet", "--only", "parse"],
    )
    .json();
    assert!(report.get("health").is_none(), "{report}");
}

#[test]
fn only_and_skip_that_leave_no_check_exit_2() {
    let fixture = Fixture::new("ws-ok");
    let result = check(
        &fixture,
        &[
            "--format",
            "json",
            "--quiet",
            "--skip",
            "parse,deps,files,visibility,boundaries,size,complexity,dupes",
        ],
    );
    assert_json_error(&result, "CARGO_FALLOW_INVALID_ARGUMENT");
}

/// The JSON report and the SARIF results of `check`, and of each single
/// command, on one fixture.
fn assert_combined_equals_single(fixture: &str) {
    let fixture = Fixture::new(fixture);
    let combined = check(&fixture, &["--format", "json", "--quiet"]).json();
    let sarif = check(&fixture, &["--format", "sarif", "--quiet"]).json();
    let issues = check(&fixture, &["--format", "codeclimate", "--quiet"]).json();
    let manifest = fixture.manifest();
    let manifest = manifest.to_str().expect("the path is UTF-8");
    let mut single_results = Vec::new();
    for name in [
        "deps",
        "visibility",
        "boundaries",
        "size",
        "complexity",
        "dupes",
    ] {
        let args = |format: &'static str| {
            [
                name,
                "--manifest-path",
                manifest,
                "--format",
                format,
                "--quiet",
            ]
        };
        let single = run(cargo_fallow(), &args("json"));
        assert_eq!(combined["sections"][name], single.json(), "{name}");
        let log = run(cargo_fallow(), &args("sarif")).json();
        single_results.extend(
            log["runs"][0]["results"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        );
    }
    assert_eq!(sarif["runs"][0]["results"], Value::Array(single_results));
    let total = combined["total_issues"].as_u64().map(|n| n as usize);
    assert_eq!(issues.as_array().map(Vec::len), total);
    let mut fingerprints: Vec<&str> = issues
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|issue| issue["fingerprint"].as_str())
        .collect();
    fingerprints.sort_unstable();
    fingerprints.dedup();
    assert_eq!(
        Some(fingerprints.len()),
        total,
        "each fingerprint is unique"
    );
}

#[test]
fn combined_report_equals_the_single_commands() {
    assert_combined_equals_single("ws-ok");
    assert_combined_equals_single("ws-structure");
}

#[test]
fn codeclimate_report_has_one_issue_for_each_finding_with_a_stable_fingerprint() {
    let fixture = Fixture::new("ws-ok");
    let first = check(&fixture, &["--format", "codeclimate", "--quiet"]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let issues = first.json();
    assert_eq!(issues.as_array().map(Vec::len), Some(1));
    let issue = &issues[0];
    assert_eq!(issue["type"], "issue");
    assert_eq!(issue["check_name"], "fallow/dead-public");
    assert_eq!(issue["categories"], serde_json::json!(["Bug Risk"]));
    assert_eq!(issue["severity"], "minor");
    assert_eq!(issue["location"]["path"], "crates/alpha-lib/src/lib.rs");
    assert_eq!(issue["location"]["lines"]["begin"], 4);
    let json = check(&fixture, &["--format", "json", "--quiet"]).json();
    assert_eq!(issue["fingerprint"], json["findings"][0]["id"]);
    let second = check(&fixture, &["--format", "codeclimate", "--quiet"]);
    assert_eq!(first.stdout, second.stdout);
}

#[test]
fn each_single_command_writes_codeclimate() {
    let fixture = Fixture::new("ws-ok");
    let manifest = fixture.manifest();
    let manifest = manifest.to_str().expect("the path is UTF-8");
    for name in [
        "deps",
        "visibility",
        "boundaries",
        "size",
        "complexity",
        "dupes",
    ] {
        let result = run(
            cargo_fallow(),
            &[
                name,
                "--manifest-path",
                manifest,
                "--format",
                "codeclimate",
                "--quiet",
            ],
        );
        assert_eq!(result.code, 0, "{name}: {}", result.stderr);
        let expected = usize::from(name == "visibility");
        assert_eq!(
            result.json().as_array().map(Vec::len),
            Some(expected),
            "{name}"
        );
    }
}

#[test]
fn parse_error_is_reported_but_not_enforced_by_default() {
    let fixture = Fixture::new("ws-parse-error");
    let result = check(&fixture, &["--format", "json", "--quiet"]);
    assert_eq!(result.code, 0, "{}", result.stderr);
    let gate = &result.json()["gate_outcomes"]["parse-error"];
    assert_eq!(gate["status"], "fail");
    assert_eq!(gate["enforced"], false);
    assert_eq!(gate["observed"], 1.0);
}

#[test]
fn enforced_parse_error_exits_1_in_json() {
    let fixture = Fixture::new("ws-parse-error");
    let result = check(
        &fixture,
        &["--format", "json", "--quiet", "--fail-on-parse-error"],
    );
    assert_eq!(result.code, 1, "{}", result.stderr);
    let gate = &result.json()["gate_outcomes"]["parse-error"];
    assert_eq!(gate["status"], "fail");
    assert_eq!(gate["enforced"], true);
    assert_eq!(
        gate["files"],
        serde_json::json!([{ "path": "broken/src/lib.rs", "error_count": 3, "panicked": false }])
    );
}

#[test]
fn enforced_parse_error_exits_1_in_human_and_sarif() {
    let fixture = Fixture::new("ws-parse-error");
    let human = check(&fixture, &["--quiet", "--fail-on-parse-error"]);
    assert_eq!(human.code, 1, "{}", human.stderr);
    assert!(
        human.stdout.contains("parse-error: fail (1 file)"),
        "{}",
        human.stdout
    );
    assert!(
        human.stdout.contains("broken/src/lib.rs"),
        "{}",
        human.stdout
    );

    let sarif = check(
        &fixture,
        &["--format", "sarif", "--quiet", "--fail-on-parse-error"],
    );
    assert_eq!(sarif.code, 1, "{}", sarif.stderr);
    assert_eq!(sarif.json()["version"], "2.1.0");
}

#[test]
fn sarif_log_shows_each_file_of_a_failed_gate() {
    let fixture = Fixture::new("ws-parse-error");
    for (flags, level) in [
        (&["--fail-on-parse-error"][..], "error"),
        (&[][..], "warning"),
    ] {
        let mut args = vec!["--format", "sarif", "--quiet"];
        args.extend_from_slice(flags);
        let result = check(&fixture, &args);
        let run = &result.json()["runs"][0];
        assert_eq!(
            run["results"],
            serde_json::json!([{
                "ruleId": "parse-error",
                "level": level,
                "message": { "text": "The file does not parse." },
                "locations": [{
                    "physicalLocation": {
                        "artifactLocation": { "uri": "broken/src/lib.rs" }
                    }
                }]
            }]),
            "{flags:?}"
        );
        assert_eq!(run["tool"]["driver"]["rules"][0]["id"], "parse-error");
        let invocation = &run["invocations"][0];
        assert_eq!(invocation["executionSuccessful"], true);
        let notification = &invocation["toolExecutionNotifications"][0];
        assert_eq!(notification["level"], level);
        assert_eq!(notification["descriptor"]["id"], "parse-error");
    }
}

#[test]
fn unreadable_root_file_is_a_warning_and_not_a_parse_error() {
    // A source file that is not UTF-8 cannot be read as Rust source. The
    // gate contract has no reason for such a file, so the gate does not
    // judge it, the same as fallow with `source-read-failure`.
    let fixture = Fixture::package(&[("src/lib.rs", &[0xff, 0xfe, 0x00])]);
    let result = check(
        &fixture,
        &["--format", "json", "--quiet", "--fail-on-parse-error"],
    );
    assert_eq!(result.code, 0, "{}", result.stderr);
    let gate = &result.json()["gate_outcomes"]["parse-error"];
    assert_eq!(gate["status"], "pass");
    assert_eq!(gate["observed"], 0.0);
    assert!(gate["files"].is_null(), "{gate}");
    assert!(
        result.stderr.contains(
            "warning: cannot read `src/lib.rs`, so the parse-error gate does not judge it:"
        ),
        "{}",
        result.stderr
    );
}

/// Repeats `open`, then `middle`, then `close`, each `depth` times.
fn nested(open: &str, middle: &str, close: &str, depth: usize) -> Vec<u8> {
    format!("{}{middle}{}\n", open.repeat(depth), close.repeat(depth)).into_bytes()
}

/// A `type` item with `depth` levels of `Box<`.
fn nested_type(depth: usize) -> Vec<u8> {
    nested("type T = ", "", "", 1)
        .into_iter()
        .chain(nested("Box<", "u8", ">", depth))
        .chain(*b";\n")
        .collect()
}

#[test]
fn nesting_under_the_limit_parses_without_a_stack_overflow() {
    let fixture = Fixture::package(&[("src/lib.rs", &nested_type(500))]);
    let result = check(
        &fixture,
        &["--format", "json", "--quiet", "--fail-on-parse-error"],
    );
    assert_eq!(result.code, 0, "{}", result.stderr);
    let gate = &result.json()["gate_outcomes"]["parse-error"];
    assert_eq!(gate["status"], "pass");
}

#[test]
fn nesting_over_the_limit_is_too_deep_for_valid_and_invalid_source() {
    let unclosed = nested("type T = ", "", "", 1)
        .into_iter()
        .chain(nested("&", "", "", 20_000))
        .collect::<Vec<u8>>();
    for source in [nested_type(2000), unclosed] {
        let fixture = Fixture::package(&[("src/lib.rs", &source)]);
        let result = check(
            &fixture,
            &["--format", "json", "--quiet", "--fail-on-parse-error"],
        );
        assert_eq!(result.code, 1, "{}", result.stderr);
        let gate = &result.json()["gate_outcomes"]["parse-error"];
        assert_eq!(
            gate["files"],
            serde_json::json!([{
                "path": "src/lib.rs",
                "error_count": 1,
                "panicked": true,
                "reason": "nesting-too-deep",
            }])
        );
    }
}

#[test]
fn deep_tree_from_an_operator_chain_is_too_deep_and_does_not_abort() {
    let source = format!("fn f() -> u32 {{ 1{} }}\n", " + 1".repeat(20_000));
    let fixture = Fixture::package(&[("src/lib.rs", source.as_bytes())]);
    for _ in 0..2 {
        let result = check(
            &fixture,
            &["--format", "json", "--quiet", "--fail-on-parse-error"],
        );
        assert_eq!(result.code, 1, "{}", result.stderr);
        let gate = &result.json()["gate_outcomes"]["parse-error"];
        assert_eq!(gate["files"][0]["reason"], "nesting-too-deep", "{gate}");
    }
}

/// The parse stack grows with the nesting depth, not with the file size. A
/// stack of 4 KiB for each source byte asks for 32 GiB here. That is over the
/// stack cap, and Linux can refuse it, so the file then counts as too deep.
/// CI runs this test on Linux.
#[test]
fn large_flat_generated_root_file_parses() {
    use std::fmt::Write as _;
    const SIZE: usize = 8 << 20;
    let mut source = String::from("// @generated\npub static TABLE: &[u32] = &[");
    for value in 0..100_000 {
        let _ = write!(source, "{value}, ");
    }
    source.push_str("];\n");
    let mut index = 0;
    while source.len() < SIZE {
        let _ = write!(
            source,
            "pub const C{index}: [u32; 2] = [{index}, 1];\n\
             pub fn f{index}(a: u32) -> u32 {{ if a > {index} {{ a }} else {{ a + 1 }} }}\n\
             pub struct S{index} {{ pub a: Vec<u32>, b: Option<u8> }}\n"
        );
        index += 1;
    }
    let fixture = Fixture::package(&[("src/lib.rs", source.as_bytes())]);
    let result = check(
        &fixture,
        &[
            "--only",
            "parse",
            "--format",
            "json",
            "--quiet",
            "--fail-on-parse-error",
        ],
    );
    assert_eq!(result.code, 0, "{}", result.stderr);
    let gate = &result.json()["gate_outcomes"]["parse-error"];
    assert_eq!(gate["status"], "pass", "{gate}");
    assert_eq!(gate["observed"], 0.0, "{gate}");
}

#[test]
fn parser_recovers_and_counts_each_error() {
    let fixture = Fixture::package(&[("src/lib.rs", b"fn a( {}\nfn b( {}\nfn c() {}\n")]);
    let result = check(
        &fixture,
        &["--format", "json", "--quiet", "--fail-on-parse-error"],
    );
    assert_eq!(result.code, 1, "{}", result.stderr);
    let file = &result.json()["gate_outcomes"]["parse-error"]["files"][0];
    assert_eq!(file["panicked"], false, "{file}");
    assert!(
        file["error_count"].as_u64().is_some_and(|count| count >= 2),
        "{file}"
    );
}

#[test]
fn crlf_string_literal_parses() {
    let fixture = Fixture::package(&[("src/lib.rs", b"pub const S: &str = \"a\r\nb\";\r\n")]);
    let result = check(
        &fixture,
        &["--format", "json", "--quiet", "--fail-on-parse-error"],
    );
    assert_eq!(result.code, 0, "{}", result.stderr);
}

fn assert_json_error(result: &Run, code: &str) {
    assert_eq!(result.code, 2, "{}", result.stderr);
    let error = result.json();
    assert_eq!(error["error"], true);
    assert_eq!(error["exit_code"], 2);
    assert_eq!(error["code"], code);
    assert!(
        error["message"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "{error}"
    );
}

#[test]
fn missing_manifest_exits_2_in_json() {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let missing = dir.path().join("Cargo.toml");
    let missing = missing.to_str().expect("the path is UTF-8");
    let result = run(
        cargo_fallow(),
        &[
            "check",
            "--manifest-path",
            missing,
            "--format",
            "json",
            "--quiet",
        ],
    );
    assert_json_error(&result, "CARGO_FALLOW_MANIFEST_NOT_FOUND");
}

#[test]
fn bad_manifest_exits_2_in_json() {
    let fixture = Fixture::new("bad-manifest");
    let result = check(&fixture, &["--format", "json", "--quiet"]);
    assert_json_error(&result, "CARGO_FALLOW_METADATA_FAILED");
}

#[test]
fn invalid_format_exits_2_in_json() {
    let result = run(
        cargo_fallow(),
        &["check", "--format", "xml", "--format=json"],
    );
    assert_json_error(&result, "CARGO_FALLOW_INVALID_ARGUMENT");
}

#[test]
fn unknown_flag_exits_2_in_json() {
    let result = run(cargo_fallow(), &["--format", "json", "--bogus"]);
    assert_json_error(&result, "CARGO_FALLOW_INVALID_ARGUMENT");
}

#[test]
fn errors_go_to_stderr_in_human_and_sarif() {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let missing = dir.path().join("Cargo.toml");
    let missing = missing.to_str().expect("the path is UTF-8");
    for format in ["human", "sarif"] {
        let result = run(
            cargo_fallow(),
            &[
                "check",
                "--manifest-path",
                missing,
                "--format",
                format,
                "--quiet",
            ],
        );
        assert_eq!(result.code, 2, "{format}");
        assert!(
            result.stderr.contains("error:"),
            "{format}: {}",
            result.stderr
        );
        assert_eq!(result.stdout, "", "{format}");
    }
    let usage = run(cargo_fallow(), &["check", "--format", "xml"]);
    assert_eq!(usage.code, 2);
    assert!(usage.stderr.contains("error:"), "{}", usage.stderr);
    assert_eq!(usage.stdout, "");
}

#[test]
fn help_and_version_exit_0() {
    let help = run(cargo_fallow(), &["--help"]);
    assert_eq!(help.code, 0);
    assert!(help.stdout.contains("--manifest-path"), "{}", help.stdout);
    let version = run(direct(), &["--version"]);
    assert_eq!(version.code, 0);
    assert!(
        version.stdout.contains(env!("CARGO_PKG_VERSION")),
        "{}",
        version.stdout
    );
}

#[test]
fn json_output_is_deterministic() {
    let fixture = Fixture::new("ws-ok");
    let first = check(&fixture, &["--format", "json", "--quiet"]);
    let second = check(&fixture, &["--format", "json", "--quiet"]);
    assert_eq!(first.code, 0);
    assert_eq!(first.stdout, second.stdout);
}

#[test]
fn package_never_installs_a_binary_named_fallow() {
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .no_deps()
        .exec()
        .expect("read the metadata of this package");
    let package = metadata
        .root_package()
        .expect("the manifest has a root package");
    let bins: Vec<&str> = package
        .targets
        .iter()
        .filter(|target| target.is_bin())
        .map(|target| target.name.as_str())
        .collect();
    assert_eq!(bins, ["cargo-fallow"]);
}

const COMPLEX_MANIFEST: &str = "[package]\nname = \"single\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[workspace]\n\n[workspace.metadata.fallow.complexity]\nmax-cyclomatic = 1\n";
const COMPLEX_FN: &str = "pub fn f(a: u8) -> u8 { if a == 0 { 1 } else { 2 } }\n";

fn complexity_ids(source: &str) -> Vec<String> {
    let fixture = Fixture::package(&[
        ("Cargo.toml", COMPLEX_MANIFEST.as_bytes()),
        ("src/lib.rs", source.as_bytes()),
    ]);
    let report = check(
        &fixture,
        &["--format", "json", "--quiet", "--only", "complexity"],
    )
    .json();
    report["findings"]
        .as_array()
        .expect("findings is an array")
        .iter()
        .map(|finding| finding["id"].as_str().expect("id is a string").to_owned())
        .collect()
}

#[test]
fn a_finding_id_stays_when_a_line_above_changes() {
    let before = complexity_ids(COMPLEX_FN);
    let after = complexity_ids(&format!("// a new comment\n\n{COMPLEX_FN}"));
    assert_eq!(before.len(), 1);
    assert_eq!(before, after);
    // Two functions with one name in one file keep two ids.
    let twice = complexity_ids(&format!(
        "mod a {{ {COMPLEX_FN} }}\nmod b {{ {COMPLEX_FN} }}\n"
    ));
    assert_eq!(twice.len(), 2);
    assert_ne!(twice[0], twice[1]);
}

#[test]
fn an_unused_workspace_dependency_does_not_count_against_member_dependencies() {
    let manifest = "[package]\nname = \"single\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[workspace]\n\n[workspace.dependencies]\nnothing-uses-this = \"1\"\n";
    let fixture = Fixture::package(&[
        ("Cargo.toml", manifest.as_bytes()),
        ("src/lib.rs", b"pub fn f() {}\n"),
    ]);
    let report = check(
        &fixture,
        &["--format", "json", "--quiet", "--offline", "--only", "deps"],
    )
    .json();
    let finding = &report["sections"]["deps"]["findings"][0];
    assert_eq!(finding["rule"], "unused-workspace-dependency");
    let health = &report["health"];
    assert_eq!(health["metrics"]["unused_deps"], 0);
    assert_eq!(health["metrics"]["declared_dependencies"], 0);
    assert_eq!(health["penalties"]["unused_deps"], 0.0);
}
