//! Black-box tests of the `mutants` product surface: the exit codes, the formats,
//! the report contract, `--in-diff` and `--inspect`.
//!
//! The fixture has one known killed mutant and one known surviving mutant. Each
//! mutated process runs under the memory guard and a time limit. The memory guard
//! has no probe on Windows, so the file is for Unix only.
#![cfg(unix)]
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::path::Path;
use std::process::{Command as Process, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

const MANIFEST: &str = "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\npublish = false\n\n[workspace]\n";

/// `check` has the known killed mutant on line 3. `unseen` has the known surviving
/// mutant on line 8. `orphan` has a mutant that no test executes, on line 13.
const KNOWN: &str = r"/// Has a test.
pub fn check(a: u8) -> bool {
    a == 1
}

/// Has a test that sees no difference.
pub fn unseen(a: u8) -> bool {
    a == 2
}

/// Has no test.
pub fn orphan(a: u8) -> bool {
    a == 3
}

#[cfg(test)]
mod tests {
    #[test]
    fn check_works() {
        assert!(super::check(1));
        assert!(!super::check(2));
        let _ = super::unseen(1);
    }
}
";

const ONLY_KILLED: &str = r"pub fn check(a: u8) -> bool {
    a == 1
}

#[cfg(test)]
mod tests {
    #[test]
    fn check_works() {
        assert!(super::check(1));
        assert!(!super::check(2));
    }
}
";

fn fixture(files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    fs::write(dir.path().join("Cargo.toml"), MANIFEST).expect("write the manifest");
    write_files(dir.path(), files);
    dir
}

fn write_files(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().expect("a file has a parent")).expect("create the parent");
        fs::write(path, text).expect("write a file");
    }
}

struct Output {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Output {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|err| panic!("{err}: {}\n{}", self.stdout, self.stderr))
    }
}

/// One run at a time: each run has up to two test processes, and that is the
/// limit for this machine. `.config/nextest.toml` gives the same limit to nextest.
static ONE_RUN: Mutex<()> = Mutex::new(());

