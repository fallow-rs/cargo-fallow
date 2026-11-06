//! Black-box tests of the `files` command: the `.rs` files that no target
//! compiles.
//!
//! Each test writes a small workspace to a temporary directory. The package
//! `app` is in `app/`. A finding is its path from the workspace root.
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

/// Writes a workspace with the member `app` and the files.
fn workspace(manifest_extra: &str, files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    write(
        dir.path(),
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\"]\nresolver = \"3\"\n",
    );
    write(
        dir.path(),
        "app/Cargo.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n{manifest_extra}"
        ),
    );
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

/// Runs the binary with the arguments and returns the exit code and the
/// JSON report.
fn run_args(dir: &TempDir, args: &[&str]) -> (i32, Value) {
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args(args)
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

/// Runs `files --format json --quiet` with the extra arguments.
fn run(dir: &TempDir, extra: &[&str]) -> (i32, Value) {
    let mut args = vec!["files", "--format", "json", "--quiet"];
    args.extend_from_slice(extra);
    run_args(dir, &args)
}

/// The paths of the findings.
fn paths(report: &Value) -> BTreeSet<String> {
    report["findings"]
        .as_array()
        .expect("findings is an array")
        .iter()
        .map(|finding| finding["path"].as_str().unwrap_or("?").to_owned())
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[test]
fn orphan_file_is_reported_and_declared_files_are_not() {
    let dir = workspace(
        "",
        &[
            (
                "app/src/lib.rs",
                "mod used;\nmod nested;\n#[path = \"other/named.rs\"]\nmod named;\n",
            ),
            ("app/src/used.rs", ""),
            ("app/src/nested/mod.rs", "mod inner;\n"),
            ("app/src/nested/inner.rs", ""),
            ("app/src/other/named.rs", ""),
            ("app/src/orphan.rs", "pub fn f() {}\n"),
            ("app/src/nested/old.rs", ""),
        ],
    );
    let (code, report) = run(&dir, &[]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(
        paths(&report),
        set(&["app/src/nested/old.rs", "app/src/orphan.rs"])
    );
    assert_eq!(report["kind"], "files");
    let finding = &report["findings"][0];
    assert_eq!(finding["rule"], "unused-file");
    assert_eq!(finding["package"], "app");
    assert_eq!(finding["actions"][0]["type"], "delete-file");
    assert_eq!(finding["actions"][0]["auto_fixable"], false);
    assert_eq!(report["total_issues"], 2);
    let gate = &report["gate_outcomes"]["unused-files"];
    assert_eq!(gate["status"], "fail");
    assert_eq!(gate["enforced"], false);
}

#[test]
fn fail_on_issues_enforces_the_gate() {
    let dir = workspace("", &[("app/src/orphan.rs", "")]);
    let (code, report) = run(&dir, &["--fail-on-issues"]);
    assert_eq!(code, 1, "{report}");
    assert_eq!(report["gate_outcomes"]["unused-files"]["enforced"], true);
    let clean = workspace("", &[]);
    let (code, report) = run(&clean, &["--fail-on-issues"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["gate_outcomes"]["unused-files"]["status"], "pass");
}

#[test]
fn test_helper_module_and_fixture_folders_are_not_reported() {
    let dir = workspace(
        "",
        &[
            ("app/tests/it.rs", "mod common;\n"),
            ("app/tests/common/mod.rs", "pub fn helper() {}\n"),
            ("app/tests/common/stale.rs", ""),
            ("app/tests/fixtures/input.rs", "fn main() {}\n"),
            ("app/tests/ui/compile_fail.rs", "fn main() { x }\n"),
            ("app/tests/data/sample.rs", ""),
            ("app/benches/b.rs", ""),
            ("app/examples/e.rs", "fn main() {}\n"),
            ("app/examples/stale.rs", "fn main() {}\n"),
        ],
    );
    let (_, report) = run(&dir, &[]);
    // `examples/stale.rs` is an example target that Cargo finds by itself.
    assert_eq!(paths(&report), set(&["app/tests/common/stale.rs"]));
}

#[test]
fn target_off_auto_discovery_leaves_its_file_unused() {
    let dir = workspace(
        "autoexamples = false\n",
        &[
            ("app/examples/e.rs", "fn main() {}\n"),
            ("app/examples/f.rs", "fn main() {}\n"),
        ],
    );
    let (_, report) = run(&dir, &[]);
    // No example target compiles a file in `examples/`, so the folder has no
    // reached file and the check gives credit.
    assert_eq!(paths(&report), set(&[]));
    let named = workspace(
        "autoexamples = false\n[[example]]\nname = \"e\"\npath = \"examples/e.rs\"\n",
        &[
            ("app/examples/e.rs", "fn main() {}\n"),
            ("app/examples/f.rs", "fn main() {}\n"),
        ],
    );
    let (_, report) = run(&named, &[]);
    assert_eq!(paths(&report), set(&["app/examples/f.rs"]));
}

#[test]
fn module_in_a_macro_call_and_a_cfg_attr_path_count() {
    let dir = workspace(
        "",
        &[
            (
                "app/src/lib.rs",
                "cfg_if::cfg_if! {\n    if #[cfg(unix)] { mod unix; } else { mod windows; }\n}\n\
                 #[cfg_attr(unix, path = \"sys/unix_impl.rs\")]\n\
                 #[cfg_attr(windows, path = \"sys/windows_impl.rs\")]\n\
                 mod sys;\n",
            ),
            ("app/src/unix.rs", ""),
            ("app/src/windows.rs", "mod helper;\n"),
            ("app/src/windows/helper.rs", ""),
            ("app/src/sys/unix_impl.rs", ""),
            ("app/src/sys/windows_impl.rs", ""),
            ("app/src/sys/stale.rs", ""),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(paths(&report), set(&["app/src/sys/stale.rs"]));
}

#[test]
fn path_attribute_in_an_inline_module_uses_the_inline_folder() {
    let dir = workspace(
        "",
        &[
            (
                "app/src/lib.rs",
                "mod outer {\n    #[path = \"inner_impl.rs\"]\n    mod inner;\n}\n",
            ),
            ("app/src/outer/inner_impl.rs", ""),
            ("app/src/outer/stale.rs", ""),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(paths(&report), set(&["app/src/outer/stale.rs"]));
}

#[test]
fn literal_include_counts_and_a_dynamic_include_skips_the_package() {
    let dir = workspace(
        "",
        &[
            (
                "app/src/lib.rs",
                "include!(\"part.rs\");\nconst T: &str = include_str!(r\"text.rs\");\n\
                 include!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));\n",
            ),
            ("app/src/part.rs", ""),
            ("app/src/text.rs", ""),
            ("app/src/orphan.rs", ""),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(paths(&report), set(&["app/src/orphan.rs"]));

    let dynamic = workspace(
        "",
        &[
            (
                "app/src/lib.rs",
                "include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src/part.rs\"));\n",
            ),
            ("app/src/part.rs", ""),
            ("app/src/orphan.rs", ""),
        ],
    );
    let (_, report) = run(&dynamic, &[]);
    assert_eq!(paths(&report), set(&[]));
    assert_eq!(report["summary"]["skipped_packages"][0]["package"], "app");
}

#[test]
fn macro_that_declares_a_module_skips_the_package() {
    let dir = workspace(
        "",
        &[
            (
                "app/src/lib.rs",
                "macro_rules! m { ($name:ident) => { mod $name; } }\nm!(hidden);\n",
            ),
            ("app/src/hidden.rs", ""),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(paths(&report), set(&[]));
    assert_eq!(report["summary"]["skipped_packages"][0]["package"], "app");
}

#[test]
fn generated_files_and_nested_packages_are_not_reported() {
    let dir = workspace(
        "",
        &[
            ("app/src/gen.rs", "// @generated by a tool\npub fn f() {}\n"),
            (
                "app/src/tool/Cargo.toml",
                "[package]\nname = \"tool\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            ),
            ("app/src/tool/src/lib.rs", ""),
            ("app/src/tool/src/stale.rs", ""),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(paths(&report), set(&[]));
}

#[test]
fn a_generated_file_still_reaches_its_modules_and_includes() {
    let dir = workspace(
        "",
        &[
            ("app/src/lib.rs", "mod tables_gen;\nmod plain;\n"),
            (
                "app/src/tables_gen.rs",
                "// @generated by a tool\nmod tables;\nconst T: &str = include_str!(\"words.rs\");\n",
            ),
            ("app/src/tables_gen/tables.rs", ""),
            ("app/src/words.rs", ""),
            (
                "app/src/plain.rs",
                "// @generated by a tool\npub static W: &[&str] = &[\"include\", \"model\"];\n",
            ),
            ("app/src/stale.rs", ""),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(paths(&report), set(&["app/src/stale.rs"]));
}

#[test]
fn build_script_file_and_its_modules_count() {
    let dir = workspace(
        "build = \"build/main.rs\"\n",
        &[
            ("app/build/main.rs", "mod util;\nfn main() {}\n"),
            ("app/build/util.rs", ""),
            ("app/build/stale.rs", ""),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(paths(&report), set(&["app/build/stale.rs"]));
}

#[test]
fn check_runs_the_files_check_by_default() {
    let dir = workspace("", &[("app/src/orphan.rs", "")]);
    let (code, report) = run_args(
        &dir,
        &["check", "--format", "json", "--quiet", "--skip", "deps"],
    );
    assert_eq!(code, 0, "{report}");
    let checks: Vec<&str> = report["checks"]
        .as_array()
        .expect("checks is an array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(checks.contains(&"files"), "{checks:?}");
    let rules: Vec<&str> = report["findings"]
        .as_array()
        .expect("findings is an array")
        .iter()
        .filter_map(|finding| finding["rule"].as_str())
        .collect();
    assert_eq!(rules, ["unused-file"]);
    assert_eq!(report["sections"]["files"]["total_issues"], 1);
    let (code, _) = run_args(
        &dir,
        &[
            "check",
            "--format",
            "json",
            "--quiet",
            "--only",
            "files",
            "--fail-on-issues",
        ],
    );
    assert_eq!(code, 1);
}

#[test]
fn human_and_sarif_forms_name_the_file() {
    let dir = workspace("", &[("app/src/orphan.rs", "")]);
    let output = Command::cargo_bin("cargo-fallow")
        .expect("the binary exists")
        .current_dir(dir.path())
        .args(["files", "--quiet"])
        .output()
        .expect("run the binary");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("app/src/orphan.rs"), "{text}");
    assert!(text.contains("unused-files"), "{text}");
    let (_, sarif) = run_args(&dir, &["files", "--format", "sarif", "--quiet"]);
    let result = &sarif["runs"][0]["results"][0];
    assert_eq!(result["ruleId"], "unused-file");
    assert_eq!(
        result["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        "app/src/orphan.rs"
    );
}

#[test]
fn included_file_with_modules_skips_the_package() {
    let dir = workspace(
        "",
        &[
            ("app/src/lib.rs", "include!(\"gen.rs\");\n"),
            ("app/src/gen.rs", "mod inner;\n"),
            ("app/src/inner.rs", ""),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(paths(&report), set(&[]));
    assert_eq!(report["summary"]["skipped_packages"][0]["package"], "app");
}

#[test]
fn path_attribute_on_an_inline_module_sets_the_folder_of_its_modules() {
    let dir = workspace(
        "",
        &[
            (
                "app/src/lib.rs",
                "#[path = \"sub\"]\nmod outer {\n    mod leaf;\n}\n\
                 #[path = \"outer/kept.rs\"]\nmod kept;\n\
                 #[cfg_attr(unix, path = \"csub\")]\nmod co {\n    mod cl;\n}\n\
                 #[path = r\"raw_impl.rs\"]\nmod raw;\n\
                 #[cfg_attr(any(), path = \"never_impl.rs\")]\nmod never;\n\
                 #[cfg_attr(windows, path = \"win\\\\imp.rs\")]\nmod win;\n",
            ),
            ("app/src/sub/leaf.rs", ""),
            ("app/src/outer/kept.rs", ""),
            // rustc loads `sub/leaf.rs`, not the default `outer/leaf.rs`.
            ("app/src/outer/leaf.rs", ""),
            ("app/src/csub/cl.rs", ""),
            ("app/src/co/cl.rs", ""),
            ("app/src/raw_impl.rs", ""),
            ("app/src/never.rs", ""),
            ("app/src/never_impl.rs", ""),
            ("app/src/win.rs", ""),
            ("app/src/win/imp.rs", ""),
            ("app/src/stale.rs", ""),
        ],
    );
    let (_, report) = run(&dir, &[]);
    assert_eq!(
        paths(&report),
        set(&[
            "app/src/never_impl.rs",
            "app/src/outer/leaf.rs",
            "app/src/stale.rs",
        ]),
        "{report}"
    );
    assert_eq!(report["summary"]["skipped_packages"], serde_json::json!([]));
}

#[test]
fn macro_from_another_member_that_declares_a_module_skips_each_package() {
    let dir = tempfile::tempdir().expect("create a temporary directory");
    write(
        dir.path(),
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\", \"macros\"]\nresolver = \"3\"\n",
    );
    for name in ["app", "macros"] {
        write(
            dir.path(),
            &format!("{name}/Cargo.toml"),
            &format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
        );
    }
    write(
        dir.path(),
        "macros/src/lib.rs",
        "#[macro_export]\nmacro_rules! m { ($n:ident) => { mod $n; } }\n",
    );
    write(dir.path(), "app/src/lib.rs", "macros::m!(foo);\n");
    write(dir.path(), "app/src/foo.rs", "");
    let (_, report) = run(&dir, &[]);
    assert_eq!(paths(&report), set(&[]));
    let skipped: Vec<&str> = report["summary"]["skipped_packages"]
        .as_array()
        .expect("skipped_packages is an array")
        .iter()
        .filter_map(|entry| entry["package"].as_str())
        .collect();
    assert_eq!(skipped, ["app", "macros"]);
}

#[test]
fn macro_module_forms_that_the_scan_cannot_follow_give_credit() {
    // Each case: the code, and the files that stay unused.
    let cases: [(&str, &[&str]); 4] = [
        ("paste::paste! { mod [<a _x>]; }\n", &[]),
        (
            "cfg_if::cfg_if! { if #[cfg(unix)] { mod a { mod b; } } }\n",
            &[],
        ),
        // A macro gives the literal to `include_str!`, so the `.rs` literal
        // in the macro call counts. The package stays judged.
        (
            "macro_rules! my_include { ($p:literal) => { include_str!($p) }; }\n\
             const S: &str = my_include!(\"foo.rs\");\n",
            &["app/src/a/b.rs", "app/src/a_x.rs"],
        ),
        ("fn f() {\n    #[path = \"foo.rs\"]\n    mod m;\n}\n", &[]),
    ];
    for (text, unused) in cases {
        // `keep.rs` is a reached file in `src/a`, so the folder rule does not
        // hide `src/a/b.rs`.
        let lib = format!("{text}#[path = \"a/keep.rs\"]\nmod keep;\n");
        let dir = workspace(
            "",
            &[
                ("app/src/lib.rs", &lib),
                ("app/src/a_x.rs", ""),
                ("app/src/a/b.rs", ""),
                ("app/src/a/keep.rs", ""),
                ("app/src/foo.rs", ""),
            ],
        );
        let (_, report) = run(&dir, &[]);
        assert_eq!(paths(&report), set(unused), "{text}");
    }
}

/// Runs git in `dir` with a fixed test identity.
fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
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
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?}");
}

#[test]
fn changed_since_keeps_only_the_changed_files() {
    let dir = workspace("", &[("app/src/old.rs", "")]);
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "base"]);
    write(dir.path(), "app/src/new.rs", "");
    let (_, report) = run(&dir, &["--changed-since", "HEAD"]);
    assert_eq!(paths(&report), set(&["app/src/new.rs"]));
    let (_, full) = run(&dir, &[]);
    assert_eq!(paths(&full), set(&["app/src/new.rs", "app/src/old.rs"]));
}

#[test]
fn check_reuses_the_graph_parse_and_gives_the_same_files_section() {
    // Each rule of the walk: a module in a macro call, a `cfg_attr` path, a
    // literal include, a generated file, a second target with another edition,
    // and a file that two targets reach.
    let dir = workspace(
        "[[bin]]\nname = \"tool\"\npath = \"src/bin/tool.rs\"\nedition = \"2018\"\n",
        &[
            (
                "app/src/lib.rs",
                "m! { mod in_macro; }\n#[cfg_attr(unix, path = \"sys/unix_impl.rs\")]\nmod sys;\n\
                 mod tables_gen;\nmod shared;\nconst T: &str = include_str!(\"words.rs\");\n",
            ),
            ("app/src/in_macro.rs", ""),
            ("app/src/sys/unix_impl.rs", ""),
            ("app/src/sys/stale.rs", ""),
            (
                "app/src/tables_gen.rs",
                "// @generated by a tool\npub static W: &[&str] = &[\"include\", \"model\"];\n",
            ),
            ("app/src/words.rs", ""),
            ("app/src/shared.rs", "mod inner;\n"),
            ("app/src/shared/inner.rs", ""),
            (
                "app/src/bin/tool.rs",
                "#[path = \"../shared.rs\"]\nmod shared;\n",
            ),
            ("app/src/orphan.rs", ""),
        ],
    );
    let (_, files) = run(&dir, &[]);
    let (_, check) = run_args(
        &dir,
        &["check", "--format", "json", "--quiet", "--skip", "deps"],
    );
    assert_eq!(check["sections"]["files"], files, "{check:#}");
    assert_eq!(
        paths(&files),
        set(&["app/src/orphan.rs", "app/src/sys/stale.rs"])
    );
}

#[test]
fn an_error_of_files_comes_after_an_error_of_boundaries_in_check() {
    // The files check runs on the graph thread. Its error keeps the place that
    // it had before: after the error of an earlier check.
    let dir = workspace("", &[]);
    let manifest = dir.path().join("Cargo.toml");
    let text = fs::read_to_string(&manifest).expect("read the manifest");
    let text = format!(
        "{text}\n[workspace.metadata.fallow.files]\nnope = 1\n\n\
         [workspace.metadata.fallow.boundaries]\nnope = 1\n"
    );
    fs::write(&manifest, text).expect("write the manifest");
    let (code, report) = run_args(
        &dir,
        &["check", "--format", "json", "--quiet", "--skip", "deps"],
    );
    assert_eq!(code, 2, "{report}");
    let message = report["message"].as_str().unwrap_or_default();
    assert!(message.contains("fallow.boundaries"), "{message}");
}
