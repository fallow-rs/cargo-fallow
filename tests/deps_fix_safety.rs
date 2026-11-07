//! Black-box tests of the unused dependency entries that `fix` leaves for a
//! person: an entry that the code of an external macro can use, and an entry
//! that pins the version of a crate for other packages.
//!
//! Each test writes a small workspace to a temporary directory. Each
//! dependency is a path dependency, so `cargo metadata` needs no network. The
//! crates under `ext/` are outside the workspace.
#![expect(
    clippy::expect_used,
    reason = "a test fails loudly when its setup does not work"
)]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

/// The workspace members that the `app` member can depend on.
const LIBS: &[&str] = &["alpha", "beta", "gamma", "delta"];

/// The crates outside the workspace.
const EXTERNAL: &[&str] = &["mac", "other", "ctor", "rust-i18n"];

/// The text of `fix` for an entry that the code of an external macro can use.
const EXTERNAL_MACRO: &str = "the code of that macro can use the entry";

/// The text of `fix` for an entry that pins a version.
const PIN: &str = "can pin the version of the crate for other dependencies";

/// Writes a workspace with the member `app`, the members in [`LIBS`] and the
/// crates in [`EXTERNAL`] outside the workspace. `app_manifest` is the text
/// after the `[package]` table of `app/Cargo.toml`.
fn workspace(app_manifest: &str, files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    let mut members = vec!["\"app\"".to_owned()];
    members.extend(LIBS.iter().map(|lib| format!("\"libs/{lib}\"")));
    write(
        dir.path(),
        "Cargo.toml",
        &format!(
            "[workspace]\nmembers = [{}]\nexclude = [\"ext\"]\nresolver = \"3\"\n",
            members.join(", ")
        ),
    );
    write(
        dir.path(),
        "app/Cargo.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n{app_manifest}"
        ),
    );
    let crates = LIBS
        .iter()
        .map(|lib| format!("libs/{lib}"))
        .chain(EXTERNAL.iter().map(|name| format!("ext/{name}")));
    for folder in crates {
        let name = folder.rsplit('/').next().expect("a folder has a name");
        write(
            dir.path(),
            &format!("{folder}/Cargo.toml"),
            &format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n"
            ),
        );
        write(
            dir.path(),
            &format!("{folder}/src/lib.rs"),
            "pub fn f() {}\n#[macro_export]\nmacro_rules! m { () => {} }\n",
        );
    }
    for (path, text) in files {
        write(dir.path(), path, text);
    }
    if !dir.path().join("app/src/lib.rs").exists() {
        write(dir.path(), "app/src/lib.rs", "");
    }
    dir
}

fn write(root: &Path, path: &str, text: &str) {
    let full = root.join(path);
    fs::create_dir_all(full.parent().expect("a file path has a parent")).expect("create a folder");
    fs::write(full, text).expect("write a file");
}

fn dep(name: &str, folder: &str) -> String {
    format!("{name} = {{ path = \"../{folder}/{name}\" }}\n")
}

