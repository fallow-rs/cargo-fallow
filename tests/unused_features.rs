//! Black-box tests of the `unused-feature` rule of the `deps` command.
//!
//! A finding is `package feature`, for example `app unused`.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("a file path has a parent")).expect("create a folder");
    fs::write(full, text).expect("write a file");
}

fn manifest(name: &str, rest: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n{rest}"
    )
}

/// A workspace with the members `app`, `alpha` and `beta`. `root_extra` goes
/// after the `[workspace]` table.
fn workspace(root_extra: &str, files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        &format!(
            "[workspace]\nmembers = [\"app\", \"alpha\", \"beta\"]\nresolver = \"3\"\n{root_extra}"
        ),
    );
    for (path, text) in files {
        write(root, path, text);
    }
    for (path, text) in [
        ("app/Cargo.toml", manifest("app", "")),
        ("alpha/Cargo.toml", manifest("alpha", "")),
        ("beta/Cargo.toml", manifest("beta", "")),
        ("app/src/lib.rs", String::new()),
        ("alpha/src/lib.rs", "pub fn f() {}\n".to_owned()),
        ("beta/src/lib.rs", "pub fn f() {}\n".to_owned()),
    ] {
        if !root.join(path).exists() {
            write(root, path, &text);
        }
    }
    dir
}

fn run(dir: &TempDir, extra: &[&str]) -> (Option<i32>, Value) {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args(["deps", "--format", "json", "--quiet", "--offline"])
        .args(extra)
        .output()
        .expect("run the binary");
    let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "the output is JSON: {err}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code(), report)
}

/// The `unused-feature` findings as `package feature`.
fn unused_features(report: &Value) -> BTreeSet<String> {
    report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter(|finding| finding["rule"] == "unused-feature")
        .map(|finding| {
            format!(
                "{} {}",
                finding["package"].as_str().unwrap_or_default(),
                finding["dependency"].as_str().unwrap_or_default()
            )
        })
        .collect()
}

#[test]
fn a_feature_that_enables_nothing_and_that_no_cfg_names_is_reported() {
    let dir = workspace(
        "",
        &[
            (
                "app/Cargo.toml",
                &manifest(
                    "app",
                    "[features]\nunused = []\nin_cfg = []\nin_macro = []\nin_attr = []\n",
                ),
            ),
            (
                "app/src/lib.rs",
                "#[cfg(feature = \"in_cfg\")]\npub fn a() {}\npub fn b() -> bool {\n    cfg!(feature=\"in_macro\")\n}\n#[cfg_attr(feature = \"in_attr\", derive(Debug))]\npub struct C;\n",
            ),
        ],
    );
    let (code, report) = run(&dir, &["--fail-on-issues"]);
    assert_eq!(
        unused_features(&report),
        BTreeSet::from(["app unused".to_owned()]),
        "{report:#}"
    );
    let finding = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .find(|finding| finding["rule"] == "unused-feature")
        .expect("a finding");
    assert_eq!(finding["severity"], "warn");
    assert_eq!(finding["section"], "features");
    assert_eq!(finding["line"], 8, "the line of the key: {finding:#}");
    assert_eq!(finding["manifest_path"], "app/Cargo.toml");
    // A warning does not fail the gate.
    assert_eq!(code, Some(0), "{report:#}");
}