/// Runs `cargo-fallow mutants` on the manifest of `dir`.
fn mutants(dir: &Path, flags: &[&str]) -> Output {
    let _one_run = ONE_RUN.lock().unwrap_or_else(PoisonError::into_inner);
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .env("MBX_DISABLE", "1")
        .args(["mutants", "--quiet", "--jobs", "2", "--manifest-path"])
        .arg(dir.join("Cargo.toml"))
        .args(flags)
        .timeout(Duration::from_secs(280))
        .output()
        .expect("the binary runs");
    Output {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// The schema tier has exactly the mutants of the fixture comments.
const SCHEMA: [&str; 2] = ["--tier", "schema"];

fn with(flags: &[&'static str]) -> Vec<&'static str> {
    SCHEMA.iter().chain(flags).copied().collect()
}

fn by_status<'a>(report: &'a Value, status: &str) -> Vec<&'a Value> {
    report["mutants"]
        .as_array()
        .expect("the mutants")
        .iter()
        .filter(|mutant| mutant["status"] == status)
        .collect()
}

#[test]
fn survivors_exit_0_without_the_gate_and_1_with_the_gate_in_each_format() {
    let source = fixture(&[("src/lib.rs", KNOWN)]);
    for format in ["human", "json", "sarif"] {
        let open = mutants(source.path(), &with(&["--format", format]));
        assert_eq!(open.code, Some(0), "{format}: {}", open.stderr);
        assert_survivor_is_shown(format, &open, false);

        let gated = mutants(
            source.path(),
            &with(&["--format", format, "--fail-on-survivors"]),
        );
        assert_eq!(gated.code, Some(1), "{format}: {}", gated.stderr);
        assert_survivor_is_shown(format, &gated, true);
    }
}

fn assert_survivor_is_shown(format: &str, output: &Output, enforced: bool) {
    match format {
        "human" => {
            assert!(output.stdout.contains("src/lib.rs"), "{}", output.stdout);
            assert!(output.stdout.contains(":8:5"), "{}", output.stdout);
            let wanted = if enforced {
                "surviving-mutants: fail (1 file)\n"
            } else {
                "surviving-mutants: fail (1 file), not enforced\n"
            };
            assert!(output.stdout.contains(wanted), "{}", output.stdout);
        }
        "json" => {
            let gate = &output.json()["gate_outcomes"]["surviving-mutants"];
            assert_eq!(gate["status"], "fail", "{gate}");
            assert_eq!(gate["enforced"], enforced, "{gate}");
            assert_eq!(gate["observed"], 1.0, "{gate}");
            assert_eq!(gate["threshold"], 0.0, "{gate}");
            assert_eq!(gate["files"][0]["path"], "src/lib.rs", "{gate}");
            assert_eq!(gate["files"][0]["error_count"], 1, "{gate}");
        }
        _ => {
            let log = output.json();
            assert_eq!(log["version"], "2.1.0");
            let run = &log["runs"][0];
            assert_eq!(run["invocations"][0]["executionSuccessful"], true);
            let results = run["results"].as_array().expect("the results");
            assert_eq!(results.len(), 1, "{log}");
            let result = &results[0];
            assert_eq!(result["ruleId"], "surviving-mutants");
            assert_eq!(result["level"], if enforced { "error" } else { "warning" });
            let location = &result["locations"][0]["physicalLocation"];
            assert_eq!(location["artifactLocation"]["uri"], "src/lib.rs");
            assert_eq!(location["region"]["startLine"], 8);
            assert_eq!(run["tool"]["driver"]["rules"][0]["id"], "surviving-mutants");
            let fingerprint = result["partialFingerprints"]["mutantId"].as_str();
            assert_eq!(fingerprint.map(str::len), Some(16), "{result}");
        }
    }
}

#[test]
fn a_killed_mutant_passes_the_gate_in_each_format() {
    let source = fixture(&[("src/lib.rs", ONLY_KILLED)]);
    for format in ["human", "json", "sarif"] {
        let run = mutants(
            source.path(),
            &with(&["--format", format, "--fail-on-survivors"]),
        );
        assert_eq!(run.code, Some(0), "{format}: {}", run.stderr);
        match format {
            "human" => assert!(
                run.stdout.contains("surviving-mutants: pass"),
                "{}",
                run.stdout
            ),
            "json" => {
                let report = run.json();
                let gate = &report["gate_outcomes"]["surviving-mutants"];
                assert_eq!(gate["status"], "pass", "{report}");
                assert_eq!(report["summary"]["killed"], 1, "{report}");
                assert_eq!(report["total_issues"], 0, "{report}");
            }
            _ => {
                let log = run.json();
                let results = log["runs"][0]["results"].as_array().map(Vec::len);
                assert_eq!(results, Some(0), "{log}");
            }
        }
    }
}

#[test]
fn invalid_input_exits_2_with_the_shared_error_in_each_format() {
    let source = fixture(&[("src/lib.rs", KNOWN)]);
    for flags in [
        ["--package", "nope", "--format"],
        ["--inspect", "0000000000000001", "--format"],
        ["--inspect", "not-an-id", "--format"],
        ["--in-diff", "no-such-ref", "--format"],
    ] {
        for format in ["human", "sarif", "json"] {
            let mut all = flags.to_vec();
            all.push(format);
            let run = mutants(source.path(), &all);
            assert_eq!(run.code, Some(2), "{all:?}: {}", run.stderr);
            if format == "json" {
                let error = run.json();
                assert_eq!(error["error"], true, "{error}");
                assert_eq!(error["exit_code"], 2, "{error}");
                assert_eq!(error["code"], "CARGO_FALLOW_INVALID_ARGUMENT", "{error}");
                assert!(error["message"].is_string(), "{error}");
            } else {
                assert_eq!(run.stdout, "", "{all:?}");
                assert!(run.stderr.starts_with("error: "), "{}", run.stderr);
            }
        }
    }
}

#[test]
fn a_build_failure_exits_2_with_the_execution_code() {
    let source = fixture(&[("src/lib.rs", "pub fn broken( {")]);
    let run = mutants(source.path(), &with(&["--format", "json"]));
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert_eq!(run.json()["code"], "CARGO_FALLOW_MUTATION_FAILED");
}

#[test]
fn inspect_conflicts_with_a_flag_of_a_run() {
    let source = fixture(&[("src/lib.rs", KNOWN)]);
    let run = mutants(
        source.path(),
        &[
            "--inspect",
            "0000000000000001",
            "--in-diff",
            "HEAD",
            "--format",
            "json",
        ],
    );
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert_eq!(run.json()["code"], "CARGO_FALLOW_INVALID_ARGUMENT");
}

#[test]
fn json_has_the_report_contract_and_one_object_for_each_mutant() {
    let source = fixture(&[("src/lib.rs", KNOWN)]);
    let report = mutants(source.path(), &with(&["--format", "json"])).json();
    assert_eq!(report["schema_version"], 1, "{report}");
    assert_eq!(report["version"], env!("CARGO_PKG_VERSION"), "{report}");
    assert_eq!(report["kind"], "mutants", "{report}");
    assert_eq!(report["language"], "rust", "{report}");
    assert_eq!(report["package"], "fixture", "{report}");
    assert_eq!(report["package_version"], "0.1.0", "{report}");
    assert_eq!(report["total_issues"], 1, "{report}");

    let killed = by_status(&report, "killed");
    assert_eq!(killed.len(), 1, "{report}");
    let killed = killed[0];
    assert_eq!(killed["file"], "src/lib.rs");
    assert_eq!(killed["line"], 3);
    assert_eq!(killed["column"], 5);
    assert_eq!(killed["operator"], "relational-operator");
    assert_eq!(killed["replacement"], "!=");
    assert_eq!(killed["tier"], "schema");
    assert_eq!(killed["tests_run"], 1);
    assert!(killed["duration_ms"].is_u64(), "{killed}");
    let id = killed["id"].as_str().expect("the id");
    assert_eq!(id.len(), 16, "{id}");
    assert!(id.bytes().all(|byte| byte.is_ascii_hexdigit()), "{id}");
    assert_eq!(killed["actions"], Value::Array(Vec::new()), "{killed}");

    let survived = by_status(&report, "survived");
    assert_eq!(survived.len(), 1, "{report}");
    let survived = survived[0];
    assert_eq!(survived["line"], 8);
    let action = &survived["actions"][0];
    assert_eq!(action["type"], "inspect-mutant", "{action}");
    assert_eq!(action["auto_fixable"], false, "{action}");
    assert!(action["description"].is_string(), "{action}");
    let command = action["command"].as_str().expect("the command");
    let id = survived["id"].as_str().expect("the id");
    let start = "cargo fallow mutants --package fixture --manifest-path ";
    assert!(command.starts_with(start), "{command}");
    assert!(command.ends_with(&format!(" --inspect {id}")), "{command}");

    let uncovered = by_status(&report, "no_coverage");
    assert_eq!(uncovered.len(), 1, "{report}");
    assert_eq!(uncovered[0]["line"], 13);
    assert_eq!(uncovered[0]["actions"][0]["type"], "inspect-mutant");
}

#[test]
fn two_runs_give_the_same_mutants_in_the_same_order() {
    let source = fixture(&[("src/lib.rs", "pub mod other;\n"), ("src/other.rs", KNOWN)]);
    let order = |report: &Value| -> Vec<(String, String)> {
        report["mutants"]
            .as_array()
            .expect("the mutants")
            .iter()
            .map(|mutant| (mutant["id"].to_string(), mutant["status"].to_string()))
            .collect()
    };
    let flags = ["--format", "json", "--no-retry"];
    let first = mutants(source.path(), &flags).json();
    let second = mutants(source.path(), &flags).json();
    assert!(!order(&first).is_empty(), "{first}");
    assert_eq!(order(&first), order(&second));
}

#[test]
fn human_output_has_the_summary_first_and_groups_survivors_by_file() {
    let source = fixture(&[
        ("src/lib.rs", "pub mod a;\npub mod b;\n"),
        ("src/a.rs", KNOWN),
        ("src/b.rs", KNOWN),
    ]);
    let run = mutants(source.path(), &SCHEMA);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let text = &run.stdout;
    assert!(!text.contains('\u{1b}'), "no color in a pipe: {text}");
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some("cargo fallow mutants: fixture 0.1.0"),
        "{text}"
    );
    assert_eq!(
        lines.next(),
        Some("6 mutants, score 50.0%: 2 killed, 2 survived, 2 no coverage"),
        "{text}"
    );
    let header = text.find("\nSurvived (2)\n").expect("the survivor header");
    let first = text.find("\n  src/a.rs\n    :8:5 relational-operator `a == 2` -> `!=` [");
    let second = text.find("\n  src/b.rs\n    :8:5 relational-operator `a == 2` -> `!=` [");
    let (first, second) = (first.expect("file a"), second.expect("file b"));
    assert!(header < first && first < second, "{text}");
    assert!(
        text.contains("Inspect a mutant: cargo fallow mutants --inspect <id>"),
        "{text}"
    );
}

#[test]
fn human_output_sorts_a_file_by_position_over_both_tiers() {
    let source = fixture(&[("src/lib.rs", KNOWN)]);
    let run = mutants(source.path(), &[]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let section = run
        .stdout
        .split("\nSurvived (")
        .nth(1)
        .and_then(|rest| rest.split("\n\n").next())
        .expect("the survivor section");
    let positions: Vec<&str> = section
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix(':'))
        .filter_map(|line| line.split(' ').next())
        .collect();
    // The rebuild mutants of the body start on line 7, before the schema mutant.
    assert_eq!(positions.last(), Some(&"8:5"), "{section}");
    assert!(positions.len() > 1, "{section}");
    assert!(positions[0].starts_with("7:"), "{section}");
}

#[test]
fn a_run_with_pending_mutants_warns_and_does_not_pass_the_gate() {
    let source = fixture(&[("src/lib.rs", ONLY_KILLED)]);
    let flags = with(&[
        "--format",
        "json",
        "--fail-on-survivors",
        "--max-mutants",
        "0",
    ]);
    let run = mutants(source.path(), &flags);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let report = run.json();
    assert_eq!(report["summary"]["pending"], 1, "{report}");
    let gate = &report["gate_outcomes"]["surviving-mutants"];
    assert_eq!(gate["status"], "warn", "{report}");
}

#[test]
fn no_run_skips_the_gate() {
    let source = fixture(&[("src/lib.rs", KNOWN)]);
    let flags = with(&["--format", "json", "--fail-on-survivors", "--no-run"]);
    let run = mutants(source.path(), &flags);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let report = run.json();
    let gate = &report["gate_outcomes"]["surviving-mutants"];
    assert_eq!(gate["status"], "skipped", "{report}");
}

/// The id of the mutant on `line`.
fn id_on_line(source: &Path, line: u64) -> String {
    let report = mutants(source, &with(&["--format", "json", "--no-run"])).json();
    report["mutants"]
        .as_array()
        .expect("the mutants")
        .iter()
        .find(|mutant| mutant["line"] == line)
        .and_then(|mutant| mutant["id"].as_str())
        .expect("a mutant on the line")
        .to_owned()
}

#[test]
fn inspect_shows_the_mutation_and_the_tests_that_cover_it() {
    let source = fixture(&[("src/lib.rs", KNOWN)]);
    let before = fs::read_to_string(source.path().join("src/lib.rs")).expect("read the source");
    let id = id_on_line(source.path(), 8);

    // No run came first, so the command builds the map and keeps it.
    let report = mutants(source.path(), &["--inspect", &id, "--format", "json"]).json();
    assert_eq!(report["schema_version"], 1, "{report}");
    assert_eq!(report["kind"], "mutants-inspect", "{report}");
    assert_eq!(report["language"], "rust", "{report}");
    assert_eq!(report["package"], "fixture", "{report}");
    let mutant = &report["mutant"];
    assert_eq!(mutant["id"], id.as_str(), "{report}");
    assert_eq!(mutant["file"], "src/lib.rs");
    assert_eq!(mutant["line"], 8);
    assert_eq!(mutant["column"], 5);
    assert_eq!(mutant["tier"], "schema");
    assert_eq!(mutant["operator"], "relational-operator");
    assert_eq!(mutant["replacement"], "!=");
    assert_eq!(mutant["before"], "    a == 2", "{report}");
    assert_eq!(mutant["after"], "    a != 2", "{report}");
    let coverage = &report["coverage"];
    assert_eq!(coverage["map"], "built", "{report}");
    assert_eq!(coverage["state"], "covered", "{report}");
    let tests = coverage["tests"].as_array().expect("the tests");
    assert_eq!(tests.len(), 1, "{report}");
    let name = tests[0].as_str().expect("a name");
    assert!(name.ends_with("tests::check_works"), "{report}");

    // The second call reads the map. The human form shows the same facts.
    let again = mutants(source.path(), &["--inspect", &id, "--format", "json"]).json();
    assert_eq!(again["coverage"]["map"], "cached", "{again}");
    let human = mutants(source.path(), &["--inspect", &id]);
    let text = &human.stdout;
    assert_eq!(human.code, Some(0), "{}", human.stderr);
    assert!(text.contains(&format!("mutant {id}")), "{text}");
    assert!(text.contains("src/lib.rs:8:5"), "{text}");
    assert!(text.contains("-     a == 2\n"), "{text}");
    assert!(text.contains("+     a != 2\n"), "{text}");
    assert!(text.contains("tests::check_works"), "{text}");

    let after = fs::read_to_string(source.path().join("src/lib.rs")).expect("read the source");
    assert_eq!(before, after);
}

#[test]
fn inspect_reads_the_map_of_an_earlier_run() {
    let source = fixture(&[("src/lib.rs", KNOWN)]);
    let report = mutants(source.path(), &with(&["--format", "json"])).json();
    let action = &by_status(&report, "survived")[0]["actions"][0];
    let id = action["command"]
        .as_str()
        .and_then(|command| command.rsplit(' ').next())
        .expect("the id of the command");
    let inspected = mutants(source.path(), &["--inspect", id, "--format", "json"]).json();
    assert_eq!(inspected["coverage"]["map"], "cached", "{inspected}");
    assert_eq!(inspected["mutant"]["line"], 8, "{inspected}");
}

#[test]
fn inspect_says_when_no_test_executes_the_mutant() {
    let source = fixture(&[("src/lib.rs", KNOWN)]);
    let id = id_on_line(source.path(), 13);
    let report = mutants(source.path(), &["--inspect", &id, "--format", "json"]).json();
    assert_eq!(report["coverage"]["state"], "not_covered", "{report}");
    let tests = &report["coverage"]["tests"];
    assert_eq!(tests, &Value::Array(Vec::new()), "{report}");
    let human = mutants(source.path(), &["--inspect", &id]);
    assert!(
        human.stdout.contains("No test executes"),
        "{}",
        human.stdout
    );
}

#[test]
fn inspect_finds_a_mutant_in_any_member_without_a_package_flag() {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    fs::write(
        dir.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"a\", \"b\"]\nresolver = \"2\"\n",
    )
    .expect("write the manifest");
    for (name, source) in [("a", ONLY_KILLED), ("b", KNOWN)] {
        let manifest =
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
        write_files(
            dir.path(),
            &[
                (&format!("{name}/Cargo.toml"), &manifest),
                (&format!("{name}/src/lib.rs"), source),
            ],
        );
    }
    let flags = with(&["--package", "b", "--format", "json", "--no-run"]);
    let listed = mutants(dir.path(), &flags).json();
    let id = listed["mutants"][0]["id"].as_str().expect("an id");
    let report = mutants(dir.path(), &["--inspect", id, "--format", "json"]).json();
    assert_eq!(report["package"], "b", "{report}");
    assert_eq!(report["mutant"]["file"], "b/src/lib.rs", "{report}");
}

#[test]
fn sarif_is_not_a_format_of_inspect() {
    let source = fixture(&[("src/lib.rs", KNOWN)]);
    let id = id_on_line(source.path(), 3);
    let run = mutants(source.path(), &["--inspect", &id, "--format", "sarif"]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(run.stderr.contains("SARIF"), "{}", run.stderr);
}

/// Runs git in `dir` with no state from the environment of the test.
fn git(dir: &Path, args: &[&str]) {
    let status = Process::new("git")
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
        ])
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?}");
}

