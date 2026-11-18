//! Black-box tests of the name resolution through many glob imports.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::time::Duration;

use assert_cmd::Command;
use serde_json::Value;

/// The modules of the generated `features` module.
const MODULES: usize = 200;

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create a folder");
    }
    fs::write(path, text).expect("write a file");
}

/// A module declares many child modules and glob-imports each of them. Each
/// child glob-imports its parent and the prelude of a library crate, as the
/// generated bindings of `web-sys` do. The glob imports form a cycle through
/// each child. A lookup must not resolve the path of a glob import inside
/// the walk of another glob import, so the run ends quickly. A name from the
/// prelude must still give credit.
#[test]
fn many_glob_imports_through_a_cycle_resolve_quickly() {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\", \"base\"]\nresolver = \"3\"\n",
    );
    write(
        root,
        "base/Cargo.toml",
        "[package]\nname = \"base\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n",
    );
    write(
        root,
        "base/src/lib.rs",
        "pub mod prelude {\n    pub struct Marker;\n    impl Marker {\n        pub fn touch() {}\n    }\n    pub fn unused() {}\n}\n",
    );
    write(
        root,
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [dependencies]\nbase = { path = \"../base\" }\n",
    );
    write(
        root,
        "app/src/main.rs",
        "mod features;\nfn main() {\n    features::T0::go();\n}\n",
    );
    let mut features = String::new();
    for index in 0..MODULES {
        writeln!(features, "mod m{index};\npub use m{index}::*;").expect("write to a string");
        write(
            root,
            &format!("app/src/features/m{index}.rs"),
            &format!(
                "use super::*;\nuse base::prelude::*;\npub struct T{index};\n\
                 impl T{index} {{\n    pub fn go() {{\n        Marker::touch();\n    }}\n}}\n"
            ),
        );
    }
    write(root, "app/src/features/mod.rs", &features);

    let output = Command::cargo_bin("cargo-fallow")
        .expect("find the binary")
        .args([
            "visibility",
            "--format",
            "json",
            "--quiet",
            "--manifest-path",
        ])
        .arg(root.join("Cargo.toml"))
        .timeout(Duration::from_secs(20))
        .output()
        .expect("run the binary");
    assert_eq!(
        output.status.code(),
        Some(0),
        "the run did not finish in time: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("the output is JSON");
    let text = report["findings"].to_string();
    assert!(!text.contains("Marker"), "{text}");
    assert!(text.contains("\"item\":\"prelude::unused\""), "{text}");
}