#[test]
fn a_feature_that_another_feature_or_a_member_enables_is_used() {
    let dir = workspace(
        "",
        &[
            (
                "app/Cargo.toml",
                &manifest(
                    "app",
                    "[dependencies]\nalpha = { path = \"../alpha\", optional = true, features = [\"by_entry\"] }\nrenamed = { package = \"beta\", path = \"../beta\" }\n\n[features]\ndefault = [\"by_default\"]\nby_default = []\nwide = [\"inner\", \"renamed/by_slash\", \"alpha?/by_weak\", \"dep:alpha\"]\ninner = []\n",
                ),
            ),
            (
                "app/src/lib.rs",
                "#[cfg(feature = \"wide\")]\npub fn w() {}\npub use alpha::f;\npub use renamed::f as g;\n",
            ),
            (
                "alpha/Cargo.toml",
                &manifest("alpha", "[features]\nby_entry = []\nby_weak = []\n"),
            ),
            (
                "beta/Cargo.toml",
                &manifest("beta", "[features]\nby_slash = []\n"),
            ),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(unused_features(&report), BTreeSet::new(), "{report:#}");
}

#[test]
fn a_feature_with_an_effect_or_an_outside_reader_is_used() {
    let dir = workspace(
        "",
        &[
            (
                "app/Cargo.toml",
                &manifest(
                    "app",
                    "build = \"build.rs\"\n\n[dependencies]\nalpha = { path = \"../alpha\", optional = true }\n\n[features]\nwith_dep = [\"dep:alpha\"]\nbuild_env = []\nfor_target = []\nfor_docs = []\n\n[[example]]\nname = \"demo\"\nrequired-features = [\"for_target\"]\n\n[package.metadata.docs.rs]\nfeatures = [\"for_docs\"]\n",
                ),
            ),
            ("app/examples/demo.rs", "fn main() {}\n"),
            (
                "app/build.rs",
                "fn main() {\n    if std::env::var_os(\"CARGO_FEATURE_BUILD_ENV\").is_some() {}\n}\n",
            ),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(unused_features(&report), BTreeSet::new(), "{report:#}");
}

#[test]
fn a_cfg_in_another_member_gives_credit() {
    // An exported macro of `alpha` can expand `#[cfg(feature = "shared")]` in
    // the crate that calls it, so any member that names a feature gives credit.
    let dir = workspace(
        "",
        &[
            (
                "app/Cargo.toml",
                &manifest("app", "[features]\nshared = []\n"),
            ),
            (
                "alpha/src/lib.rs",
                "#[macro_export]\nmacro_rules! gated {\n    () => {\n        #[cfg(feature = \"shared\")]\n        pub fn g() {}\n    };\n}\n",
            ),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(unused_features(&report), BTreeSet::new(), "{report:#}");
}

#[test]
fn a_build_script_that_reads_each_feature_gives_credit_to_each_feature() {
    let dir = workspace(
        "",
        &[
            (
                "app/Cargo.toml",
                &manifest("app", "build = \"build.rs\"\n\n[features]\nany = []\n"),
            ),
            (
                "app/build.rs",
                "fn main() {\n    for (key, _) in std::env::vars() {\n        if key.starts_with(\"CARGO_FEATURE_\") {}\n    }\n}\n",
            ),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(unused_features(&report), BTreeSet::new(), "{report:#}");
}

#[test]
fn an_excluded_library_gets_no_finding() {
    let dir = workspace(
        "\n[workspace.metadata.fallow.visibility]\nexclude-crates = [\"alpha\"]\n",
        &[(
            "alpha/Cargo.toml",
            &manifest("alpha", "[features]\nfor_users = []\n"),
        )],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(unused_features(&report), BTreeSet::new(), "{report:#}");
}

#[test]
fn an_ignore_entry_hides_the_finding() {
    // The package metadata names each string as a use, so the test puts the
    // ignore list in the workspace metadata.
    let dir = workspace(
        "\n[workspace.metadata.fallow.deps]\nignored = [\"kept\"]\n",
        &[(
            "app/Cargo.toml",
            &manifest("app", "[features]\nkept = []\n"),
        )],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(unused_features(&report), BTreeSet::new(), "{report:#}");
}

#[test]
fn a_feature_that_a_workflow_turns_on_is_used() {
    let dir = workspace(
        "",
        &[
            (
                "app/Cargo.toml",
                &manifest("app", "[features]\nbench = []\nlocal = []\n"),
            ),
            (
                ".github/workflows/bench.yml",
                "jobs:\n  bench:\n    steps:\n      - run: cargo build --features bench\n",
            ),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        unused_features(&report),
        BTreeSet::from(["app local".to_owned()]),
        "{report:#}"
    );
}

#[test]
fn a_library_that_a_registry_can_publish_gets_no_finding() {
    let dir = workspace(
        "",
        &[(
            "alpha/Cargo.toml",
            "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\ndeprecated = []\n",
        )],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(unused_features(&report), BTreeSet::new(), "{report:#}");
}

#[test]
fn a_name_in_rust_code_a_build_script_or_another_manifest_gives_credit() {
    let dir = workspace(
        "\n[workspace.metadata.dist]\nfeatures = [\"by_dist\"]\n",
        &[
            (
                "app/Cargo.toml",
                &manifest(
                    "app",
                    "build = \"build.rs\"\n\n[features]\nby_xtask = []\nby_writer = []\nby_fuzz = []\nby_dist = []\nalone = []\n",
                ),
            ),
            (
                "app/build.rs",
                "use std::io::Write;\nfn main() {\n    let mut f = std::io::sink();\n    writeln!(f, \"#[cfg(feature = \\\"by_writer\\\")]\").ok();\n}\n",
            ),
            (
                "alpha/src/main.rs",
                "fn main() {\n    std::process::Command::new(\"cargo\").args([\"build\", \"--features\", \"by_xtask\"]);\n}\n",
            ),
            (
                "fuzz/Cargo.toml",
                "[package]\nname = \"fuzz\"\nversion = \"0.0.0\"\npublish = false\n\n[dependencies]\napp = { path = \"../app\", features = [\"by_fuzz\"] }\n\n[workspace]\n",
            ),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        unused_features(&report),
        BTreeSet::from(["app alone".to_owned()]),
        "{report:#}"
    );
}

#[test]
fn an_ignore_entry_in_the_package_table_hides_the_finding_and_stays_needed() {
    let dir = workspace(
        "",
        &[(
            "app/Cargo.toml",
            &manifest(
                "app",
                "[features]\nkept = []\n\n[package.metadata.fallow.deps]\nignored = [\"kept\"]\n",
            ),
        )],
    );
    let (_, report) = run(&dir, &[]);
    let rules: Vec<&str> = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter_map(|finding| finding["rule"].as_str())
        .collect();
    assert!(
        !rules.contains(&"unused-feature") && !rules.contains(&"redundant-ignore"),
        "{report:#}"
    );
}

#[test]
fn a_binary_that_a_registry_can_publish_gets_no_finding() {
    let dir = workspace(
        "",
        &[
            (
                "app/Cargo.toml",
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\nfor_install = []\n",
            ),
            ("app/src/main.rs", "fn main() {}\n"),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(unused_features(&report), BTreeSet::new(), "{report:#}");
}

#[test]
fn a_file_above_the_workspace_root_in_the_repository_gives_credit() {
    let outer = tempfile::tempdir().expect("create a temporary directory");
    fs::create_dir_all(outer.path().join(".git")).expect("create a folder");
    write(
        outer.path(),
        ".github/workflows/ci.yml",
        "steps:\n  - run: cargo test --features from_ci\n",
    );
    let inner = outer.path().join("rust");
    write(
        &inner,
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\"]\nresolver = \"3\"\n",
    );
    write(
        &inner,
        "app/Cargo.toml",
        &manifest("app", "[features]\nfrom_ci = []\nlonely = []\n"),
    );
    write(&inner, "app/src/lib.rs", "");
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(&inner)
        .args(["deps", "--format", "json", "--quiet", "--offline"])
        .output()
        .expect("run the binary");
    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(
        unused_features(&report),
        BTreeSet::from(["app lonely".to_owned()]),
        "{report:#}"
    );
}

#[test]
fn a_source_folder_named_target_gives_credit_and_a_cargo_target_folder_does_not() {
    let dir = workspace(
        "",
        &[
            (
                "app/Cargo.toml",
                &manifest("app", "[features]\nin_module = []\nin_build_output = []\n"),
            ),
            ("app/src/lib.rs", "mod target;\n"),
            (
                "app/src/target/mod.rs",
                "#[cfg(feature = \"in_module\")]\npub fn f() {}\n",
            ),
            (
                "target/CACHEDIR.TAG",
                "Signature: 8a477f597d28d172789f06886806bc55\n",
            ),
            (
                "target/debug/out.rs",
                "#[cfg(feature = \"in_build_output\")]\n",
            ),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        unused_features(&report),
        BTreeSet::from(["app in_build_output".to_owned()]),
        "{report:#}"
    );
}

#[test]
fn a_features_table_of_a_manifest_outside_the_workspace_gives_credit() {
    let dir = workspace(
        "",
        &[
            (
                "app/Cargo.toml",
                &manifest("app", "[features]\nforwarded = []\nalone = []\n"),
            ),
            (
                "fuzz/Cargo.toml",
                "[package]\nname = \"fuzz\"\nversion = \"0.0.0\"\npublish = false\n\n[dependencies]\napp = { path = \"../app\" }\n\n[features]\nfast = [\"app/forwarded\"]\n\n[workspace]\n",
            ),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        unused_features(&report),
        BTreeSet::from(["app alone".to_owned()]),
        "{report:#}"
    );
}

#[test]
fn a_package_for_a_named_registry_gets_no_finding() {
    let dir = workspace(
        "",
        &[(
            "app/Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = [\"internal\"]\n\n[features]\nfor_users = []\n",
        )],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(unused_features(&report), BTreeSet::new(), "{report:#}");
}

#[test]
fn the_action_of_a_finding_is_not_auto_fixable() {
    let dir = workspace(
        "",
        &[(
            "app/Cargo.toml",
            &manifest("app", "[features]\nalone = []\n"),
        )],
    );
    let (_, report) = run(&dir, &[]);
    let finding = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .find(|finding| finding["rule"] == "unused-feature")
        .expect("a finding");
    assert_eq!(finding["line"], 8, "{finding:#}");
    assert_eq!(finding["actions"][0]["type"], "remove-feature");
    assert_eq!(finding["actions"][0]["auto_fixable"], false);
}

#[cfg(unix)]
#[test]
fn a_symlink_out_of_the_repository_is_not_read() {
    let outside = tempfile::tempdir().expect("create a temporary directory");
    write(outside.path(), "notes.txt", "outside_word\n");
    let dir = workspace(
        "",
        &[(
            "app/Cargo.toml",
            &manifest("app", "[features]\noutside_word = []\n"),
        )],
    );
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).expect("create a symlink");
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        unused_features(&report),
        BTreeSet::from(["app outside_word".to_owned()]),
        "{report:#}"
    );
}

#[cfg(unix)]
#[test]
fn a_file_that_the_scan_cannot_read_stops_each_finding() {
    use std::os::unix::fs::PermissionsExt;
    let dir = workspace(
        "",
        &[
            (
                "app/Cargo.toml",
                &manifest("app", "[features]\nhidden = []\n"),
            ),
            ("secret/notes.txt", "hidden\n"),
        ],
    );
    let secret = dir.path().join("secret/notes.txt");
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).expect("set the mode");
    if fs::read(&secret).is_ok() {
        // A superuser reads each file, so the case cannot occur.
        return;
    }
    let (_, report) = run(&dir, &[]);
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).expect("set the mode");
    assert_eq!(unused_features(&report), BTreeSet::new(), "{report:#}");
}

#[cfg(unix)]
#[test]
fn a_special_file_or_a_file_symlink_out_of_the_repository_is_not_read() {
    let outside = tempfile::tempdir().expect("create a temporary directory");
    write(outside.path(), "notes.txt", "outside_word\n");
    let dir = workspace(
        "",
        &[(
            "app/Cargo.toml",
            &manifest("app", "[features]\noutside_word = []\n"),
        )],
    );
    std::os::unix::fs::symlink(
        outside.path().join("notes.txt"),
        dir.path().join("notes.txt"),
    )
    .expect("create a symlink");
    std::os::unix::fs::symlink("/dev/zero", dir.path().join("zero")).expect("create a symlink");
    let fifo = dir.path().join("pipe");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(made.success());
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        unused_features(&report),
        BTreeSet::from(["app outside_word".to_owned()]),
        "{report:#}"
    );
}