fn repository(root: &Path) {
    fs::write(root.join(".gitignore"), "target\nCargo.lock\n").expect("write the ignore file");
    git(root, &["init", "-q"]);
    git(root, &["checkout", "-q", "-b", "main"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "first"]);
}

fn lines(report: &Value) -> Vec<u64> {
    report["mutants"]
        .as_array()
        .expect("the mutants")
        .iter()
        .filter_map(|mutant| mutant["line"].as_u64())
        .collect()
}

#[test]
fn in_diff_keeps_only_the_mutants_on_changed_lines() {
    let source = fixture(&[("src/lib.rs", KNOWN)]);
    repository(source.path());
    let edited = KNOWN.replace("a == 2", "a == 9");
    fs::write(source.path().join("src/lib.rs"), edited).expect("edit the source");
    let flags = with(&[
        "--format",
        "json",
        "--in-diff",
        "HEAD",
        "--fail-on-survivors",
    ]);
    let run = mutants(source.path(), &flags);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    let report = run.json();
    assert_eq!(lines(&report), [8], "{report}");
    assert_eq!(report["mutants"][0]["status"], "survived", "{report}");
    let scope = &report["in_diff"];
    assert_eq!(scope["ref"], "HEAD", "{report}");
    assert_eq!(scope["changed_files"], 1, "{report}");
    assert_eq!(scope["mutants_outside"], 2, "{report}");
    assert_eq!(scope["base"].as_str().map(str::len), Some(40), "{report}");
}