/// Runs `cargo fallow` with the arguments and returns stdout as JSON.
fn run(dir: &TempDir, args: &[&str]) -> Value {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args(args)
        .output()
        .expect("run the binary");
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is JSON ({err}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// The first action of each finding of `deps`, keyed by `section key`.
fn actions(dir: &TempDir) -> BTreeMap<String, Value> {
    keyed_actions(dir, &["section", "dependency"])
}

/// The first action of each finding of `deps`, keyed by the values of
/// `fields`, separated by a space.
fn keyed_actions(dir: &TempDir, fields: &[&str]) -> BTreeMap<String, Value> {
    let report = run(dir, &["deps", "--format", "json", "--quiet"]);
    report["findings"]
        .as_array()
        .expect("findings is an array")
        .iter()
        .map(|finding| {
            let key: Vec<&str> = fields
                .iter()
                .map(|field| finding[*field].as_str().unwrap_or("?"))
                .collect();
            (key.join(" "), finding["actions"][0].clone())
        })
        .collect()
}

/// Asserts that the action of `key` has `auto_fixable` set to `fixable`, and
/// that its description has `text` when the entry stays for a person.
fn assert_action(actions: &BTreeMap<String, Value>, key: &str, fixable: bool, text: &str) {
    let action = actions
        .get(key)
        .unwrap_or_else(|| panic!("no finding for {key}: {actions:?}"));
    assert_eq!(action["auto_fixable"], fixable, "{key}: {action}");
    let description = action["description"].as_str().unwrap_or_default();
    assert_eq!(description.contains(text), !fixable, "{key}: {description}");
}

#[test]
fn external_attribute_macro_in_a_bench_keeps_unused_dev_entries() {
    let manifest = format!(
        "[dependencies]\n{}\n[dev-dependencies]\n{}{}",
        dep("beta", "libs"),
        dep("mac", "ext"),
        dep("alpha", "libs"),
    );
    let dir = workspace(
        &manifest,
        &[(
            "app/benches/b.rs",
            "#[mac::bench]\nfn work() {}\nfn main() { work(); }\n",
        )],
    );
    let actions = actions(&dir);
    assert_eq!(actions.len(), 2, "{actions:?}");
    assert_action(&actions, "dev-dependencies alpha", false, EXTERNAL_MACRO);
    // The bench is not code of the `[dependencies]` table.
    assert_action(&actions, "dependencies beta", true, EXTERNAL_MACRO);
}

#[test]
fn external_bang_macro_by_path_keeps_unused_entries_of_its_section() {
    let manifest = format!(
        "[dependencies]\n{}{}\n[dev-dependencies]\n{}",
        dep("mac", "ext"),
        dep("alpha", "libs"),
        dep("beta", "libs"),
    );
    let dir = workspace(&manifest, &[("app/src/lib.rs", "mac::m!();\n")]);
    let actions = actions(&dir);
    assert_eq!(actions.len(), 2, "{actions:?}");
    assert_action(&actions, "dependencies alpha", false, EXTERNAL_MACRO);
    // No test, bench or example calls an external macro.
    assert_action(&actions, "dev-dependencies beta", true, EXTERNAL_MACRO);
}

#[test]
fn expression_macros_std_macros_and_workspace_macros_keep_entries_fixable() {
    let manifest = format!(
        "[dependencies]\n{}{}{}",
        dep("mac", "ext"),
        dep("gamma", "libs"),
        dep("alpha", "libs"),
    );
    let dir = workspace(
        &manifest,
        &[(
            "app/src/lib.rs",
            "#[derive(Debug, mac::Thing)]\npub struct S;\n#[must_use]\npub fn g() -> u8 {\n\
             std::println!(\"x\");\n    core::assert!(true);\n    gamma::m!();\n    mac::m!();\n    mac::f();\n    0\n}\n\
             gamma::m!();\n",
        )],
    );
    let actions = actions(&dir);
    assert_eq!(actions.len(), 1, "{actions:?}");
    assert_action(&actions, "dependencies alpha", true, EXTERNAL_MACRO);
}

#[test]
fn external_macro_in_test_only_code_keeps_only_dev_entries() {
    let manifest = format!(
        "[features]\nx = []\n\n[dependencies]\n{}{}\n[dev-dependencies]\n{}",
        dep("mac", "ext"),
        dep("alpha", "libs"),
        dep("beta", "libs"),
    );
    let dir = workspace(
        &manifest,
        &[(
            "app/src/lib.rs",
            "pub fn g() { mac::f(); }\n#[cfg(all(test, feature = \"x\"))]\nmod tests {\n\
             #[mac::check]\n    fn t() {}\n}\n",
        )],
    );
    let actions = actions(&dir);
    assert_eq!(actions.len(), 2, "{actions:?}");
    // Only a test build compiles the module, and `[dependencies]` does not
    // serve a test build alone.
    assert_action(&actions, "dependencies alpha", true, EXTERNAL_MACRO);
    assert_action(&actions, "dev-dependencies beta", false, EXTERNAL_MACRO);
}

#[test]
fn external_attribute_macro_in_cfg_attr_keeps_unused_entries() {
    let manifest = format!(
        "[features]\nx = []\n\n[dependencies]\n{}{}",
        dep("mac", "ext"),
        dep("alpha", "libs"),
    );
    let dir = workspace(
        &manifest,
        &[(
            "app/src/lib.rs",
            "#[cfg_attr(feature = \"x\", mac::attr)]\npub fn g() {}\n",
        )],
    );
    let actions = actions(&dir);
    assert_eq!(actions.len(), 1, "{actions:?}");
    assert_action(&actions, "dependencies alpha", false, EXTERNAL_MACRO);
}

#[test]
fn macros_of_a_self_contained_macro_crate_keep_entries_fixable() {
    let manifest = format!(
        "[dependencies]\n{}{}\n[dev-dependencies]\n{}{}",
        dep("rust-i18n", "ext"),
        dep("beta", "libs"),
        dep("ctor", "ext"),
        dep("alpha", "libs"),
    );
    let dir = workspace(
        &manifest,
        &[
            ("app/src/lib.rs", "rust_i18n::i18n!(\"locales\");\n"),
            ("app/tests/it.rs", "#[ctor::ctor]\nfn init() {}\n"),
        ],
    );
    let actions = actions(&dir);
    assert_eq!(actions.len(), 2, "{actions:?}");
    assert_action(&actions, "dependencies beta", true, EXTERNAL_MACRO);
    assert_action(&actions, "dev-dependencies alpha", true, EXTERNAL_MACRO);
}

/// Writes a workspace where `app` depends on `mac` and `alpha`, with
/// `app/src/lib.rs` set to `lib`, and returns the actions of `deps`.
fn single_name_actions(lib: &str) -> BTreeMap<String, Value> {
    let manifest = format!(
        "[dependencies]\n{}{}{}",
        dep("mac", "ext"),
        dep("gamma", "libs"),
        dep("alpha", "libs")
    );
    let dir = workspace(&manifest, &[("app/src/lib.rs", lib)]);
    actions(&dir)
}

#[test]
fn single_name_attribute_from_a_glob_import_keeps_unused_entries() {
    // `#[wasm_bindgen] pub async fn` writes `wasm_bindgen_futures::` paths.
    let actions = single_name_actions("use mac::prelude::*;\n#[attr]\npub async fn g() {}\n");
    assert_action(&actions, "dependencies alpha", false, EXTERNAL_MACRO);
}

#[test]
fn single_name_attribute_from_a_named_import_keeps_unused_entries() {
    let named = single_name_actions("use mac::prelude::attr;\n#[attr]\npub fn g() {}\n");
    assert_action(&named, "dependencies alpha", false, EXTERNAL_MACRO);
    let renamed =
        single_name_actions("use mac::prelude::attr as bind;\n#[bind(x)]\npub fn g() {}\n");
    assert_action(&renamed, "dependencies alpha", false, EXTERNAL_MACRO);
}

#[test]
fn single_name_item_macro_from_an_import_keeps_unused_entries() {
    let named = single_name_actions("use mac::m;\nm!();\n");
    assert_action(&named, "dependencies alpha", false, EXTERNAL_MACRO);
    let glob = single_name_actions("use mac::*;\npub struct S;\nimpl S {\n    m!();\n}\n");
    assert_action(&glob, "dependencies alpha", false, EXTERNAL_MACRO);
}

#[test]
fn single_name_attribute_through_a_local_import_keeps_unused_entries() {
    let parent = single_name_actions(
        "use mac::*;\nmod inner {\n    use super::*;\n    #[attr]\n    pub fn g() {}\n}\n",
    );
    assert_action(&parent, "dependencies alpha", false, EXTERNAL_MACRO);
    let reexport = single_name_actions(
        "pub use mac::attr;\nmod inner {\n    use crate::attr;\n    #[attr]\n    pub fn g() {}\n}\n",
    );
    assert_action(&reexport, "dependencies alpha", false, EXTERNAL_MACRO);
}

#[test]
fn single_name_attribute_in_a_glob_cycle_keeps_unused_entries() {
    // The test module comes first, so its walk meets the cycle of `a` and the
    // root before the walk of the library code in `a`.
    let actions = single_name_actions(
        "use mac::*;\nuse a::*;\n#[cfg(test)]\nmod t {\n    use super::*;\n    #[attr]\n    fn x() {}\n}\n\
         mod a {\n    use super::*;\n    #[attr]\n    pub fn g() {}\n}\n",
    );
    assert_action(&actions, "dependencies alpha", false, EXTERNAL_MACRO);
}

#[test]
fn single_name_macros_that_no_external_import_binds_keep_entries_fixable() {
    let cases = [
        // A builtin attribute, a field attribute and an expression macro.
        "use mac::*;\n#[inline]\npub fn g() { m!(); }\npub struct S {\n    #[helper]\n    pub x: u8,\n}\n",
        // A glob import of a workspace crate.
        "use gamma::*;\n#[attr]\npub fn g() {}\nm!();\n",
        // A local macro, and an import that binds another name.
        "use mac::f;\nmacro_rules! local { () => {} }\nlocal!();\n#[attr]\npub fn g() { f(); }\n",
    ];
    for lib in cases {
        let actions = single_name_actions(lib);
        assert_action(&actions, "dependencies alpha", true, EXTERNAL_MACRO);
    }
}

#[test]
fn exact_or_bounded_version_of_a_shared_crate_stays_for_a_person() {
    // `beta` also depends on `alpha`, `gamma`, `delta` and `other`.
    let manifest = "[dependencies]\n\
         alpha = { path = \"../libs/alpha\", version = \"=0.1.0\" }\n\
         gamma = { path = \"../libs/gamma\", version = \"0.1.0\" }\n\
         delta = { path = \"../libs/delta\", version = \">=0.1, <0.1.5\" }\n\
         other = { path = \"../ext/other\", version = \"<=0.1.0\" }\n\
         mac = { path = \"../ext/mac\", version = \"=0.1.0\" }\n";
    let dir = workspace(
        manifest,
        &[
            ("app/src/lib.rs", "pub fn g() { beta::f(); }\n"),
            (
                "libs/beta/Cargo.toml",
                "[package]\nname = \"beta\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
                 [dependencies]\nalpha = { path = \"../alpha\" }\ngamma = { path = \"../gamma\" }\n\
                 delta = { path = \"../delta\" }\nother = { path = \"../../ext/other\" }\n",
            ),
            (
                "libs/beta/src/lib.rs",
                "pub fn f() { alpha::f(); gamma::f(); delta::f(); other::f(); }\n",
            ),
        ],
    );
    let manifest = format!("{manifest}beta = {{ path = \"../libs/beta\" }}\n");
    write(
        dir.path(),
        "app/Cargo.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n{manifest}"
        ),
    );
    let actions = actions(&dir);
    assert_eq!(actions.len(), 5, "{actions:?}");
    assert_action(&actions, "dependencies alpha", false, PIN);
    assert_action(&actions, "dependencies delta", false, PIN);
    assert_action(&actions, "dependencies other", false, PIN);
    // A caret requirement does not hold the version of `beta`.
    assert_action(&actions, "dependencies gamma", true, PIN);
    // No other package depends on `mac`, so the pin holds nothing else.
    assert_action(&actions, "dependencies mac", true, PIN);
}

#[test]
fn pin_that_another_entry_repeats_stays_fixable() {
    let manifest = format!(
        "[dependencies]\nalpha = {{ path = \"../libs/alpha\", version = \"=0.1.0\" }}\n{}",
        dep("beta", "libs"),
    );
    let dir = workspace(
        &manifest,
        &[
            ("app/src/lib.rs", "pub fn g() { beta::f(); }\n"),
            (
                "libs/beta/Cargo.toml",
                "[package]\nname = \"beta\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
                 [dependencies]\nalpha = { path = \"../alpha\", version = \"=0.1.0\" }\n",
            ),
            ("libs/beta/src/lib.rs", "pub fn f() { alpha::f(); }\n"),
        ],
    );
    let actions = actions(&dir);
    assert_eq!(actions.len(), 1, "{actions:?}");
    // `beta` keeps the same pin, so the removal does not change the resolve.
    assert_action(&actions, "dependencies alpha", true, PIN);
}

#[test]
fn two_unused_entries_with_the_same_pin_stay_for_a_person() {
    let pin = "alpha = { path = \"../libs/alpha\", version = \"=0.1.0\" }\n";
    let manifest = format!(
        "[dependencies]\n{pin}{}{}",
        dep("beta", "libs"),
        dep("gamma", "libs"),
    );
    let dir = workspace(
        &manifest,
        &[
            ("app/src/lib.rs", "pub fn g() { beta::f(); gamma::f(); }\n"),
            (
                "libs/beta/Cargo.toml",
                "[package]\nname = \"beta\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
                 [dependencies]\nalpha = { path = \"../alpha\" }\n",
            ),
            ("libs/beta/src/lib.rs", "pub fn f() { alpha::f(); }\n"),
            (
                "libs/gamma/Cargo.toml",
                "[package]\nname = \"gamma\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
                 [dependencies]\nalpha = { path = \"../alpha\", version = \"=0.1.0\" }\n",
            ),
        ],
    );
    let actions = keyed_actions(&dir, &["package", "dependency"]);
    assert_eq!(actions.len(), 2, "{actions:?}");
    // `fix` removes both entries, so neither entry keeps the pin for `beta`.
    assert_action(&actions, "app alpha", false, PIN);
    assert_action(&actions, "gamma alpha", false, PIN);
}

#[test]
fn pin_in_a_dev_entry_outside_the_workspace_does_not_keep_the_version() {
    let manifest = format!(
        "[dependencies]\nalpha = {{ path = \"../libs/alpha\", version = \"=0.1.0\" }}\n{}{}",
        dep("beta", "libs"),
        dep("other", "ext"),
    );
    let dir = workspace(
        &manifest,
        &[
            ("app/src/lib.rs", "pub fn g() { beta::f(); other::f(); }\n"),
            (
                "libs/beta/Cargo.toml",
                "[package]\nname = \"beta\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
                 [dependencies]\nalpha = { path = \"../alpha\" }\n",
            ),
            ("libs/beta/src/lib.rs", "pub fn f() { alpha::f(); }\n"),
            (
                "ext/other/Cargo.toml",
                "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
                 [dev-dependencies]\nalpha = { path = \"../../libs/alpha\", version = \"=0.1.0\" }\n",
            ),
        ],
    );
    let actions = actions(&dir);
    assert_eq!(actions.len(), 1, "{actions:?}");
    // Cargo does not resolve the dev-dependencies of `other`.
    assert_action(&actions, "dependencies alpha", false, PIN);
}

#[test]
fn fix_dry_run_leaves_the_kept_entries_out_of_the_edits() {
    let manifest = format!(
        "[dependencies]\nalpha = {{ path = \"../libs/alpha\", version = \"=0.1.0\" }}\n{}\n\
         [dev-dependencies]\n{}{}",
        dep("beta", "libs"),
        dep("mac", "ext"),
        dep("gamma", "libs"),
    );
    let dir = workspace(
        &manifest,
        &[
            (
                "app/benches/b.rs",
                "#[mac::bench]\nfn work() {}\nfn main() { work(); }\n",
            ),
            (
                "libs/delta/Cargo.toml",
                "[package]\nname = \"delta\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
                 [dependencies]\nalpha = { path = \"../alpha\" }\n",
            ),
            ("libs/delta/src/lib.rs", "pub fn f() { alpha::f(); }\n"),
        ],
    );
    let before = fs::read_to_string(dir.path().join("app/Cargo.toml")).expect("read a file");
    let report = run(
        &dir,
        &[
            "fix",
            "--dry-run",
            "--only",
            "deps",
            "--format",
            "json",
            "--quiet",
        ],
    );
    let fixes: Vec<&str> = report["fixes"]
        .as_array()
        .expect("fixes is an array")
        .iter()
        .filter_map(|fix| fix["dependency"].as_str())
        .collect();
    assert_eq!(fixes, ["beta"], "{report}");
    assert_eq!(report["skipped"], 2, "{report}");
    let after = fs::read_to_string(dir.path().join("app/Cargo.toml")).expect("read a file");
    assert_eq!(after, before);
}
