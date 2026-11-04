//! Black-box tests of the structure checks: `boundaries`, `size`,
//! `complexity` and `dupes`.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::collections::BTreeSet;
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
        self.run_with_env(args, &[])
    }

    /// Runs one subcommand with extra environment variables.
    fn run_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let output = Command::cargo_bin("cargo-fallow")
            .expect("find the binary")
            .current_dir(self.root())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .envs(env.iter().copied())
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
        git_in(self.root(), args);
    }
}

/// Runs git in `dir` with a fixed test identity.
fn git_in(dir: &Path, args: &[&str]) {
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
    assert!(status.status.success(), "git {args:?} failed: {status:?}");
}

impl Fixture {
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
fn complexity_skips_a_file_whose_operator_chain_gives_a_deep_tree() {
    let fixture = Fixture::new();
    let chain = format!("pub fn deep() -> u32 {{ 1{} }}\n", " + 1".repeat(20_000));
    fixture.write("crates/process/src/lib.rs", &chain);
    for _ in 0..2 {
        let (code, report) = fixture.json(&["complexity"]);
        assert_eq!(code, 0, "{report}");
        assert_eq!(
            report["summary"]["skipped_files"],
            serde_json::json!([{
                "path": "crates/process/src/lib.rs",
                "reason": "the file nests too deep for the parser",
            }]),
            "{report:#}"
        );
    }
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
fn dupes_skip_a_large_data_file_without_a_generator_header_in_time() {
    let fixture = Fixture::new();
    let lib = fs::read_to_string(fixture.root().join("crates/process/src/lib.rs"))
        .expect("read the process lib");
    fixture.write(
        "crates/process/src/lib.rs",
        &format!("{lib}\npub mod table;\n"),
    );
    let table = format!(
        "pub static T: &[u32] = &[\n{}];\n",
        "1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,\n".repeat(8200)
    );
    fixture.write("crates/process/src/table.rs", &table);
    let started = std::time::Instant::now();
    let (code, report) = fixture.json(&["dupes", "--min-tokens", "30", "--min-lines", "5"]);
    let elapsed = started.elapsed();
    assert_eq!(code, 0, "{report}");
    assert!(elapsed.as_secs() < 60, "the run took {elapsed:?}");
    let skipped = &report["summary"]["skipped_files"];
    assert_eq!(
        skipped[0]["path"], "crates/process/src/table.rs",
        "{skipped}"
    );
    // A release build skips the file for its token count. A debug build can
    // need more parse stack than the system gives for this file size, and
    // then skips the file as too deep. A unit test covers the token budget.
    assert!(
        skipped[0]["reason"].as_str().is_some_and(|reason| {
            reason.contains("tokens") || reason.contains("nests too deep")
        }),
        "{skipped}"
    );
    assert!(
        !report["findings"].as_array().expect("findings").is_empty(),
        "the copied function is still a clone"
    );
}

#[test]
fn a_config_threshold_reports_the_gate_but_fails_only_with_a_gate_flag() {
    let fixture = Fixture::new();
    let manifest = fs::read_to_string(fixture.manifest()).expect("read the manifest");
    fixture.write(
        "Cargo.toml",
        &format!(
            "{manifest}\n[workspace.metadata.fallow.dupes]\nmin-tokens = 30\nthreshold = 0.1\n"
        ),
    );
    let (code, report) = fixture.json(&["dupes"]);
    let gate = &report["gate_outcomes"]["duplication-threshold"];
    assert_eq!(code, 0, "{report}");
    assert_eq!(gate["status"], "fail", "{gate}");
    assert_eq!(gate["enforced"], false, "{gate}");
    let (code, report) = fixture.json(&["dupes", "--fail-on-clones"]);
    assert_eq!(code, 1, "{report}");
    assert_eq!(
        report["gate_outcomes"]["duplication-threshold"]["enforced"], false,
        "--fail-on-clones enforces only the clone gate"
    );
    let (code, report) = fixture.json(&["dupes", "--threshold", "0.1"]);
    assert_eq!(code, 1, "the flag is an explicit gate");
    assert_eq!(
        report["gate_outcomes"]["duplication-findings"]["enforced"], false,
        "the exit comes from the threshold gate"
    );

    let (code, report) = fixture.json(&["check", "--only", "dupes"]);
    let gate = &report["gate_outcomes"]["duplication-threshold"];
    assert_eq!(code, 0, "{report}");
    assert_eq!(gate["status"], "fail", "{gate}");
    assert_eq!(gate["enforced"], false, "{gate}");
    let (code, report) = fixture.json(&["check", "--only", "dupes", "--fail-on-issues"]);
    assert_eq!(code, 1);
    assert_eq!(
        report["gate_outcomes"]["duplication-threshold"]["enforced"], true,
        "{report}"
    );
}

#[test]
fn dupes_read_their_settings_from_the_table_with_the_command_name() {
    let fixture = Fixture::new();
    let manifest = fs::read_to_string(fixture.manifest()).expect("read the manifest");
    fixture.write(
        "Cargo.toml",
        &format!("{manifest}\n[workspace.metadata.fallow.dupes]\nmin-tokens = 30\nmin-lines = 5\n"),
    );
    let (code, report) = fixture.json(&["dupes"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["summary"]["min_tokens"], 30, "{report:#}");
    fixture.write(
        "Cargo.toml",
        &format!("{manifest}\n[workspace.metadata.fallow.dupes]\nmin-token = 30\n"),
    );
    let (code, report) = fixture.json(&["dupes"]);
    assert_eq!(code, 2, "{report}");
    assert!(
        report["message"]
            .as_str()
            .is_some_and(|message| message.contains("[workspace.metadata.fallow.dupes]")),
        "{report}"
    );
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
fn size_growth_skips_a_new_file_with_an_at_generated_comment() {
    let fixture = committed();
    let body = "// x\n".repeat(60);
    fixture.write(
        "crates/app/src/tables.rs",
        &format!("// This file is @generated by a build tool\n{body}"),
    );
    fixture.write("crates/app/src/big.rs", &body);
    fixture.git(&["add", "--all"]);
    let (code, report) = fixture.json(&["size", "--staged", "--fail-on-growth"]);
    assert_eq!(code, 1, "{report:#}");
    let messages = strings(&report, "message");
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0].starts_with("crates/app/src/big.rs: "),
        "{messages:?}"
    );
}

/// A fixture with one commit on `main`.
fn committed() -> Fixture {
    let fixture = Fixture::new();
    fixture.git(&["init", "--quiet"]);
    fixture.git(&["add", "--all"]);
    fixture.git(&["commit", "--quiet", "-m", "base"]);
    fixture
}

#[test]
fn size_ignores_git_variables_that_point_to_another_repository() {
    let fixture = committed();
    fixture.write("crates/app/src/big.rs", &"// x\n".repeat(41));
    fixture.git(&["add", "--all"]);
    let run = fixture.run_with_env(
        &["size", "--staged", "--fail-on-growth", "--format", "json"],
        &[
            ("GIT_DIR", "/nonexistent/.git"),
            ("GIT_WORK_TREE", "/nonexistent"),
            ("GIT_INDEX_FILE", "/nonexistent/index"),
            ("GIT_OBJECT_DIRECTORY", "/nonexistent/objects"),
            ("GIT_CEILING_DIRECTORIES", "/"),
        ],
    );
    assert_eq!(run.code, 1, "{}", run.stdout);
    let report: Value = serde_json::from_str(&run.stdout).expect("the output is JSON");
    assert_eq!(strings(&report, "kind"), ["new-large-file"], "{report:#}");
}

#[test]
fn size_refuses_a_base_that_git_would_read_as_an_option() {
    let fixture = committed();
    for base in ["--base=--all", "--base=-p"] {
        let (code, report) = fixture.json(&["size", base]);
        assert_eq!(code, 2, "{report}");
        assert!(
            report["message"]
                .as_str()
                .is_some_and(|message| message.contains("must name a git ref")),
            "{report}"
        );
    }
    let (code, report) = fixture.json(&["size", "--base", "main"]);
    assert_eq!(code, 0, "{report}");
}

#[test]
fn size_fails_when_git_cannot_read_a_changed_blob() {
    let fixture = committed();
    fixture.git(&[
        "update-index",
        "--add",
        "--info-only",
        "--cacheinfo",
        "100644,1234567890123456789012345678901234567890,crates/app/src/ghost.rs",
    ]);
    let (code, report) = fixture.json(&["size", "--staged", "--fail-on-growth"]);
    assert_eq!(code, 2, "{report}");
    assert!(
        report["message"]
            .as_str()
            .is_some_and(|message| message.contains("cannot read")),
        "{report}"
    );
}

#[test]
fn size_on_a_shallow_clone_names_the_fetch_in_the_help() {
    let origin = committed();
    origin.git(&["checkout", "--quiet", "-b", "topic"]);
    origin.write("crates/app/src/more.rs", "// more\n");
    origin.git(&["add", "--all"]);
    origin.git(&["commit", "--quiet", "-m", "topic"]);
    origin.git(&["checkout", "--quiet", "main"]);
    origin.write("crates/app/src/other.rs", "// other\n");
    origin.git(&["add", "--all"]);
    origin.git(&["commit", "--quiet", "-m", "main"]);
    let clone = tempfile::tempdir().expect("create a temporary directory");
    let url = format!("file://{}", origin.root().display()).replace('\\', "/");
    git_in(
        clone.path(),
        &[
            "clone",
            "--quiet",
            "--depth",
            "1",
            "--no-single-branch",
            &url,
            "work",
        ],
    );
    let work = Fixture {
        dir: tempfile::tempdir().expect("create a temporary directory"),
    };
    copy_dir(&clone.path().join("work"), work.root());
    let (code, report) = work.json(&["size", "--base", "origin/topic"]);
    assert_eq!(code, 2, "{report}");
    assert!(
        report["help"]
            .as_str()
            .is_some_and(|help| help.contains("git fetch --unshallow")),
        "{report}"
    );
}

#[cfg(unix)]
#[test]
fn size_skips_a_staged_path_that_is_not_utf8_and_checks_the_rest() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let fixture = committed();
    fixture.write("crates/app/src/big.rs", &"// x\n".repeat(41));
    fixture.git(&["add", "--all"]);
    let blob = StdCommand::new("git")
        .current_dir(fixture.root())
        .args(["hash-object", "-w", "crates/app/src/big.rs"])
        .output()
        .expect("run git");
    let blob = String::from_utf8_lossy(&blob.stdout).trim().to_owned();
    let mut info = format!("100644,{blob},crates/app/src/").into_bytes();
    info.extend_from_slice(b"bad\xff.rs");
    let status = StdCommand::new("git")
        .current_dir(fixture.root())
        .args(["update-index", "--add", "--cacheinfo"])
        .arg(OsStr::from_bytes(&info))
        .status()
        .expect("run git");
    assert!(status.success());
    let (code, report) = fixture.json(&["size", "--staged", "--fail-on-growth"]);
    assert_eq!(code, 1, "{report}");
    assert_eq!(
        strings(&report, "path"),
        ["crates/app/src/big.rs"],
        "{report:#}"
    );
    assert_eq!(
        report["summary"]["skipped_files"][0]["path"], "crates/app/src/bad\u{fffd}.rs",
        "{report:#}"
    );
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

#[test]
fn fail_on_issues_is_the_gate_flag_of_each_structure_command() {
    let fixture = Fixture::new();
    for args in [
        &["boundaries", "--fail-on-issues"][..],
        &["complexity", "--fail-on-issues"],
        &["dupes", "--min-tokens", "30", "--fail-on-issues"],
    ] {
        let (code, report) = fixture.json(args);
        assert_eq!(code, 1, "{args:?}: {report:#}");
        let enforced = report["gate_outcomes"]
            .as_object()
            .expect("gate outcomes")
            .values()
            .any(|gate| gate["enforced"] == true);
        assert!(enforced, "{args:?}: {report:#}");
    }
    // The old flag and the new flag together are one gate, not a usage error.
    for args in [
        &["boundaries", "--fail-on-violations", "--fail-on-issues"][..],
        &["complexity", "--fail-on-complexity", "--fail-on-issues"],
        &[
            "dupes",
            "--min-tokens",
            "30",
            "--fail-on-clones",
            "--fail-on-issues",
        ],
    ] {
        let (code, _) = fixture.json(args);
        assert_eq!(code, 1, "{args:?}");
    }
}

#[test]
fn size_fail_on_issues_gates_growth_and_needs_no_base() {
    let fixture = committed();
    let (code, report) = fixture.json(&["size", "--fail-on-issues"]);
    assert_eq!(code, 0, "{report:#}");
    let gate = &report["gate_outcomes"]["file-growth"];
    assert_eq!(gate["status"], "skipped", "{report:#}");

    fixture.git(&["checkout", "--quiet", "-b", "topic"]);
    let core = fs::read_to_string(fixture.root().join("crates/core/src/lib.rs")).expect("read");
    fixture.write(
        "crates/core/src/lib.rs",
        &format!("{core}\npub fn more() {{}}\n"),
    );
    fixture.git(&["commit", "--quiet", "--all", "-m", "grow"]);
    let (code, report) = fixture.json(&["size", "--base", "main", "--fail-on-issues"]);
    assert_eq!(code, 1, "{report:#}");
    assert_eq!(report["gate_outcomes"]["file-growth"]["enforced"], true);
    fixture.write("crates/app/src/big.rs", &"// x\n".repeat(41));
    fixture.git(&["add", "--all"]);
    let (code, report) = fixture.json(&["size", "--staged", "--fail-on-issues"]);
    assert_eq!(code, 1, "{report:#}");
    assert_eq!(strings(&report, "kind"), ["new-large-file"]);
    let (code, _) = fixture.json(&[
        "size",
        "--base",
        "main",
        "--fail-on-growth",
        "--fail-on-issues",
    ]);
    assert_eq!(code, 1);
}

/// The ids of the findings of a report.
fn ids_of(report: &Value) -> Vec<String> {
    report["findings"]
        .as_array()
        .expect("findings is an array")
        .iter()
        .filter_map(|finding| finding["id"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn changed_since_keeps_only_the_findings_in_changed_files() {
    let fixture = committed();
    fixture.git(&["checkout", "--quiet", "-b", "topic"]);
    let render = fs::read_to_string(fixture.root().join("crates/app/src/render.rs")).expect("read");
    fixture.write(
        "crates/app/src/render.rs",
        &format!("{render}// A change.\n"),
    );
    fixture.git(&["commit", "--quiet", "--all", "-m", "change render"]);

    let (code, report) = fixture.json(&["boundaries", "--changed-since", "main"]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(
        strings(&report, "rule"),
        ["render-does-not-run", "render-does-not-run"],
        "{report:#}"
    );
    assert_eq!(report["total_issues"], 2);
    assert_eq!(report["changed_since"]["ref"], "main");
    assert_eq!(
        report["changed_since"]["changed_files"],
        serde_json::json!(["crates/app/src/render.rs"])
    );
    assert_eq!(
        report["changed_since"]["base"].as_str().map(str::len),
        Some(40)
    );

    // A complexity finding in an unchanged file is gone, so the gate passes.
    let (code, report) =
        fixture.json(&["complexity", "--changed-since", "main", "--fail-on-issues"]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["total_issues"], 0);
    // The suppressed finding is in an unchanged file, so it does not count.
    assert_eq!(report["summary"]["suppressed"], 0, "{report:#}");

    // An uncommitted edit counts, and the finding keeps the id of a full run.
    let (_, full) = fixture.json(&["complexity"]);
    let core = fs::read_to_string(fixture.root().join("crates/core/src/lib.rs")).expect("read");
    fixture.write("crates/core/src/lib.rs", &format!("{core}// A change.\n"));
    let (code, report) =
        fixture.json(&["complexity", "--changed-since", "main", "--fail-on-issues"]);
    assert_eq!(code, 1, "{report:#}");
    assert_eq!(strings(&report, "name"), ["branchy"]);
    assert_eq!(ids_of(&report), ids_of(&full));

    let human = fixture.run(&["complexity", "--changed-since", "main"]);
    assert!(
        human
            .stdout
            .starts_with("Scope: 2 files changed since `main` (fork point "),
        "{}",
        human.stdout
    );
}

#[test]
fn an_untracked_file_puts_the_manifest_findings_of_its_package_in_scope() {
    let fixture = committed();
    let (_, report) = fixture.json(&["boundaries", "--changed-since", "main"]);
    assert_eq!(report["total_issues"], 0, "{report:#}");
    fixture.write("crates/process/notes.md", "A new file.\n");
    let (_, report) = fixture.json(&["boundaries", "--changed-since", "main"]);
    assert_eq!(
        strings(&report, "rule"),
        ["process-stays-a-leaf"],
        "{report:#}"
    );
    assert_eq!(report["findings"][0]["path"], "crates/process/Cargo.toml");
}

#[test]
fn changed_since_keeps_a_clone_group_with_one_changed_copy_and_skips_the_threshold() {
    // A second clone group in two files that do not change.
    let fixture = Fixture::new();
    let copy = "\npub fn tally(values: &[u32]) -> u32 {\n    let mut total = 0;\n    for value in values {\n        if *value > 3 {\n            total += value * 2;\n        } else {\n            total += value + 7;\n        }\n    }\n    total\n}\n";
    for path in ["crates/app/src/render.rs", "crates/app/src/engine.rs"] {
        let text = fs::read_to_string(fixture.root().join(path)).expect("read");
        fixture.write(path, &format!("{text}{copy}"));
    }
    fixture.git(&["init", "--quiet"]);
    fixture.git(&["add", "--all"]);
    fixture.git(&["commit", "--quiet", "-m", "base"]);
    let process =
        fs::read_to_string(fixture.root().join("crates/process/src/lib.rs")).expect("read");
    fixture.write(
        "crates/process/src/lib.rs",
        &format!("{process}// A change.\n"),
    );
    let (_, full) = fixture.json(&["dupes", "--min-tokens", "30", "--threshold", "1"]);
    let (code, report) = fixture.json(&[
        "dupes",
        "--min-tokens",
        "30",
        "--threshold",
        "1",
        "--changed-since",
        "main",
    ]);
    assert_eq!(code, 0, "{report:#}");
    let groups = report["findings"].as_array().expect("findings");
    assert!(!groups.is_empty(), "{report:#}");
    assert!(groups.len() < full["findings"].as_array().map_or(0, Vec::len) + 1);
    for group in groups {
        let files: Vec<&str> = group["instances"]
            .as_array()
            .map(|all| all.iter().filter_map(|i| i["file"].as_str()).collect())
            .unwrap_or_default();
        assert!(files.contains(&"crates/process/src/lib.rs"), "{group:#}");
    }
    assert!(
        report["gate_outcomes"]["duplication-threshold"].is_null(),
        "{report:#}"
    );
    assert_eq!(full["summary"]["stats"]["clone_groups"], 2, "{full:#}");
    assert_eq!(groups.len(), 1, "{report:#}");
    assert_eq!(report["summary"]["stats"]["clone_groups"], groups.len());
    let families = report["summary"]["clone_families"]
        .as_array()
        .expect("families");
    assert_eq!(report["summary"]["stats"]["clone_families"], families.len());
    assert_eq!(
        full["gate_outcomes"]["duplication-threshold"]["status"],
        "fail"
    );
}

#[test]
fn check_with_changed_since_scopes_each_section_and_gives_no_score() {
    let fixture = committed();
    let (_, full) = fixture.json(&["check", "--skip", "deps"]);
    assert!(full["health"]["penalties"].is_object(), "{full:#}");
    let core = fs::read_to_string(fixture.root().join("crates/core/src/lib.rs")).expect("read");
    fixture.write("crates/core/src/lib.rs", &format!("{core}// A change.\n"));
    let (code, report) = fixture.json(&["check", "--skip", "deps", "--changed-since", "main"]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["health"]["scope"], "changed", "{report:#}");
    assert!(report["health"]["penalties"].is_null(), "{report:#}");
    assert_eq!(
        report["changed_since"]["changed_files"],
        serde_json::json!(["crates/core/src/lib.rs"])
    );
    let paths: Vec<&str> = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter_map(|finding| finding["location"]["path"].as_str())
        .collect();
    assert!(!paths.is_empty(), "{report:#}");
    // A clone group keeps each copy, so its other place can be outside the
    // scope. Each other finding is in the changed file.
    for finding in report["findings"].as_array().expect("findings") {
        let in_scope =
            finding["location"]["path"] == "crates/core/src/lib.rs" || finding["check"] == "dupes";
        assert!(in_scope, "{finding:#}");
    }
    assert_eq!(report["total_issues"], paths.len());
    assert!(
        report["sections"]["boundaries"]["findings"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    let (code, _) = fixture.json(&[
        "check",
        "--skip",
        "deps",
        "--changed-since",
        "main",
        "--fail-on-issues",
    ]);
    assert_eq!(code, 1);
}

#[test]
fn changed_since_with_a_bad_ref_or_outside_git_exits_2() {
    let outside = Fixture::new();
    let (code, report) = outside.json(&["complexity", "--changed-since", "main"]);
    assert_eq!(code, 2, "{report:#}");
    assert_eq!(report["code"], "CARGO_FALLOW_INVALID_ARGUMENT");

    let fixture = committed();
    for command in [
        "boundaries",
        "size",
        "complexity",
        "dupes",
        "check",
        "visibility",
    ] {
        let (code, report) = fixture.json(&[command, "--changed-since", "no-such-ref"]);
        assert_eq!(code, 2, "{command}: {report:#}");
        assert_eq!(report["code"], "CARGO_FALLOW_INVALID_ARGUMENT", "{command}");
        assert!(
            report["message"]
                .as_str()
                .is_some_and(|message| message.contains("no-such-ref")),
            "{command}: {report:#}"
        );
    }
    let (code, report) = fixture.json(&["complexity", "--changed-since=--output=x"]);
    assert_eq!(code, 2, "{report:#}");
    assert!(
        report["message"]
            .as_str()
            .is_some_and(|message| message.contains("must name a git ref")),
        "{report:#}"
    );
}

#[test]
fn deps_and_visibility_with_changed_since_keep_the_findings_of_the_changed_package() {
    let fixture = committed();
    let process =
        fs::read_to_string(fixture.root().join("crates/process/src/lib.rs")).expect("read");
    fixture.write(
        "crates/process/src/lib.rs",
        &format!("{process}// A change.\n"),
    );

    let (code, report) = fixture.json(&["visibility", "--changed-since", "main"]);
    assert_eq!(code, 0, "{report:#}");
    let files: Vec<&str> = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter_map(|finding| finding["file"].as_str())
        .collect();
    assert_eq!(files, ["crates/process/src/lib.rs"], "{report:#}");
    assert_eq!(report["changed_since"]["ref"], "main");

    // The source file of the package puts the findings of its manifest in scope.
    let (code, report) = fixture.json(&["deps", "--changed-since", "main", "--fail-on-issues"]);
    assert_eq!(code, 1, "{report:#}");
    let manifests: Vec<&str> = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter_map(|finding| finding["manifest_path"].as_str())
        .collect();
    assert_eq!(manifests, ["crates/process/Cargo.toml"], "{report:#}");
    assert_eq!(report["total_issues"], 1);
}

#[test]
fn a_saved_baseline_hides_its_findings_and_a_new_finding_stays() {
    let fixture = Fixture::new();
    let path = fixture.root().join("baseline.json");
    let path = path.to_str().expect("UTF-8 path");
    let (code, saved) = fixture.json(&[
        "check",
        "--skip",
        "deps",
        "--fail-on-issues",
        "--save-baseline",
        path,
    ]);
    assert_eq!(code, 0, "a run that saves a baseline exits 0: {saved:#}");
    let file: Value =
        serde_json::from_str(&fs::read_to_string(path).expect("read the baseline")).expect("JSON");
    assert_eq!(file["kind"], "check");
    assert_eq!(file["schema_version"], 1);
    let entries = file["findings"].as_array().expect("entries");
    assert!(!entries.is_empty(), "{file:#}");
    assert!(
        entries.iter().any(|entry| entry["check"] == "dupes"),
        "{file:#}"
    );

    let (code, report) = fixture.json(&[
        "check",
        "--skip",
        "deps",
        "--fail-on-issues",
        "--baseline",
        path,
    ]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["total_issues"], 0, "{report:#}");
    assert_eq!(report["baseline"]["matched"], entries.len());
    assert_eq!(report["baseline"]["stale"], serde_json::json!([]));
    assert_eq!(report["gate_outcomes"]["stale-baseline"]["status"], "pass");

    // A blank line above each finding keeps each id.
    for file in [
        "crates/core/src/lib.rs",
        "crates/app/src/lib.rs",
        "crates/app/src/render.rs",
        "crates/process/src/lib.rs",
        "crates/process/Cargo.toml",
    ] {
        let text = fs::read_to_string(fixture.root().join(file)).expect("read");
        fixture.write(file, &format!("\n{text}"));
    }
    let (code, report) = fixture.json(&[
        "check",
        "--skip",
        "deps",
        "--fail-on-issues",
        "--baseline",
        path,
    ]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["total_issues"], 0, "{report:#}");

    // A new finding stays, and the run fails on it.
    let core = fs::read_to_string(fixture.root().join("crates/core/src/lib.rs")).expect("read");
    fixture.write(
        "crates/core/src/lib.rs",
        &format!("{core}\npub fn twisty(a: u8) -> u8 {{ if a > 1 {{ if a > 2 {{ if a > 3 {{ if a > 4 {{ if a > 5 {{ 1 }} else {{ 2 }} }} else {{ 3 }} }} else {{ 4 }} }} else {{ 5 }} }} else {{ 6 }} }}\n"),
    );
    let (code, report) = fixture.json(&["complexity", "--fail-on-issues", "--baseline", path]);
    assert_eq!(code, 1, "{report:#}");
    assert_eq!(strings(&report, "name"), ["twisty"], "{report:#}");
    assert_eq!(report["baseline"]["path"], path);
}

#[test]
fn a_removed_finding_gives_a_stale_entry_and_the_flag_fails_on_it() {
    let fixture = Fixture::new();
    let path = fixture.root().join("baseline.json");
    let path = path.to_str().expect("UTF-8 path");
    let (code, _) = fixture.json(&["complexity", "--save-baseline", path]);
    assert_eq!(code, 0);
    let core = fs::read_to_string(fixture.root().join("crates/core/src/lib.rs")).expect("read");
    let start = core.find("pub fn branchy").expect("the function");
    let end = start + core[start..].find("\n}\n").expect("its end") + 3;
    fixture.write(
        "crates/core/src/lib.rs",
        &format!("{}{}", &core[..start], &core[end..]),
    );

    let (code, report) = fixture.json(&["complexity", "--baseline", path]);
    assert_eq!(code, 0, "{report:#}");
    let stale = report["baseline"]["stale"].as_array().expect("stale");
    assert_eq!(stale.len(), 1, "{report:#}");
    assert_eq!(stale[0]["check"], "complexity");
    assert_eq!(report["gate_outcomes"]["stale-baseline"]["status"], "fail");
    assert_eq!(report["gate_outcomes"]["stale-baseline"]["enforced"], false);

    let (code, report) =
        fixture.json(&["complexity", "--baseline", path, "--fail-on-stale-baseline"]);
    assert_eq!(code, 1, "{report:#}");
    let human = fixture.run(&["complexity", "--baseline", path, "--fail-on-stale-baseline"]);
    assert_eq!(human.code, 1);
    assert!(human.stdout.contains("1 stale"), "{}", human.stdout);

    // A check that did not run does not judge its entries.
    let (code, report) = fixture.json(&["dupes", "--baseline", path, "--fail-on-stale-baseline"]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["baseline"]["stale"], serde_json::json!([]));
}

#[test]
fn a_corrupt_or_missing_baseline_exits_2() {
    let fixture = Fixture::new();
    let path = fixture.root().join("baseline.json");
    for text in [
        "not json",
        "{\"schema_version\": 1}",
        "{\"schema_version\": 9, \"kind\": \"check\", \"language\": \"rust\", \"identity\": \"x\", \"findings\": []}",
    ] {
        fs::write(&path, text).expect("write");
        let (code, report) = fixture.json(&["size", "--baseline", path.to_str().expect("UTF-8")]);
        assert_eq!(code, 2, "{text}: {report:#}");
        assert_eq!(report["code"], "CARGO_FALLOW_INVALID_ARGUMENT", "{text}");
    }
    let missing = fixture.root().join("missing.json");
    let (code, _) = fixture.json(&["size", "--baseline", missing.to_str().expect("UTF-8")]);
    assert_eq!(code, 2);
    // A scoped save would hide the old findings outside the scope.
    let (code, _) = fixture.json(&[
        "size",
        "--save-baseline",
        "x.json",
        "--changed-since",
        "main",
    ]);
    assert_eq!(code, 2);
    let (code, _) = fixture.json(&["size", "--fail-on-stale-baseline"]);
    assert_eq!(code, 2);
}

#[test]
fn deps_and_visibility_read_a_baseline() {
    let fixture = Fixture::new();
    let path = fixture.root().join("baseline.json");
    let path = path.to_str().expect("UTF-8 path");
    for command in ["deps", "visibility"] {
        let (code, full) = fixture.json(&[command, "--fail-on-issues", "--save-baseline", path]);
        assert_eq!(code, 0, "{command}: {full:#}");
        assert!(
            full["total_issues"].as_u64() > Some(0),
            "{command}: {full:#}"
        );
        let (code, report) = fixture.json(&[command, "--fail-on-issues", "--baseline", path]);
        assert_eq!(code, 0, "{command}: {report:#}");
        assert_eq!(report["total_issues"], 0, "{command}: {report:#}");
        assert_eq!(
            report["baseline"]["matched"], full["total_issues"],
            "{command}"
        );
    }
}

#[test]
fn a_baseline_with_changed_since_does_not_judge_stale_entries() {
    let fixture = committed();
    let path = fixture.root().join("../baseline.json");
    let path = path.to_str().expect("UTF-8 path");
    let (code, _) = fixture.json(&["complexity", "--save-baseline", path]);
    assert_eq!(code, 0);
    let (code, report) = fixture.json(&[
        "complexity",
        "--baseline",
        path,
        "--changed-since",
        "main",
        "--fail-on-stale-baseline",
    ]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(
        report["gate_outcomes"]["stale-baseline"]["status"],
        "skipped"
    );
}

#[test]
fn a_clone_fingerprint_stays_when_another_file_gets_new_tokens() {
    let fixture = Fixture::new();
    let fingerprints = |fixture: &Fixture| -> Vec<String> {
        let (_, report) = fixture.json(&["dupes", "--min-tokens", "30"]);
        strings(&report, "fingerprint")
    };
    let before = fingerprints(&fixture);
    assert!(!before.is_empty());
    let engine = fs::read_to_string(fixture.root().join("crates/app/src/engine.rs")).expect("read");
    fixture.write(
        "crates/app/src/engine.rs",
        &format!("{engine}\npub fn zebra_quux(alpha_beta: u64) -> u64 {{ alpha_beta ^ 0x5eed }}\n"),
    );
    assert_eq!(fingerprints(&fixture), before);
}

#[test]
fn a_narrowing_flag_or_a_save_does_not_judge_or_enforce_the_baseline() {
    let fixture = Fixture::new();
    let path = fixture.root().join("baseline.json");
    let path = path.to_str().expect("UTF-8 path");
    let (_, _) = fixture.json(&["visibility", "--save-baseline", path]);
    // Each saved entry is `dead_public`, so a run on another kind sees none of
    // them, and none is stale.
    let (code, report) = fixture.json(&[
        "visibility",
        "--kind",
        "unnecessary-public",
        "--baseline",
        path,
        "--fail-on-stale-baseline",
    ]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(
        report["gate_outcomes"]["stale-baseline"]["status"],
        "skipped"
    );
    let (_, _) = fixture.json(&["dupes", "--min-tokens", "30", "--save-baseline", path]);
    let (code, report) = fixture.json(&[
        "dupes",
        "--min-tokens",
        "100",
        "--baseline",
        path,
        "--fail-on-stale-baseline",
    ]);
    assert_eq!(code, 0, "{report:#}");

    // The stale gate is in the gate block and in the SARIF log.
    let (_, _) = fixture.json(&["complexity", "--save-baseline", path]);
    fixture.write("crates/core/src/lib.rs", "pub fn small() {}\n");
    let human = fixture.run(&["complexity", "--baseline", path]);
    assert!(
        human
            .stdout
            .contains("  stale-baseline: fail, not enforced"),
        "{}",
        human.stdout
    );
    let sarif = fixture.run(&["complexity", "--baseline", path, "--format", "sarif"]);
    assert!(
        sarif.stdout.contains("The `stale-baseline` gate failed"),
        "{}",
        sarif.stdout
    );

    // A save run reports no enforced gate, also in the sections of `check`.
    let (code, report) = fixture.json(&[
        "check",
        "--skip",
        "deps",
        "--fail-on-issues",
        "--save-baseline",
        path,
    ]);
    assert_eq!(code, 0);
    let text = report.to_string();
    assert!(!text.contains("\"enforced\":true"), "{report:#}");

    // A run with a baseline gives no health score.
    let (_, report) = fixture.json(&["check", "--skip", "deps", "--baseline", path]);
    assert_eq!(report["health"]["scope"], "baseline", "{report:#}");
    assert!(report["health"]["penalties"].is_null());
}

#[test]
fn package_keeps_only_the_findings_of_the_named_members() {
    let fixture = Fixture::new();
    let (code, report) = fixture.json(&["boundaries", "-p", "process", "--fail-on-issues"]);
    assert_eq!(code, 1, "{report:#}");
    assert_eq!(
        strings(&report, "rule"),
        ["process-stays-a-leaf"],
        "{report:#}"
    );
    assert_eq!(report["packages"], serde_json::json!(["process"]));

    let (_, report) = fixture.json(&["boundaries", "--package", "app"]);
    assert_eq!(
        strings(&report, "rule"),
        [
            "process-tree-in-process",
            "render-does-not-run",
            "render-does-not-run"
        ],
        "{report:#}"
    );

    let (code, report) = fixture.json(&[
        "complexity",
        "-p",
        "app",
        "-p",
        "process",
        "--fail-on-issues",
    ]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["packages"], serde_json::json!(["app", "process"]));

    let (_, report) = fixture.json(&["deps", "-p", "process"]);
    let manifests: Vec<&str> = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter_map(|finding| finding["manifest_path"].as_str())
        .collect();
    assert_eq!(manifests, ["crates/process/Cargo.toml"], "{report:#}");

    // A clone group stays when one copy is in a named member.
    let (_, report) = fixture.json(&["dupes", "--min-tokens", "30", "-p", "process"]);
    assert_eq!(report["total_issues"], 1, "{report:#}");

    let (_, full) = fixture.json(&["check", "--skip", "deps"]);
    let (_, report) = fixture.json(&["check", "--skip", "deps", "-p", "core"]);
    assert_eq!(report["health"]["scope"], "package", "{report:#}");
    assert_eq!(report["packages"], serde_json::json!(["core"]));
    let expected: Vec<&str> = full["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter(|finding| {
            finding["location"]["path"]
                .as_str()
                .is_some_and(|path| path.starts_with("crates/core/"))
                || finding["check"] == "dupes"
        })
        .filter_map(|finding| finding["id"].as_str())
        .collect();
    let scoped: Vec<&str> = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter_map(|finding| finding["id"].as_str())
        .collect();
    assert_eq!(scoped, expected, "{report:#}");
    let human = fixture.run(&["check", "--skip", "deps", "-p", "core"]);
    assert!(
        human.stdout.contains("Scope: package core"),
        "{}",
        human.stdout
    );
}

#[test]
fn an_unknown_package_exits_2_and_names_the_members() {
    let fixture = Fixture::new();
    for command in [
        "check",
        "deps",
        "visibility",
        "files",
        "boundaries",
        "size",
        "complexity",
        "dupes",
    ] {
        let (code, report) = fixture.json(&[command, "-p", "nope"]);
        assert_eq!(code, 2, "{command}: {report:#}");
        assert_eq!(report["code"], "CARGO_FALLOW_INVALID_ARGUMENT", "{command}");
        assert!(
            report["help"]
                .as_str()
                .is_some_and(|help| help.contains("app, core, process")),
            "{command}: {report:#}"
        );
    }
    let (code, _) = fixture.json(&["size", "-p", "core", "--save-baseline", "x.json"]);
    assert_eq!(code, 2);
}

#[test]
fn a_clone_group_needs_one_copy_that_passes_each_filter() {
    let fixture = committed();
    let process =
        fs::read_to_string(fixture.root().join("crates/process/src/lib.rs")).expect("read");
    fixture.write(
        "crates/process/src/lib.rs",
        &format!("{process}// A change.\n"),
    );
    // The copy in `core` did not change, and the changed copy is not in `core`.
    let (_, report) = fixture.json(&[
        "dupes",
        "--min-tokens",
        "30",
        "--changed-since",
        "main",
        "-p",
        "core",
    ]);
    assert_eq!(report["total_issues"], 0, "{report:#}");
    let (_, report) = fixture.json(&[
        "dupes",
        "--min-tokens",
        "30",
        "--changed-since",
        "main",
        "-p",
        "process",
    ]);
    assert_eq!(report["total_issues"], 1, "{report:#}");
}

#[test]
fn compact_writes_one_line_for_each_finding() {
    let fixture = Fixture::new();
    let run = fixture.run(&["boundaries", "--format", "compact"]);
    assert_eq!(run.code, 0);
    assert_eq!(
        run.stdout,
        "crates/process/Cargo.toml:8:1: process-stays-a-leaf: process must not depend on workspace crate core\n\
         crates/app/src/lib.rs:9:13: process-tree-in-process: crates/app/src/lib.rs contains process_group(0), which belongs to process\n\
         crates/app/src/render.rs:1:5: render-does-not-run: crate::render must not use crate::engine::run\n\
         crates/app/src/render.rs:6:5: render-does-not-run: crate::render must not use crate::engine::stop\n"
    );
    let gated = fixture.run(&["boundaries", "--format", "compact", "--fail-on-issues"]);
    assert_eq!(gated.code, 1);
    // A run with no finding writes nothing.
    let empty = fixture.run(&["size", "--format", "compact"]);
    assert_eq!((empty.code, empty.stdout.as_str()), (0, ""));
    // Each command has the format.
    for command in [
        "check",
        "deps",
        "visibility",
        "files",
        "complexity",
        "dupes",
    ] {
        let run = fixture.run(&[command, "--format", "compact"]);
        assert_eq!(run.code, 0, "{command}");
        for line in run.stdout.lines() {
            assert!(line.contains(": "), "{command}: {line}");
        }
    }
}

#[test]
fn markdown_has_a_gate_table_and_a_table_for_each_check() {
    let fixture = Fixture::new();
    let run = fixture.run(&[
        "check",
        "--skip",
        "deps",
        "--format",
        "markdown",
        "--fail-on-issues",
    ]);
    assert_eq!(run.code, 1);
    let text = &run.stdout;
    assert!(text.starts_with("## cargo fallow check: "), "{text}");
    assert!(text.contains("\n| Gate | Status | Enforced |\n"), "{text}");
    assert!(
        text.contains("| `boundary-violations` | **fail** | yes |"),
        "{text}"
    );
    assert!(text.contains("\n### boundaries (4)\n"), "{text}");
    assert!(text.contains("\n### size (0)\n\nNo issues.\n"), "{text}");
    assert!(!text.contains("### parse"), "{text}");
    assert!(
        text.contains("| `crates/app/src/render.rs:1` | `render-does-not-run` | crate::render must not use crate::engine::run |"),
        "{text}"
    );
    let single = fixture.run(&["deps", "--format", "markdown"]);
    assert!(
        single
            .stdout
            .starts_with("## cargo fallow deps: 3 issues\n"),
        "{}",
        single.stdout
    );
    for command in ["visibility", "files", "boundaries", "complexity", "dupes"] {
        let run = fixture.run(&[command, "--format", "markdown"]);
        assert!(
            run.stdout
                .starts_with(&format!("## cargo fallow {command}: "))
                && run.stdout.contains(&format!("\n### {command} (")),
            "{command}: {}",
            run.stdout
        );
    }
    let empty = fixture.run(&["size", "--format", "markdown"]);
    assert!(
        empty
            .stdout
            .starts_with("## cargo fallow size: no issues found\n"),
        "{}",
        empty.stdout
    );
}

#[test]
fn fix_has_no_compact_or_markdown_output() {
    let fixture = Fixture::new();
    for format in [
        "compact",
        "markdown",
        "github-annotations",
        "github-summary",
    ] {
        let run = fixture.run(&["fix", "--dry-run", "--format", format]);
        assert_eq!(run.code, 2, "{format}: {}", run.stdout);
        // `explain` reads no workspace, so the run has no `--manifest-path`.
        let explain = Command::cargo_bin("cargo-fallow")
            .expect("find the binary")
            .args(["fallow", "explain", "unused-dependency", "--format", format])
            .output()
            .expect("run cargo-fallow");
        assert_eq!(explain.status.code(), Some(2), "{format}");
        assert!(
            String::from_utf8_lossy(&explain.stderr)
                .contains("no SARIF, CodeClimate, compact, markdown, GitHub or badge"),
            "{format}: {explain:?}"
        );
    }
}

/// The output of a jq filter of the action, or `None` when jq is missing.
fn action_jq(filter: &str, input: &str, args: &[(&str, &str)]) -> Option<String> {
    let mut command = StdCommand::new("jq");
    command.arg("-r");
    for (name, value) in args {
        command.args(["--arg", name, value]);
    }
    let output = command
        .arg("-f")
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("action/jq")
                .join(filter),
        )
        .arg(input)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[test]
fn github_annotations_equal_the_filter_of_the_action() {
    let fixture = committed();
    for command in ["check", "boundaries", "complexity", "dupes", "visibility"] {
        let sarif = fixture.run(&[command, "--format", "sarif"]);
        let path = fixture.root().join("../log.sarif");
        fs::write(&path, &sarif.stdout).expect("write the log");
        let annotations = fixture.run(&[command, "--format", "github-annotations"]);
        assert_eq!(annotations.code, 0, "{command}");
        let Some(expected) = action_jq(
            "annotations.jq",
            path.to_str().expect("UTF-8"),
            &[("max", "50"), ("prefix", "")],
        ) else {
            // A machine without jq cannot run the filter. The CI runners have it.
            return;
        };
        assert_eq!(annotations.stdout, expected, "{command}");
        assert!(!annotations.stdout.is_empty(), "{command}");
    }
}

#[test]
fn github_annotations_start_at_the_repository_root() {
    let fixture = Fixture::new();
    let repo = fixture.root().join("../repo");
    // The workspace is a folder of a larger repository.
    copy_dir(fixture.root(), &repo.join("tools/ws"));
    git_in(&repo, &["init", "--quiet"]);
    let output = Command::cargo_bin("cargo-fallow")
        .expect("find the binary")
        .current_dir(repo.join("tools/ws"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args([
            "fallow",
            "boundaries",
            "--format",
            "github-annotations",
            "--quiet",
        ])
        .output()
        .expect("run cargo-fallow");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.lines()
            .all(|line| line.contains("file=tools/ws/crates/")),
        "{text}"
    );
    // The same lines as the filter of the action with the prefix.
    let sarif = Command::cargo_bin("cargo-fallow")
        .expect("find the binary")
        .current_dir(repo.join("tools/ws"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(["fallow", "boundaries", "--format", "sarif", "--quiet"])
        .output()
        .expect("run cargo-fallow");
    let path = repo.join("log.sarif");
    fs::write(&path, &sarif.stdout).expect("write the log");
    if let Some(expected) = action_jq(
        "annotations.jq",
        path.to_str().expect("UTF-8"),
        &[("max", "50"), ("prefix", "tools/ws/")],
    ) {
        assert_eq!(text, expected);
    }
}

#[test]
fn github_summary_has_the_gates_and_the_findings() {
    let fixture = Fixture::new();
    let run = fixture.run(&[
        "boundaries",
        "--format",
        "github-summary",
        "--fail-on-issues",
    ]);
    assert_eq!(run.code, 1);
    assert!(
        run.stdout
            .starts_with("### cargo fallow boundaries\n\n4 issues, exit code 1.\n"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout
            .contains("| `boundary-violations` | fail | yes | 4 |"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("\n#### Findings\n"), "{}", run.stdout);
    // An error is an annotation on stdout with the code in the title.
    let error = fixture.run(&[
        "size",
        "--base",
        "no-such-ref",
        "--format",
        "github-annotations",
    ]);
    assert_eq!(error.code, 2);
    assert!(
        error
            .stdout
            .starts_with("::error title=cargo-fallow CARGO_FALLOW_INVALID_ARGUMENT::"),
        "{}",
        error.stdout
    );
    let summary = fixture.run(&[
        "size",
        "--base",
        "no-such-ref",
        "--format",
        "github-summary",
    ]);
    assert_eq!(summary.code, 2);
    assert!(
        summary
            .stdout
            .starts_with("### cargo fallow\n\nThe run failed with exit code 2: "),
        "{}",
        summary.stdout
    );
}

impl Fixture {
    /// Adds a `[workspace.metadata.fallow.rules]` table to the root manifest.
    fn rules(&self, table: &str) {
        let manifest = fs::read_to_string(self.manifest()).expect("read the manifest");
        self.write(
            "Cargo.toml",
            &format!("{manifest}\n[workspace.metadata.fallow.rules]\n{table}\n"),
        );
    }
}

#[test]
fn a_rule_level_turns_a_rule_off_or_changes_its_gate() {
    // `off` removes the findings of the rule.
    let fixture = Fixture::new();
    fixture.rules("complexity = \"off\"");
    let (code, report) = fixture.json(&["complexity", "--fail-on-issues"]);
    assert_eq!(
        (code, report["total_issues"].as_u64()),
        (0, Some(0)),
        "{report:#}"
    );

    // `warn` keeps the findings, and the gate does not count them.
    let fixture = Fixture::new();
    fixture.rules("complexity = \"warning\"\nprocess-stays-a-leaf = \"warn\"");
    let (code, report) = fixture.json(&["complexity", "--fail-on-issues"]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["total_issues"], 1);
    assert_eq!(report["gate_outcomes"]["health-findings"]["status"], "pass");
    let (code, report) = fixture.json(&["boundaries", "--fail-on-issues"]);
    assert_eq!(code, 1, "the other rules still count: {report:#}");
    assert_eq!(
        report["gate_outcomes"]["boundary-violations"]["observed"],
        3.0
    );

    // A boundary kind names each rule of the kind.
    let fixture = Fixture::new();
    fixture.rules("module-path = \"off\"");
    let (_, report) = fixture.json(&["boundaries"]);
    assert_eq!(
        strings(&report, "rule"),
        ["process-stays-a-leaf", "process-tree-in-process"],
        "{report:#}"
    );

    // `error` gives the error severity in the shared shape and in SARIF.
    let fixture = Fixture::new();
    fixture.rules("dead_public = \"error\"\nunused-dev-dependency = \"warn\"");
    let (_, report) = fixture.json(&["check", "--only", "visibility,deps"]);
    let severities: BTreeSet<(String, String)> = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .map(|finding| {
            (
                finding["rule"].as_str().unwrap_or_default().to_owned(),
                finding["severity"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    assert!(
        severities.contains(&("dead_public".to_owned(), "error".to_owned())),
        "{severities:?}"
    );
    assert!(
        severities.contains(&("unused-dev-dependency".to_owned(), "warn".to_owned())),
        "{severities:?}"
    );
    // The deps gate counts the errors: the dev-dependency is a warning now.
    assert_eq!(
        report["gate_outcomes"]["error-severity-findings"]["observed"],
        2.0
    );
    let sarif = fixture.run(&["visibility", "--format", "sarif"]);
    let log: Value = serde_json::from_str(&sarif.stdout).expect("SARIF");
    assert!(
        log["runs"][0]["results"]
            .as_array()
            .expect("results")
            .iter()
            .all(|result| result["level"] == "error"),
        "{log:#}"
    );
}

#[test]
fn an_unknown_rule_or_level_is_an_invalid_config_error() {
    for table in [
        "no-such-rule = \"off\"",
        "complexity = \"loud\"",
        "parse-error = \"off\"",
    ] {
        let fixture = Fixture::new();
        fixture.rules(table);
        let (code, report) = fixture.json(&["complexity"]);
        assert_eq!(code, 2, "{table}: {report:#}");
        assert_eq!(report["code"], "CARGO_FALLOW_INVALID_CONFIG", "{table}");
    }
    // A boundary rule name from the configuration is a rule.
    let fixture = Fixture::new();
    fixture.rules("render-does-not-run = \"off\"");
    let (code, report) = fixture.json(&["boundaries"]);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["total_issues"], 2);
}

#[test]
fn a_boundary_kind_level_sets_the_severity_the_sarif_level_and_the_baseline() {
    let fixture = Fixture::new();
    fixture.rules("source-pattern = \"error\"\nmodule-path = \"off\"");
    let (_, report) = fixture.json(&["check", "--only", "boundaries"]);
    let pattern = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .find(|finding| finding["rule"] == "process-tree-in-process")
        .expect("the source rule finding");
    assert_eq!(pattern["severity"], "error", "{report:#}");
    let sarif = fixture.run(&["boundaries", "--format", "sarif"]);
    let log: Value = serde_json::from_str(&sarif.stdout).expect("SARIF");
    let levels: Vec<(&str, &str)> = log["runs"][0]["results"]
        .as_array()
        .expect("results")
        .iter()
        .map(|result| {
            (
                result["ruleId"].as_str().unwrap_or_default(),
                result["level"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        levels,
        [
            ("process-stays-a-leaf", "warning"),
            ("process-tree-in-process", "error")
        ]
    );
    // The baseline of a run has no entry of a rule that is off.
    let path = fixture.root().join("baseline.json");
    let (_, _) = fixture.json(&[
        "boundaries",
        "--save-baseline",
        path.to_str().expect("UTF-8"),
    ]);
    let saved = fs::read_to_string(&path).expect("read the baseline");
    assert!(!saved.contains("render-does-not-run"), "{saved}");

    // `boundaries` names each boundary rule.
    let fixture = Fixture::new();
    fixture.rules("boundaries = \"off\"");
    let (_, report) = fixture.json(&["boundaries"]);
    assert_eq!(report["total_issues"], 0, "{report:#}");
}

#[test]
fn an_off_duplication_rule_has_no_threshold_gate() {
    let fixture = Fixture::new();
    fixture.rules("code-duplication = \"off\"");
    let (code, report) = fixture.json(&["dupes", "--min-tokens", "30", "--threshold", "1"]);
    assert_eq!(code, 0, "{report:#}");
    assert!(
        report["gate_outcomes"]["duplication-threshold"].is_null(),
        "{report:#}"
    );
}