#[test]
fn in_diff_also_limits_the_rebuild_mutants() {
    let source = fixture(&[("src/lib.rs", KNOWN)]);
    repository(source.path());
    let edited = KNOWN.replace("a == 2", "a == 9");
    fs::write(source.path().join("src/lib.rs"), edited).expect("edit the source");
    let flags = ["--format", "json", "--in-diff", "HEAD", "--no-run"];
    let report = mutants(source.path(), &flags).json();
    let all = mutants(source.path(), &["--format", "json", "--no-run"]).json();
    let total = all["mutants"].as_array().map_or(0, Vec::len);
    let kept = report["mutants"].as_array().map_or(0, Vec::len);
    assert!(kept > 1, "{report}");
    for mutant in report["mutants"].as_array().expect("the mutants") {
        let line = mutant["line"].as_u64().expect("a line");
        assert!((7..=9).contains(&line), "{mutant}");
    }
    let rebuild = report["mutants"]
        .as_array()
        .expect("the mutants")
        .iter()
        .filter(|mutant| mutant["tier"] == "rebuild")
        .count();
    assert!(rebuild > 0, "{report}");
    assert_eq!(
        report["in_diff"]["mutants_outside"],
        total - kept,
        "{report}"
    );
}

#[test]
fn in_diff_with_no_changed_mutant_reports_nothing_and_builds_nothing() {
    let source = fixture(&[("src/lib.rs", KNOWN)]);
    repository(source.path());
    let flags = with(&[
        "--format",
        "json",
        "--in-diff",
        "HEAD",
        "--fail-on-survivors",
    ]);
    let run = mutants(source.path(), &flags);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let report = run.json();
    assert_eq!(lines(&report), Vec::<u64>::new(), "{report}");
    assert_eq!(report["in_diff"]["mutants_outside"], 3, "{report}");
    assert_eq!(report["baseline"]["clean"], Value::Null, "{report}");
    let gate = &report["gate_outcomes"]["surviving-mutants"];
    assert_eq!(gate["status"], "pass", "{report}");
    let human = mutants(source.path(), &with(&["--in-diff", "HEAD"]));
    let text = &human.stdout;
    assert!(text.contains("no mutant on a changed line"), "{text}");
}

