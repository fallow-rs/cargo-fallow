//! Black-box tests of `--locked`, `--offline` and `--frozen`. Each command
//! that calls `cargo metadata` accepts the three flags.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fs;

use assert_cmd::Command;

/// The commands that read the workspace with `cargo metadata`, with the
/// extra arguments that each command needs.
const COMMANDS: &[&[&str]] = &[
    &["check"],
    &["deps"],
    &["visibility"],
    &["files"],
    &["boundaries"],
    &["size"],
    &["complexity"],
    &["dupes"],
    &["fix", "--dry-run"],
    &["init"],
    &["mutants", "--list"],
];

#[test]
fn each_command_that_reads_the_workspace_accepts_the_cargo_flags() {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    )
    .expect("write the manifest");
    fs::create_dir_all(dir.path().join("src")).expect("create a folder");
    fs::write(dir.path().join("src/lib.rs"), "pub fn f() {}\n").expect("write a file");
    fs::write(
        dir.path().join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n",
    )
    .expect("write the lock file");
    for command in COMMANDS {
        for flag in ["--locked", "--offline", "--frozen"] {
            let output = Command::cargo_bin("cargo-fallow")
                .expect("the binary exists")
                .current_dir(dir.path())
                .args(*command)
                .args([flag, "--format", "json", "--quiet"])
                .output()
                .expect("run the binary");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{command:?} {flag}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

/// A `cargo` that writes its arguments to a log file and then runs the real
/// `cargo`.
#[cfg(unix)]
fn logging_cargo(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let real = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let script = dir.join("logging-cargo");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$*\" >> \"{}\"\nexec \"{real}\" \"$@\"\n",
            dir.join("cargo.log").display()
        ),
    )
    .expect("write the script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("set the mode");
    script
}

#[cfg(unix)]
#[test]
fn the_flag_reaches_the_cargo_metadata_call_of_each_command() {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    )
    .expect("write the manifest");
    fs::create_dir_all(dir.path().join("src")).expect("create a folder");
    fs::write(dir.path().join("src/lib.rs"), "pub fn f() {}\n").expect("write a file");
    let tools = tempfile::tempdir().expect("create a temporary directory");
    let cargo = logging_cargo(tools.path());
    for command in [
        &["visibility"][..],
        &["files"],
        &["dupes"],
        &["mutants", "--list"],
    ] {
        let _ = fs::remove_file(tools.path().join("cargo.log"));
        let output = Command::cargo_bin("cargo-fallow")
            .expect("the binary exists")
            .current_dir(dir.path())
            .env("CARGO", &cargo)
            .args(command)
            .args(["--offline", "--format", "json", "--quiet"])
            .output()
            .expect("run the binary");
        assert_eq!(output.status.code(), Some(0), "{command:?}");
        let log = fs::read_to_string(tools.path().join("cargo.log")).expect("read the log");
        assert!(
            log.lines()
                .any(|line| line.starts_with("metadata") && line.contains("--offline")),
            "{command:?}: {log}"
        );
    }
}
