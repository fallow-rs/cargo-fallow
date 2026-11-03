//! Black-box tests of the two modes of `mutants` that compile nothing and run no
//! test: `--list` and `--inspect <id> --no-build`.
//!
//! Each test puts a `cargo` script first on the `PATH` and in `CARGO`. The script
//! passes `cargo metadata` to the real cargo, and records each other call and
//! fails. So a build, a test run or a new lockfile makes the test fail. The
//! modes can still run `rustc -vV` for the key of the map, and `git` for
//! `--in-diff`. Neither compiles the project.
#![cfg(unix)]
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// `check` has a mutant on line 3 that a test executes. `orphan` has a mutant on
/// line 8 that no test executes.
const SOURCE: &str = r"/// Has a test.
pub fn check(a: u8) -> bool {
    a == 1
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
    }
}
";

const OTHER: &str = "pub fn other(a: u8) -> bool {\n    a > 4\n}\n";

/// One run at a time: a run with a real build has up to two test processes.
static ONE_RUN: Mutex<()> = Mutex::new(());

/// The real cargo of the test run.
fn real_cargo() -> PathBuf {
    std::env::var_os("CARGO").map_or_else(|| PathBuf::from("cargo"), PathBuf::from)
}

/// A workspace with the members `a` and `b`, and a lockfile. `b` has [`SOURCE`].
fn workspace() -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    fs::write(
        dir.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"a\", \"b\"]\nresolver = \"2\"\n",
    )
    .expect("write the manifest");
    for (name, source) in [("a", OTHER), ("b", SOURCE)] {
        let folder = dir.path().join(name).join("src");
        fs::create_dir_all(&folder).expect("create the source folder");
        fs::write(
            dir.path().join(name).join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .expect("write a manifest");
        fs::write(folder.join("lib.rs"), source).expect("write a source file");
    }
    let status = Process::new(real_cargo())
        .current_dir(dir.path())
        .args(["generate-lockfile", "--offline"])
        .status()
        .expect("cargo runs");
    assert!(status.success(), "generate the lockfile");
    dir
}

/// A folder with a `cargo` script that allows only `cargo metadata`.
struct FakeCargo {
    dir: TempDir,
}

