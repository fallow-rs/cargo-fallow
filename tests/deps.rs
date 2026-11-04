//! Black-box tests of the `deps` command.
//!
//! Each test writes a small workspace to a temporary directory. Each
//! dependency is a path dependency, so `cargo metadata` needs no network. A
//! finding is `rule package section key`, for example
//! `unused-dependency app dependencies serde`.
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

/// The dependency crates that the tests can use. Each one is a workspace
/// member with an empty library, except where the test adds files.
const LIBS: &[&str] = &[
    "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa",
];

/// Writes a workspace with the member `app` and the libraries in [`LIBS`].
/// `app_deps` is the text after the `[package]` table of `app/Cargo.toml`.
fn workspace(app_deps: &str, files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let mut members = vec!["\"app\"".to_owned()];
    members.extend(LIBS.iter().map(|lib| format!("\"libs/{lib}\"")));
    write(
        dir.path(),
        "Cargo.toml",
        &format!(
            "[workspace]\nmembers = [{}]\nresolver = \"3\"\n",
            members.join(", ")
        ),
    );
    write(
        dir.path(),
        "app/Cargo.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n{app_deps}"
        ),
    );
    for lib in LIBS {
        write(
            dir.path(),
            &format!("libs/{lib}/Cargo.toml"),
            &format!(
                "[package]\nname = \"{lib}\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n"
            ),
        );
        write(
            dir.path(),
            &format!("libs/{lib}/src/lib.rs"),
            "pub fn f() {}\n#[macro_export]\nmacro_rules! m { () => {} }\n",
        );
    }
    for (path, text) in files {
        write(dir.path(), path, text);
    }
    if !dir.path().join("app/src/lib.rs").exists() && !dir.path().join("app/src/main.rs").exists() {
        write(dir.path(), "app/src/lib.rs", "");
    }
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("a file path has a parent")).expect("create a folder");
    fs::write(full, text).expect("write a file");
}

fn path_dep(name: &str) -> String {
    format!("{name} = {{ path = \"../libs/{name}\" }}\n")
}

/// Runs `deps --format json` and returns the exit code and the report.
fn run(dir: &TempDir, extra: &[&str]) -> (i32, Value) {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args(["deps", "--format", "json", "--quiet"])
        .args(extra)
        .output()
        .expect("run the binary");
    let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is JSON ({err}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code().unwrap_or(-1), report)
}

