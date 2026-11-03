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
    assert_eq!(report["total_issues"], 0);
    assert_eq!(report["findings"], Value::Array(Vec::new()));

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
    assert_eq!(gate["enforced"], true);
    assert_eq!(gate["observed"], 0.0);
    assert_eq!(gate["threshold_label"], "error");
    assert_eq!(report["gate_outcomes"]["parse-error"]["status"], "pass");
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
    assert!(loud.stdout.contains("No issues found"), "{}", loud.stdout);
    assert!(
        loud.stdout.contains("error-severity-findings: pass"),
        "{}",
        loud.stdout
    );
    assert!(
        loud.stderr.contains("Reading cargo metadata"),
        "{}",
        loud.stderr
    );

    let quiet = check(&fixture, &["--quiet"]);
    assert_eq!(quiet.code, 0);
    assert_eq!(quiet.stdout, loud.stdout);
    assert_eq!(quiet.stderr, "");
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
    assert_eq!(run["tool"]["driver"]["rules"], Value::Array(Vec::new()));
    assert_eq!(run["results"], Value::Array(Vec::new()));
    assert_eq!(run["invocations"][0]["executionSuccessful"], true);
    assert!(run["invocations"][0]["toolExecutionNotifications"].is_null());
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