impl FakeCargo {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let log = dir.path().join("calls.log");
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = metadata ]; then exec '{}' \"$@\"; fi\n\
             echo \"$*\" >> '{}'\nexit 97\n",
            real_cargo().display(),
            log.display()
        );
        let path = dir.path().join("cargo");
        fs::write(&path, script).expect("write the script");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("make it executable");
        Self { dir }
    }

    fn program(&self) -> PathBuf {
        self.dir.path().join("cargo")
    }

    /// The calls other than `cargo metadata`.
    fn calls(&self) -> String {
        fs::read_to_string(self.dir.path().join("calls.log")).unwrap_or_default()
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

/// Runs `cargo-fallow mutants`. With `fake`, only `cargo metadata` can run.
fn mutants(dir: &Path, fake: Option<&FakeCargo>, flags: &[&str]) -> Output {
    let _one_run = ONE_RUN.lock().unwrap_or_else(PoisonError::into_inner);
    let mut command = Command::cargo_bin("cargo-fallow").expect("the binary exists");
    command
        .env("MBX_DISABLE", "1")
        .env_remove("NEXTEST_PROFILE")
        .args(["mutants", "--quiet", "--jobs", "2", "--manifest-path"])
        .arg(dir.join("Cargo.toml"))
        .args(flags)
        .timeout(Duration::from_secs(280));
    if let Some(fake) = fake {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut parts = vec![fake.dir.path().to_path_buf()];
        parts.extend(std::env::split_paths(&path));
        command
            .env("PATH", std::env::join_paths(parts).expect("a valid PATH"))
            .env("CARGO", fake.program());
    }
    let output = command.output().expect("the binary runs");
    Output {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// The id of the mutant on `line`, from `--list`.
fn id_on_line(report: &Value, line: u64) -> String {
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
fn list_gives_the_mutants_of_one_package_without_a_build() {
    let dir = workspace();
    let fake = FakeCargo::new();
    let flags = ["--list", "-p", "b", "--tier", "schema", "--format", "json"];
    let run = mutants(dir.path(), Some(&fake), &flags);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(fake.calls(), "", "the list ran cargo");
    let report = run.json();
    assert_eq!(report["kind"], "mutants", "{report}");
    assert_eq!(report["language"], "rust", "{report}");
    assert_eq!(report["package"], "b", "{report}");
    assert_eq!(report["total_issues"], 0, "{report}");
    let gate = &report["gate_outcomes"]["surviving-mutants"];
    assert_eq!(gate["status"], "skipped", "{report}");
    assert_eq!(report["selection"]["map"], "none", "{report}");
    let rows = report["mutants"].as_array().expect("the mutants");
    let lines: Vec<&Value> = rows.iter().map(|row| &row["line"]).collect();
    assert_eq!(lines, [3, 8], "{report}");
    for row in rows {
        assert_eq!(row["status"], "pending", "{report}");
        assert_eq!(row["file"], "b/src/lib.rs", "{report}");
        assert_eq!(row["tier"], "schema", "{report}");
    }
    assert_eq!(report["summary"]["pending"], 2, "{report}");

    // The other member has its own mutants.
    let other = mutants(
        dir.path(),
        Some(&fake),
        &["--list", "--package", "a", "--format", "json"],
    );
    assert_eq!(other.code, Some(0), "{}", other.stderr);
    let other = other.json();
    assert_eq!(other["package"], "a", "{other}");
    assert!(
        other["mutants"]
            .as_array()
            .expect("the mutants")
            .iter()
            .all(|row| row["file"] == "a/src/lib.rs"),
        "{other}"
    );
    assert_eq!(fake.calls(), "", "the list ran cargo");
}

#[test]
fn list_needs_a_package_in_a_workspace_with_more_than_one_package() {
    let dir = workspace();
    let fake = FakeCargo::new();
    let run = mutants(dir.path(), Some(&fake), &["--list", "--format", "json"]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    let error = run.json();
    assert_eq!(error["code"], "CARGO_FALLOW_INVALID_ARGUMENT", "{error}");
    assert_eq!(fake.calls(), "", "the list ran cargo");
}

#[test]
fn list_keeps_only_the_files_of_the_file_filter() {
    let dir = workspace();
    let lib = dir.path().join("b/src/lib.rs");
    fs::write(&lib, format!("{SOURCE}\npub mod extra;\n")).expect("add a module");
    fs::write(dir.path().join("b/src/extra.rs"), OTHER).expect("write the module");
    let fake = FakeCargo::new();
    let all = mutants(
        dir.path(),
        Some(&fake),
        &["--list", "-p", "b", "--format", "json"],
    );
    let all = all.json();
    let files: Vec<&Value> = all["mutants"]
        .as_array()
        .expect("the mutants")
        .iter()
        .map(|row| &row["file"])
        .collect();
    assert!(files.contains(&&Value::from("b/src/extra.rs")), "{all}");
    assert!(files.contains(&&Value::from("b/src/lib.rs")), "{all}");

    let flags = [
        "--list",
        "-p",
        "b",
        "--file",
        "b/src/extra.rs",
        "--format",
        "json",
    ];
    let only = mutants(dir.path(), Some(&fake), &flags);
    assert_eq!(only.code, Some(0), "{}", only.stderr);
    let only = only.json();
    let rows = only["mutants"].as_array().expect("the mutants");
    assert!(!rows.is_empty(), "{only}");
    assert!(
        rows.iter().all(|row| row["file"] == "b/src/extra.rs"),
        "{only}"
    );

    // A path of another package matches no file of this package.
    let flags = [
        "--list",
        "-p",
        "b",
        "--file",
        "a/src/lib.rs",
        "--format",
        "json",
    ];
    let run = mutants(dir.path(), Some(&fake), &flags);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert_eq!(run.json()["code"], "CARGO_FALLOW_INVALID_ARGUMENT");
    assert_eq!(fake.calls(), "", "the list ran cargo");
}

#[test]
fn inspect_without_a_build_reads_the_map_of_a_single_package() {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"single\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .expect("write the manifest");
    fs::create_dir_all(dir.path().join("src")).expect("create the source folder");
    fs::write(dir.path().join("src/lib.rs"), SOURCE).expect("write the source");
    let status = Process::new(real_cargo())
        .current_dir(dir.path())
        .args(["generate-lockfile", "--offline"])
        .status()
        .expect("cargo runs");
    assert!(status.success(), "generate the lockfile");
    let fake = FakeCargo::new();
    let listed = mutants(dir.path(), Some(&fake), &["--list", "--format", "json"]);
    assert_eq!(listed.code, Some(0), "{}", listed.stderr);
    let id = id_on_line(&listed.json(), 3);

    let built = mutants(dir.path(), None, &["--inspect", &id, "--format", "json"]);
    assert_eq!(built.code, Some(0), "{}", built.stderr);
    assert_eq!(built.json()["coverage"]["map"], "built");

    let flags = ["--inspect", &id, "--no-build", "--format", "json"];
    let run = mutants(dir.path(), Some(&fake), &flags);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let report = run.json();
    assert_eq!(report["coverage"]["map"], "cached", "{report}");
    assert_eq!(report["coverage"]["state"], "covered", "{report}");
    assert_eq!(fake.calls(), "", "the inspect ran cargo");
}

#[test]
fn list_conflicts_with_a_flag_that_runs_mutants() {
    let dir = workspace();
    for flag in ["--no-run", "--fail-on-survivors", "--inspect"] {
        let mut flags = vec!["--list", "-p", "b", flag];
        if flag == "--inspect" {
            flags.push("0000000000000001");
        }
        let run = mutants(dir.path(), None, &flags);
        assert_eq!(run.code, Some(2), "{flag}: {}", run.stderr);
    }
}

#[test]
fn no_build_needs_inspect() {
    let dir = workspace();
    let run = mutants(dir.path(), None, &["-p", "b", "--no-build"]);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
}

#[test]
fn inspect_without_a_build_and_without_a_map_exits_2_with_its_own_code() {
    let dir = workspace();
    let fake = FakeCargo::new();
    let listed = mutants(
        dir.path(),
        Some(&fake),
        &["--list", "-p", "b", "--format", "json"],
    );
    let id = id_on_line(&listed.json(), 3);
    let flags = [
        "--inspect",
        &id,
        "--no-build",
        "-p",
        "b",
        "--format",
        "json",
    ];
    let run = mutants(dir.path(), Some(&fake), &flags);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    let error = run.json();
    assert_eq!(error["code"], "CARGO_FALLOW_NO_CACHED_MAP", "{error}");
    let help = error["help"].as_str().expect("a help text");
    assert!(
        help.contains("`cargo fallow mutants --package b "),
        "{help}"
    );
    assert!(help.contains(&format!(" --inspect {id}`")), "{help}");
    assert_eq!(fake.calls(), "", "the inspect ran cargo");

    // The human form gives the same error on stderr.
    let human = mutants(
        dir.path(),
        Some(&fake),
        &["--inspect", &id, "--no-build", "-p", "b"],
    );
    assert_eq!(human.code, Some(2), "{}", human.stderr);
    assert!(
        human.stderr.contains("the cache has no coverage map"),
        "{}",
        human.stderr
    );
}

#[test]
fn inspect_without_a_build_names_a_missing_lockfile() {
    let dir = workspace();
    fs::remove_file(dir.path().join("Cargo.lock")).expect("remove the lockfile");
    let fake = FakeCargo::new();
    let listed = mutants(
        dir.path(),
        Some(&fake),
        &["--list", "-p", "b", "--format", "json"],
    );
    let id = id_on_line(&listed.json(), 3);
    let flags = ["--inspect", &id, "--no-build", "--format", "json"];
    let run = mutants(dir.path(), Some(&fake), &flags);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    let error = run.json();
    assert_eq!(error["code"], "CARGO_FALLOW_NO_CACHED_MAP", "{error}");
    let help = error["help"].as_str().expect("a help text");
    assert!(help.contains("cargo generate-lockfile"), "{help}");
    assert_eq!(fake.calls(), "", "the inspect ran cargo");
}

#[test]
fn inspect_without_a_build_reads_the_map_of_an_earlier_inspect() {
    let dir = workspace();
    let fake = FakeCargo::new();
    let listed = mutants(
        dir.path(),
        Some(&fake),
        &["--list", "-p", "b", "--format", "json"],
    );
    let listed = listed.json();
    let covered = id_on_line(&listed, 3);
    let uncovered = id_on_line(&listed, 8);

    // A plain inspect builds the map with the real cargo and keeps it.
    let built = mutants(
        dir.path(),
        None,
        &["--inspect", &covered, "-p", "b", "--format", "json"],
    );
    assert_eq!(built.code, Some(0), "{}", built.stderr);
    assert_eq!(built.json()["coverage"]["map"], "built");

    let flags = [
        "--inspect",
        &covered,
        "--no-build",
        "-p",
        "b",
        "--format",
        "json",
    ];
    let run = mutants(dir.path(), Some(&fake), &flags);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    let report = run.json();
    assert_eq!(report["kind"], "mutants-inspect", "{report}");
    assert_eq!(report["package"], "b", "{report}");
    assert_eq!(report["mutant"]["file"], "b/src/lib.rs", "{report}");
    assert_eq!(report["coverage"]["map"], "cached", "{report}");
    assert_eq!(report["coverage"]["state"], "covered", "{report}");
    let tests = report["coverage"]["tests"].as_array().expect("the tests");
    assert!(
        tests.iter().any(|test| test
            .as_str()
            .is_some_and(|name| name.ends_with("check_works"))),
        "{report}"
    );

    // A mutant that no test executes, without the package flag.
    let flags = ["--inspect", &uncovered, "--no-build", "--format", "json"];
    let run = mutants(dir.path(), Some(&fake), &flags);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.json()["coverage"]["state"], "not_covered");

    // The list now says that the cache has a map.
    let listed = mutants(
        dir.path(),
        Some(&fake),
        &["--list", "-p", "b", "--format", "json"],
    );
    let listed = listed.json();
    assert_eq!(listed["selection"]["map"], "cached", "{listed}");
    assert!(
        listed["selection"]["tests_mapped"].as_u64() >= Some(1),
        "{listed}"
    );
    assert_eq!(fake.calls(), "", "a mode without a build ran cargo");

    // A change of the source makes the map stale, so the mode stops.
    let path = dir.path().join("b/src/lib.rs");
    fs::write(&path, SOURCE.replace("a == 3", "a == 4")).expect("change the source");
    let flags = [
        "--inspect",
        &covered,
        "--no-build",
        "-p",
        "b",
        "--format",
        "json",
    ];
    let run = mutants(dir.path(), Some(&fake), &flags);
    assert_eq!(run.code, Some(2), "{}", run.stderr);
    assert_eq!(run.json()["code"], "CARGO_FALLOW_NO_CACHED_MAP");
}