/// The findings as `rule package section key`.
fn findings(report: &Value) -> BTreeSet<String> {
    report["findings"]
        .as_array()
        .expect("findings is an array")
        .iter()
        .map(|finding| {
            format!(
                "{} {} {} {}",
                finding["rule"].as_str().unwrap_or("?"),
                finding["package"].as_str().unwrap_or("?"),
                finding["section"].as_str().unwrap_or("?"),
                finding["dependency"].as_str().unwrap_or("?"),
            )
        })
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[test]
fn unused_normal_dependency_is_reported_and_used_ones_are_not() {
    let deps = format!(
        "[dependencies]\n{}{}{}{}",
        path_dep("alpha"),
        path_dep("beta"),
        path_dep("gamma"),
        path_dep("delta")
    );
    let dir = workspace(
        &deps,
        &[(
            "app/src/lib.rs",
            "use alpha::f;\npub fn g() { f(); beta::m!(); }\npub fn h() -> ::gamma::Unknown { todo!() }\n",
        )],
    );
    let (code, report) = run(&dir, &[]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(
        findings(&report),
        set(&["unused-dependency app dependencies delta"])
    );
    let finding = &report["findings"][0];
    assert_eq!(finding["manifest_path"], "app/Cargo.toml");
    assert_eq!(finding["line"], 11);
    assert_eq!(finding["severity"], "error");
    assert_eq!(finding["actions"][0]["type"], "remove-dependency");
    assert_eq!(finding["actions"][0]["auto_fixable"], true);
    assert_eq!(report["total_issues"], 1);
}

#[test]
fn fail_on_issues_enforces_the_gate() {
    let dir = workspace(&format!("[dependencies]\n{}", path_dep("alpha")), &[]);
    let (code, report) = run(&dir, &["--fail-on-issues"]);
    assert_eq!(code, 1, "{report}");
    let gate = &report["gate_outcomes"]["error-severity-findings"];
    assert_eq!(gate["status"], "fail");
    assert_eq!(gate["enforced"], true);
    assert_eq!(gate["observed"], 1.0);
}

#[test]
fn renamed_dependency_is_found_by_its_new_name() {
    let deps = "[dependencies]\n\
                renamed = { package = \"alpha\", path = \"../libs/alpha\" }\n\
                other = { package = \"beta\", path = \"../libs/beta\" }\n";
    let dir = workspace(deps, &[("app/src/lib.rs", "pub use renamed::f;\n")]);
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        findings(&report),
        set(&["unused-dependency app dependencies other"])
    );
    assert_eq!(report["findings"][0]["package_name"], "beta");
}

#[test]
fn library_name_that_differs_from_the_package_name_is_used() {
    let dir = workspace(
        &format!("[dependencies]\n{}", path_dep("alpha")),
        &[
            (
                "libs/alpha/Cargo.toml",
                "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 publish = false\n\n[lib]\nname = \"alpha_core\"\n",
            ),
            ("app/src/lib.rs", "pub use alpha_core::f;\n"),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(findings(&report), set(&[]));
}

#[test]
fn dependency_that_only_tests_use_moves_to_dev_dependencies() {
    let deps = format!(
        "[dependencies]\n{}{}{}{}",
        path_dep("alpha"),
        path_dep("beta"),
        path_dep("gamma"),
        path_dep("delta")
    );
    let dir = workspace(
        &deps,
        &[
            (
                "app/src/lib.rs",
                "pub fn g() { delta::f(); }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() { alpha::f(); }\n}\n",
            ),
            ("app/tests/it.rs", "#[test]\nfn t() { beta::f(); }\n"),
            ("app/examples/demo.rs", "fn main() { gamma::f(); }\n"),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        findings(&report),
        set(&[
            "misplaced-dependency app dependencies alpha",
            "misplaced-dependency app dependencies beta",
            "misplaced-dependency app dependencies gamma",
        ])
    );
    let beta = report["findings"]
        .as_array()
        .and_then(|all| all.iter().find(|f| f["dependency"] == "beta"))
        .expect("the beta finding");
    assert_eq!(beta["used_by"], serde_json::json!(["test"]));
    assert_eq!(beta["actions"][0]["type"], "move-to-dev-dependencies");
}

#[test]
fn misplaced_dependency_that_dev_already_has_is_removed_not_moved() {
    let deps = format!(
        "[dependencies]\n{}\n[dev-dependencies]\n{}",
        path_dep("alpha"),
        path_dep("alpha")
    );
    let dir = workspace(
        &deps,
        &[("app/tests/it.rs", "#[test]\nfn t() { alpha::f(); }\n")],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        findings(&report),
        set(&["misplaced-dependency app dependencies alpha"])
    );
    let action = &report["findings"][0]["actions"][0];
    assert_eq!(action["type"], "remove-dependency");
    assert_eq!(
        action["description"],
        "Remove `alpha` from [dependencies]. [dev-dependencies] has it already."
    );
}

#[test]
fn test_module_in_its_own_file_counts_as_test_code() {
    let deps = format!("[dependencies]\n{}", path_dep("alpha"));
    let dir = workspace(
        &deps,
        &[
            ("app/src/lib.rs", "#[cfg(test)]\nmod tests;\n"),
            ("app/src/tests.rs", "#[test]\nfn t() { alpha::f(); }\n"),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        findings(&report),
        set(&["misplaced-dependency app dependencies alpha"])
    );
}

#[test]
fn dev_and_build_dependencies_have_their_own_results() {
    let deps = format!(
        "[dev-dependencies]\n{}{}\n[build-dependencies]\n{}{}",
        path_dep("alpha"),
        path_dep("beta"),
        path_dep("gamma"),
        path_dep("delta")
    );
    let dir = workspace(
        &deps,
        &[
            ("app/tests/it.rs", "#[test]\nfn t() { alpha::f(); }\n"),
            ("app/build.rs", "fn main() { gamma::f(); }\n"),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        findings(&report),
        set(&[
            "unused-build-dependency app build-dependencies delta",
            "unused-dev-dependency app dev-dependencies beta",
        ])
    );
}

#[test]
fn build_script_use_does_not_count_for_a_normal_dependency() {
    let deps = format!(
        "[dependencies]\n{}\n[build-dependencies]\n{}",
        path_dep("alpha"),
        path_dep("alpha")
    );
    let dir = workspace(&deps, &[("app/build.rs", "fn main() { alpha::f(); }\n")]);
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        findings(&report),
        set(&["unused-dependency app dependencies alpha"])
    );
}

#[test]
fn names_in_macro_tokens_attributes_and_doctests_count() {
    let deps = format!(
        "[dependencies]\n{}{}{}{}{}",
        path_dep("alpha"),
        path_dep("beta"),
        path_dep("gamma"),
        path_dep("delta"),
        path_dep("epsilon"),
    );
    let dir = workspace(
        &deps,
        &[(
            "app/src/lib.rs",
            "macro_rules! call { () => { ::alpha::f() }; }\n\
             pub fn g() { call!(); println!(\"{:?}\", beta::f()); }\n\
             #[gamma::attr]\npub fn h() {}\n\
             #[derive(Debug)]\n#[serde(with = \"delta\")]\npub struct S;\n\
             /// ```\n/// epsilon::f();\n/// ```\npub fn k() {}\n",
        )],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        findings(&report),
        set(&["misplaced-dependency app dependencies epsilon"])
    );
}

#[test]
fn derive_crate_gives_credit_to_its_runtime_crate() {
    let deps = format!(
        "[dev-dependencies]\nalpha_derive = {{ path = \"../libs/alpha_derive\" }}\n{}{}",
        path_dep("alpha"),
        path_dep("beta")
    );
    let dir = workspace(
        &deps,
        &[
            (
                "app/tests/it.rs",
                "use alpha_derive::Thing;\n#[derive(Thing)]\nstruct S;\n",
            ),
            (
                "libs/alpha_derive/Cargo.toml",
                "[package]\nname = \"alpha_derive\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 publish = false\n",
            ),
            ("libs/alpha_derive/src/lib.rs", "pub fn f() {}\n"),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        findings(&report),
        set(&["unused-dev-dependency app dev-dependencies beta"])
    );
}

#[test]
fn extern_crate_and_global_allocator_count() {
    let deps = format!("[dependencies]\n{}{}", path_dep("alpha"), path_dep("beta"));
    let dir = workspace(
        &deps,
        &[(
            "app/src/lib.rs",
            "extern crate alpha as _;\n#[global_allocator]\nstatic A: beta::Alloc = beta::Alloc;\n",
        )],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(findings(&report), set(&[]));
}

#[test]
fn compile_test_harness_gives_credit_to_each_dev_dependency() {
    let deps = format!(
        "[dev-dependencies]\ntrybuild = {{ path = \"../libs/trybuild\" }}\n{}",
        path_dep("alpha")
    );
    let dir = workspace(
        &deps,
        &[
            ("app/tests/ui.rs", "#[test]\nfn ui() { trybuild::f(); }\n"),
            ("app/tests/ui/pass.rs", "fn main() { alpha::f(); }\n"),
            (
                "libs/trybuild/Cargo.toml",
                "[package]\nname = \"trybuild\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 publish = false\n",
            ),
            ("libs/trybuild/src/lib.rs", "pub fn f() {}\n"),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(findings(&report), set(&[]));
}

#[test]
fn platform_dependency_used_in_cfg_code_counts() {
    let deps = format!(
        "[target.'cfg(windows)'.dependencies]\n{}{}",
        path_dep("alpha"),
        path_dep("beta")
    );
    let dir = workspace(
        &deps,
        &[(
            "app/src/lib.rs",
            "#[cfg(windows)]\npub fn g() { alpha::f(); }\n",
        )],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        findings(&report),
        set(&["unused-dependency app dependencies beta"])
    );
    assert_eq!(report["findings"][0]["platform"], "cfg(windows)");
    assert_eq!(report["findings"][0]["line"], 9);
}

#[test]
fn optional_and_feature_only_dependencies_are_warnings() {
    let deps = format!(
        "[features]\nextra = [\"dep:alpha\"]\nunify = [\"beta/std\"]\n\n[dependencies]\n\
         alpha = {{ path = \"../libs/alpha\", optional = true }}\n{}",
        path_dep("beta")
    );
    let dir = workspace(
        &deps,
        &[(
            "libs/beta/Cargo.toml",
            "[package]\nname = \"beta\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
             publish = false\n\n[features]\nstd = []\n",
        )],
    );
    let (code, report) = run(&dir, &["--fail-on-issues"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(
        findings(&report),
        set(&[
            "feature-only-dependency app dependencies beta",
            "unused-optional-dependency app dependencies alpha",
        ])
    );
    assert!(
        report["findings"]
            .as_array()
            .expect("an array")
            .iter()
            .all(|finding| finding["severity"] == "warn")
    );
    let sarif = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args(["deps", "--quiet", "--format", "sarif"])
        .output()
        .expect("run the binary");
    let log: Value = serde_json::from_slice(&sarif.stdout).expect("SARIF is JSON");
    let results = log["runs"][0]["results"].as_array().expect("the results");
    assert_eq!(results.len(), 2, "{log:#}");
    assert!(
        results.iter().all(|result| result["level"] == "warning"),
        "{log:#}"
    );
}

#[test]
fn dependency_with_a_links_key_is_not_reported() {
    let deps = format!("[dependencies]\n{}", path_dep("alpha"));
    let dir = workspace(
        &deps,
        &[
            (
                "libs/alpha/Cargo.toml",
                "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 publish = false\nlinks = \"alpha\"\n",
            ),
            ("libs/alpha/build.rs", "fn main() {}\n"),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(findings(&report), set(&[]));
}

#[test]
fn include_macro_gives_credit_to_each_dependency() {
    let deps = format!("[dependencies]\n{}", path_dep("alpha"));
    let dir = workspace(
        &deps,
        &[(
            "app/src/lib.rs",
            "include!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));\n",
        )],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(findings(&report), set(&[]));
}

#[test]
fn generated_include_gives_credit_to_the_names_of_the_build_script() {
    let deps = format!(
        "[dependencies]\n{}{}{}\n[build-dependencies]\n\
         gamma-build = {{ path = \"../libs/gamma-build\" }}\n",
        path_dep("alpha"),
        path_dep("beta"),
        path_dep("gamma"),
    );
    let dir = workspace(
        &deps,
        &[
            (
                "app/src/lib.rs",
                "include!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));\n",
            ),
            (
                "app/build.rs",
                "fn main() {\n    gamma_build::f();\n    let out = std::env::var(\"OUT_DIR\").unwrap();\n    \
                 std::fs::write(format!(\"{out}/gen.rs\"), \"pub fn g() { alpha::f(); }\").unwrap();\n}\n",
            ),
            (
                "libs/gamma-build/Cargo.toml",
                "[package]\nname = \"gamma-build\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 publish = false\n",
            ),
            ("libs/gamma-build/src/lib.rs", "pub fn f() {}\n"),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        findings(&report),
        set(&["unused-dependency app dependencies beta"])
    );
}

#[test]
fn ignore_lists_hide_findings_and_a_stale_entry_is_reported() {
    let deps = format!(
        "[package.metadata.fallow.deps]\nignored = [\"alpha\", \"beta\"]\n\n\
         [package.metadata.cargo-shear]\nignored = [\"gamma\"]\n\n[dependencies]\n{}{}{}",
        path_dep("alpha"),
        path_dep("beta"),
        path_dep("gamma")
    );
    let dir = workspace(&deps, &[("app/src/lib.rs", "pub use beta::f;\n")]);
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        findings(&report),
        set(&["redundant-ignore app package.metadata.fallow.deps beta"])
    );
}

#[test]
fn workspace_dependencies_inherit_and_unused_entries_are_reported() {
    let dir = workspace(
        "[dependencies]\nalpha = { workspace = true }\nbeta.workspace = true\n",
        &[("app/src/lib.rs", "pub use alpha::f;\n")],
    );
    let root = dir.path().join("Cargo.toml");
    let mut text = fs::read_to_string(&root).expect("read the root manifest");
    text.push_str(
        "\n[workspace.dependencies]\nalpha = { path = \"libs/alpha\" }\n\
         beta = { path = \"libs/beta\" }\ngamma = { path = \"libs/gamma\" }\n",
    );
    fs::write(&root, text).expect("write the root manifest");
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        findings(&report),
        set(&[
            "unused-dependency app dependencies beta",
            "unused-workspace-dependency ? workspace.dependencies beta",
            "unused-workspace-dependency ? workspace.dependencies gamma",
        ])
    );
    let beta = report["findings"]
        .as_array()
        .and_then(|all| {
            all.iter()
                .find(|f| f["dependency"] == "beta" && f["package"] == "app")
        })
        .expect("the beta finding");
    assert_eq!(beta["line"], 9);
}

#[test]
fn workspace_entry_that_only_unused_members_inherit_is_unused_too() {
    let dir = workspace(
        "[dependencies]\nalpha = { workspace = true }\n\n[dev-dependencies]\nbeta.workspace = true\n",
        &[("app/tests/it.rs", "#[test]\nfn t() { beta::f(); }\n")],
    );
    let root = dir.path().join("Cargo.toml");
    let mut text = fs::read_to_string(&root).expect("read the root manifest");
    text.push_str(
        "\n[workspace.dependencies]\nalpha = { path = \"libs/alpha\" }\n\
         beta = { path = \"libs/beta\" }\n",
    );
    fs::write(&root, text).expect("write the root manifest");
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        findings(&report),
        set(&[
            "unused-dependency app dependencies alpha",
            "unused-workspace-dependency ? workspace.dependencies alpha",
        ])
    );
}

#[test]
fn workspace_entry_that_members_declare_without_inheritance_is_unused() {
    let dir = workspace(
        &format!(
            "[dependencies]\n{}[dev-dependencies.beta]\nworkspace = true\n",
            path_dep("alpha")
        ),
        &[("app/src/lib.rs", "pub use alpha::f;\n")],
    );
    let root = dir.path().join("Cargo.toml");
    let mut text = fs::read_to_string(&root).expect("read the root manifest");
    text.push_str(
        "\n[workspace.dependencies]\nalpha = { path = \"libs/alpha\" }\n\
         beta = { path = \"libs/beta\" }\n",
    );
    fs::write(&root, text).expect("write the root manifest");
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        findings(&report),
        set(&[
            "unused-dev-dependency app dev-dependencies beta",
            "unused-workspace-dependency ? workspace.dependencies alpha",
            "unused-workspace-dependency ? workspace.dependencies beta",
        ])
    );
}

#[test]
fn derive_inside_cfg_attr_counts() {
    let deps = "[dependencies]\nalpha = { path = \"../libs/alpha\", optional = true }\n";
    let dir = workspace(
        deps,
        &[(
            "app/src/lib.rs",
            "#[cfg_attr(feature = \"alpha\", derive(alpha::Thing))]\npub struct S;\n",
        )],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(findings(&report), set(&[]));
}

#[test]
fn optional_dependency_with_a_links_key_is_a_warning() {
    let deps = "[dependencies]\nalpha = { path = \"../libs/alpha\", optional = true }\n";
    let dir = workspace(
        deps,
        &[
            (
                "libs/alpha/Cargo.toml",
                "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 publish = false\nlinks = \"alpha\"\n",
            ),
            ("libs/alpha/build.rs", "fn main() {}\n"),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        findings(&report),
        set(&["unused-optional-dependency app dependencies alpha"])
    );
}

#[test]
fn file_that_does_not_parse_skips_its_package() {
    let deps = format!("[dependencies]\n{}", path_dep("alpha"));
    let dir = workspace(&deps, &[("app/src/lib.rs", &"(".repeat(20_000))]);
    let root = dir.path().join("Cargo.toml");
    let mut text = fs::read_to_string(&root).expect("read the root manifest");
    text.push_str("\n[workspace.metadata.fallow.deps]\nignored = [\"alpha\"]\n");
    fs::write(&root, text).expect("write the root manifest");
    let (_, report) = run(&dir, &[]);
    assert_eq!(findings(&report), set(&[]));
    assert_eq!(report["skipped"][0]["package"], "app");
}

#[test]
fn human_and_sarif_forms_name_the_finding() {
    let dir = workspace(&format!("[dependencies]\n{}", path_dep("alpha")), &[]);
    let human = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args(["deps", "--quiet"])
        .output()
        .expect("run the binary");
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("app/Cargo.toml\n    :8 alpha"), "{text}");
    let sarif = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args(["deps", "--quiet", "--format", "sarif"])
        .output()
        .expect("run the binary");
    let log: Value = serde_json::from_slice(&sarif.stdout).expect("SARIF is JSON");
    let result = &log["runs"][0]["results"][0];
    assert_eq!(result["ruleId"], "unused-dependency");
    // An error finding is a SARIF error, also when the gate is not enforced.
    assert_eq!(result["level"], "error");
    assert_eq!(
        result["locations"][0]["physicalLocation"]["region"]["startLine"],
        8
    );
}

#[test]
fn json_output_is_stable_between_runs() {
    let deps = format!(
        "[dependencies]\n{}{}{}",
        path_dep("gamma"),
        path_dep("alpha"),
        path_dep("beta")
    );
    let dir = workspace(&deps, &[]);
    let (_, first) = run(&dir, &[]);
    let (_, second) = run(&dir, &[]);
    assert_eq!(first, second);
    let keys: Vec<&str> = first["findings"]
        .as_array()
        .expect("an array")
        .iter()
        .filter_map(|finding| finding["dependency"].as_str())
        .collect();
    assert_eq!(keys, ["gamma", "alpha", "beta"]);
}

#[test]
fn local_include_gives_credit_to_the_names_of_the_included_file() {
    let deps = format!(
        "[dependencies]\n{}{}{}",
        path_dep("alpha"),
        path_dep("beta"),
        path_dep("gamma")
    );
    let dir = workspace(
        &deps,
        &[
            ("app/build.rs", "fn main() {}\n"),
            ("app/src/lib.rs", "include!(\"inc.rs\");\n"),
            (
                "app/src/inc.rs",
                "pub fn t() { alpha::f(); }\ninclude!(\"nested/more.rs\");\n",
            ),
            (
                "app/src/nested/more.rs",
                "use gamma::f;\npub fn u() { f(); }\n",
            ),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        findings(&report),
        set(&["unused-dependency app dependencies beta"])
    );
}

#[test]
fn local_include_without_a_build_script_gives_credit_only_to_its_names() {
    let deps = format!("[dependencies]\n{}{}", path_dep("alpha"), path_dep("beta"));
    let dir = workspace(
        &deps,
        &[
            ("app/src/lib.rs", "include!(\"inc.rs\");\n"),
            ("app/src/inc.rs", "pub fn t() { alpha::f(); }\n"),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        findings(&report),
        set(&["unused-dependency app dependencies beta"])
    );
}

#[test]
fn local_include_of_a_missing_file_gives_credit_to_each_dependency() {
    let deps = format!("[dependencies]\n{}", path_dep("alpha"));
    let dir = workspace(&deps, &[("app/src/lib.rs", "include!(\"missing.rs\");\n")]);
    let (_, report) = run(&dir, &[]);
    assert_eq!(findings(&report), set(&[]));
}

/// A lock file that names no package. Cargo rewrites it when it resolves.
const STALE_LOCK: &str = "# stale\nversion = 4\n";

#[test]
fn run_does_not_create_a_lock_file() {
    let dir = workspace(&format!("[dependencies]\n{}", path_dep("alpha")), &[]);
    let (code, _) = run(&dir, &[]);
    assert_eq!(code, 0);
    assert!(!dir.path().join("Cargo.lock").exists());
}

#[test]
fn run_does_not_change_an_existing_lock_file() {
    let dir = workspace(&format!("[dependencies]\n{}", path_dep("alpha")), &[]);
    write(dir.path(), "Cargo.lock", STALE_LOCK);
    let (code, _) = run(&dir, &[]);
    assert_eq!(code, 0);
    let lock = fs::read_to_string(dir.path().join("Cargo.lock")).expect("read the lock file");
    assert_eq!(lock, STALE_LOCK);
}

#[test]
fn locked_and_frozen_flags_reach_cargo() {
    for flag in ["--locked", "--frozen"] {
        let dir = workspace(&format!("[dependencies]\n{}", path_dep("alpha")), &[]);
        write(dir.path(), "Cargo.lock", STALE_LOCK);
        let (code, report) = run(&dir, &[flag]);
        assert_eq!(code, 2, "{flag}: {report}");
        assert!(
            report["message"]
                .as_str()
                .is_some_and(|message| message.contains("cargo metadata")),
            "{flag}: {report}"
        );
        let lock = fs::read_to_string(dir.path().join("Cargo.lock")).expect("read the lock file");
        assert_eq!(lock, STALE_LOCK, "{flag}");
    }
}

#[test]
fn offline_flag_reaches_cargo() {
    let dir = workspace(&format!("[dependencies]\n{}", path_dep("alpha")), &[]);
    let (code, report) = run(&dir, &["--offline"]);
    assert_eq!(code, 0, "{report}");
}

#[test]
fn include_cycle_with_many_calls_finishes() {
    let deps = format!("[dependencies]\n{}{}", path_dep("alpha"), path_dep("beta"));
    let calls = "include!(\"a.rs\");\n".repeat(20);
    let dir = workspace(
        &deps,
        &[
            ("app/src/lib.rs", "include!(\"a.rs\");\n"),
            (
                "app/src/a.rs",
                &format!("{calls}pub fn t() {{ alpha::f(); }}\n"),
            ),
        ],
    );
    let (code, report) = run(&dir, &[]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(findings(&report), set(&[]));
}