#[test]
fn in_diff_uses_the_fork_point_and_sees_new_files() {
    let source = fixture(&[("src/lib.rs", KNOWN)]);
    repository(source.path());
    git(source.path(), &["checkout", "-q", "-b", "feature"]);
    git(source.path(), &["checkout", "-q", "main"]);
    // A change on main after the fork is not a change of the branch.
    let on_main = KNOWN.replace("a == 3", "a == 7");
    fs::write(source.path().join("src/lib.rs"), on_main).expect("edit the source");
    git(source.path(), &["commit", "-q", "-a", "-m", "on main"]);
    git(source.path(), &["checkout", "-q", "feature"]);
    let on_branch = format!("pub mod fresh;\n{}", KNOWN.replace("a == 1", "a == 6"));
    fs::write(source.path().join("src/lib.rs"), on_branch).expect("edit the source");
    // No `git add`: an untracked file counts as changed.
    let fresh = "pub fn fresh(a: u8) -> bool {\n    a == 5\n}\n";
    write_files(source.path(), &[("src/fresh.rs", fresh)]);
    let flags = with(&["--format", "json", "--in-diff", "main", "--no-run"]);
    let report = mutants(source.path(), &flags).json();
    let found: Vec<(&str, u64)> = report["mutants"]
        .as_array()
        .expect("the mutants")
        .iter()
        .map(|mutant| {
            (
                mutant["file"].as_str().expect("a file"),
                mutant["line"].as_u64().expect("a line"),
            )
        })
        .collect();
    assert_eq!(found, [("src/fresh.rs", 2), ("src/lib.rs", 4)], "{report}");
    assert_eq!(report["in_diff"]["changed_files"], 2, "{report}");
}

