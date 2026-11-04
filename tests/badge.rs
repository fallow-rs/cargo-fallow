//! Black-box tests of `--format badge`.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;

use assert_cmd::Command;

fn package() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    )
    .expect("write the manifest");
    fs::create_dir_all(dir.path().join("src")).expect("create a folder");
    fs::write(dir.path().join("src/lib.rs"), "fn private() {}\n").expect("write a file");
    dir
}

fn run(dir: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run the binary");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn check_gives_a_badge_with_the_grade() {
    let dir = package();
    let (code, svg, stderr) = run(
        dir.path(),
        &["check", "--format", "badge", "--quiet", "--offline"],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(svg.starts_with("<svg "), "{svg}");
    assert!(svg.contains("aria-label=\"fallow rust: A (100)\""), "{svg}");
}

#[test]
fn a_partial_run_gives_the_no_score_badge() {
    let dir = package();
    let (code, svg, stderr) = run(
        dir.path(),
        &["check", "--only", "files", "--format", "badge", "--quiet"],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(svg.contains("fallow rust: no score"), "{svg}");
}

#[test]
fn a_command_without_a_score_refuses_the_badge() {
    let dir = package();
    let (code, svg, stderr) = run(dir.path(), &["files", "--format", "badge", "--quiet"]);
    assert_eq!(code, 2, "{svg}");
    assert!(
        stderr.contains("cargo fallow check --format badge"),
        "{stderr}"
    );
}

#[test]
fn the_badge_exits_0_when_a_gate_fails_and_mutants_refuses_it_at_once() {
    let dir = package();
    fs::write(dir.path().join("src/orphan.rs"), "").expect("write a file");
    let (code, svg, stderr) = run(
        dir.path(),
        &[
            "check",
            "--format",
            "badge",
            "--fail-on-issues",
            "--quiet",
            "--offline",
        ],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(svg.starts_with("<svg "), "{svg}");
    let (code, _, stderr) = run(dir.path(), &["mutants", "--format", "badge", "--quiet"]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("cargo fallow check --format badge"),
        "{stderr}"
    );
}
