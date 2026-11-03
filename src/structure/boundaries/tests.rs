use super::*;

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn a_manifest_line_declares_a_dependency_in_each_form() {
    assert!(declares_dependency(
        "fallow-core = { path = \"../core\" }",
        "fallow-core"
    ));
    assert!(declares_dependency(
        "fallow-core.workspace = true",
        "fallow-core"
    ));
    assert!(declares_dependency(
        "  \"fallow-core\" = \"1\"",
        "fallow-core"
    ));
    assert!(declares_dependency(
        "[dev-dependencies.fallow-core]",
        "fallow-core"
    ));
    assert!(declares_dependency(
        "[target.'cfg(unix)'.dependencies.fallow-core]",
        "fallow-core"
    ));
    assert!(!declares_dependency(
        "fallow-core-extra = \"1\"",
        "fallow-core"
    ));
    assert!(!declares_dependency("[package]", "fallow-core"));
    assert!(!declares_dependency("fallow-core", "fallow-core"));
}

#[test]
fn a_pattern_matches_tokens_and_not_text_in_strings_or_comments() {
    let pattern = lex("process_group(0)");
    assert_eq!(pattern, strings(&["process_group", "(", "0", ")"]));
    let code = Lexed::new("fn a() {\n    command.process_group ( 0 );\n}\n");
    let offset = code.find(&pattern);
    assert_eq!(
        offset.map(|at| line_col("fn a() {\n    command.process_group ( 0 );\n}\n", at)),
        Some((2, 13))
    );
    assert_eq!(
        Lexed::new("// process_group(0)\nlet a = \"process_group(0)\";").find(&pattern),
        None
    );
    assert_eq!(
        Lexed::new("CREATE_SUSPENDED_X").find(&lex("CREATE_SUSPENDED")),
        None
    );
    assert!(
        Lexed::new("use libc::waitid;")
            .find(&lex("libc::waitid"))
            .is_some()
    );
}

#[test]
fn relative_paths_become_crate_paths() {
    let module = strings(&["crate", "render", "human"]);
    assert_eq!(
        absolute(&module, &strings(&["crate", "a"])),
        Some(strings(&["crate", "a"]))
    );
    assert_eq!(
        absolute(&module, &strings(&["self", "x"])),
        Some(strings(&["crate", "render", "human", "x"]))
    );
    assert_eq!(
        absolute(&module, &strings(&["super", "super", "mutation"])),
        Some(strings(&["crate", "mutation"]))
    );
    assert_eq!(
        absolute(&module, &strings(&["super", "super", "super", "x"])),
        None
    );
    assert_eq!(absolute(&module, &strings(&["serde", "Serialize"])), None);
}

#[test]
fn module_paths_must_start_at_the_crate() {
    assert!(module_paths("r", &strings(&["crate::a::b"])).is_ok());
    assert!(module_paths("r", &strings(&["a::b"])).is_err());
    assert!(module_paths("r", &strings(&["crate::"])).is_err());
}