#[test]
fn in_diff_paths_are_relative_to_the_workspace_in_a_larger_repository() {
    let outer = tempfile::tempdir().expect("create a temporary directory");
    let inner = outer.path().join("vendor/inner");
    fs::create_dir_all(inner.join("src")).expect("create the folders");
    fs::write(inner.join("Cargo.toml"), MANIFEST).expect("write the manifest");
    fs::write(inner.join("src/lib.rs"), KNOWN).expect("write the source");
    repository(outer.path());
    let edited = KNOWN.replace("a == 2", "a == 9");
    fs::write(inner.join("src/lib.rs"), edited).expect("edit the source");
    let flags = with(&["--format", "json", "--in-diff", "HEAD", "--no-run"]);
    let report = mutants(&inner, &flags).json();
    assert_eq!(lines(&report), [8], "{report}");
    assert_eq!(report["mutants"][0]["file"], "src/lib.rs", "{report}");
}

#[test]
fn in_diff_outside_a_repository_exits_2() {
    let source = fixture(&[("src/lib.rs", KNOWN)]);
    let run = mutants(source.path(), &["--in-diff", "main", "--format", "json"]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert_eq!(run.json()["code"], "CARGO_FALLOW_INVALID_ARGUMENT");
}

#[test]
fn a_ref_that_looks_like_a_flag_is_not_an_option_of_git() {
    let source = fixture(&[("src/lib.rs", KNOWN)]);
    repository(source.path());
    let run = mutants(
        source.path(),
        &["--in-diff=--output=written", "--format", "json"],
    );
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert!(!source.path().join("written").exists());
}
