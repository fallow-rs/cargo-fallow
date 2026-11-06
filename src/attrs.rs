//! Helpers that read attributes and names from the syntax tree.
//!
//! The mutation walk and the module graph use these helpers. They read text
//! only: a `cfg` condition is not evaluated.

use ra_ap_syntax::ast::{self, HasAttrs};
use ra_ap_syntax::{AstNode, SyntaxNode};

/// The strongest class of the attributes on one node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrClass {
    Test,
    Cfg,
    Other,
}

pub fn attributes_of(node: &SyntaxNode) -> AttrClass {
    let Some(owner) = ast::AnyHasAttrs::cast(node.clone()) else {
        return AttrClass::Other;
    };
    let mut class = AttrClass::Other;
    for attr in owner.attrs() {
        match classify(&attr_body(&attr)) {
            AttrClass::Test => return AttrClass::Test,
            AttrClass::Cfg => class = AttrClass::Cfg,
            AttrClass::Other => {}
        }
    }
    class
}

/// The text between the brackets of an attribute, without whitespace.
pub fn attr_body(attr: &ast::Attr) -> String {
    let text: String = attr
        .syntax()
        .text()
        .to_string()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let body = text
        .strip_prefix("#!")
        .or_else(|| text.strip_prefix('#'))
        .unwrap_or(&text);
    body.trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned()
}

pub fn classify(body: &str) -> AttrClass {
    if let Some(condition) = body.strip_prefix("cfg(") {
        let condition = condition.strip_suffix(')').unwrap_or(condition);
        return if only_in_tests(condition) {
            AttrClass::Test
        } else {
            AttrClass::Cfg
        };
    }
    if let Some(inner) = body.strip_prefix("cfg_attr(") {
        let inner = inner.strip_suffix(')').unwrap_or(inner);
        let parts = split_list(inner);
        return match parts.split_first() {
            Some((condition, attrs))
                if only_in_tests(condition) && attrs.iter().any(|attr| is_test_attribute(attr)) =>
            {
                AttrClass::Test
            }
            _ => AttrClass::Other,
        };
    }
    if is_test_attribute(body) {
        return AttrClass::Test;
    }
    AttrClass::Other
}

/// True for an attribute that makes a test out of a function. The list holds the
/// test macros of the wider ecosystem, also with a path before the name, for
/// example `#[tokio::test]`. The name `bench` is a test only as the built-in
/// attribute, see [`is_bench_attribute`].
pub fn is_test_attribute(body: &str) -> bool {
    const NAMES: [&str; 4] = ["test", "rstest", "test_case", "quickcheck"];
    let name = body.split('(').next().unwrap_or(body);
    let last = name.rsplit("::").next().unwrap_or(name);
    NAMES.contains(&last) || is_bench_attribute(name)
}

/// True for the path of the built-in bench attribute: `bench` or
/// `test::bench`. An attribute macro of another crate with the name `bench`,
/// for example `#[gpui::bench]`, can expand to production code, so it is not a
/// test attribute.
pub fn is_bench_attribute(path: &str) -> bool {
    matches!(path.trim_start_matches("::"), "bench" | "test::bench")
}

/// True when a `cfg` condition with no whitespace can only hold in a test build.
pub fn only_in_tests(condition: &str) -> bool {
    if matches!(condition, "test" | "doctest") {
        return true;
    }
    if let Some(list) = condition
        .strip_prefix("all(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return split_list(list).iter().any(|part| only_in_tests(part));
    }
    if let Some(list) = condition
        .strip_prefix("any(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let parts = split_list(list);
        return !parts.is_empty() && parts.iter().all(|part| only_in_tests(part));
    }
    false
}

/// The value of a `cfg` condition with no whitespace in a build with all
/// features and with tests, or `None` when it depends on the target or on
/// another setting. `--precise` mode loads the workspace with those settings.
pub fn cfg_value_all_features(condition: &str) -> Option<bool> {
    if condition.starts_with("feature=") || matches!(condition, "test" | "debug_assertions") {
        return Some(true);
    }
    let list = |prefix: &str| {
        condition
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(')'))
            .map(|inner| {
                split_list(inner)
                    .into_iter()
                    .map(cfg_value_all_features)
                    .collect::<Vec<_>>()
            })
    };
    if let Some(values) = list("all(") {
        if values.contains(&Some(false)) {
            return Some(false);
        }
        return values
            .iter()
            .all(|value| *value == Some(true))
            .then_some(true);
    }
    if let Some(values) = list("any(") {
        if values.contains(&Some(true)) {
            return Some(true);
        }
        return values
            .iter()
            .all(|value| *value == Some(false))
            .then_some(false);
    }
    if let Some(inner) = condition
        .strip_prefix("not(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return cfg_value_all_features(inner).map(|value| !value);
    }
    None
}

/// Splits a comma list at the top level of its parentheses and square brackets.
pub fn split_list(text: &str) -> Vec<&str> {
    let (mut parts, mut depth, mut start) = (Vec::new(), 0_u32, 0);
    for (at, c) in text.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&text[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    if start < text.len() {
        parts.push(&text[start..]);
    }
    parts
}

/// The value of a `path="..."` attribute body without whitespace.
fn path_value(body: &str) -> Option<String> {
    let value = body.strip_prefix("path=\"")?.strip_suffix('"')?;
    Some(value.to_owned())
}

/// One file that a `mod name;` declaration can load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleFile {
    /// The value of a `path` attribute, or `None` for the default file of the name.
    pub path: Option<String>,
    /// True when a `cfg_attr` condition decides whether the build loads the file.
    pub under_cfg: bool,
    /// True when a build with all features and tests can load a different file.
    pub cfg_off: bool,
    /// True when only a test build can load the file, for example with
    /// `cfg_attr(test, path = "mock.rs")`.
    pub test_only: bool,
}

/// The files that a `mod name;` declaration can load.
///
/// The first file comes from a plain `#[path]` attribute, or it is the default
/// file of the name. Each `path` in a `cfg_attr` attribute adds one file. The
/// syntax does not tell which condition holds, so each file is a candidate.
pub fn module_files(module: &ast::Module) -> Vec<ModuleFile> {
    let mut plain = None;
    let mut conditional = Vec::new();
    for attr in module.attrs() {
        let body = attr_body(&attr);
        if plain.is_none() {
            plain = path_value(&body);
        }
        cfg_attr_paths(&body, None, &mut conditional);
    }
    let first_off = conditional
        .iter()
        .any(|(_, condition)| cfg_value_all_features(condition) != Some(false));
    let mut files = vec![ModuleFile {
        path: plain,
        under_cfg: !conditional.is_empty(),
        cfg_off: first_off,
        test_only: false,
    }];
    files.extend(conditional.into_iter().map(|(path, condition)| ModuleFile {
        path: Some(path),
        under_cfg: true,
        cfg_off: cfg_value_all_features(&condition) != Some(true),
        test_only: only_in_tests(&condition),
    }));
    files
}

/// Collects each `path` value of a `cfg_attr(..)` body without whitespace,
/// with its condition. A nested `cfg_attr` joins its condition to `outer`.
fn cfg_attr_paths(body: &str, outer: Option<&str>, into: &mut Vec<(String, String)>) {
    let Some(inner) = body
        .strip_prefix("cfg_attr(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return;
    };
    let parts = split_list(inner);
    let Some((condition, attrs)) = parts.split_first() else {
        return;
    };
    let condition = outer.map_or_else(
        || (*condition).to_owned(),
        |outer| format!("all({outer},{condition})"),
    );
    for attr in attrs {
        match path_value(attr) {
            Some(path) => into.push((path, condition.clone())),
            None => cfg_attr_paths(attr, Some(&condition), into),
        }
    }
}

/// The text of a name, without a `r#` prefix.
pub fn plain_name(name: &ast::Name) -> String {
    let text = name.syntax().text().to_string();
    text.strip_prefix("r#")
        .map_or_else(|| text.clone(), str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::{ModuleFile, cfg_value_all_features, module_files};
    use ra_ap_syntax::AstNode;
    use ra_ap_syntax::ast;

    fn files_of(text: &str) -> Vec<ModuleFile> {
        let file = ast::SourceFile::parse(text, ra_ap_syntax::Edition::Edition2024).tree();
        let Some(module) = file.syntax().descendants().find_map(ast::Module::cast) else {
            panic!("a module declaration");
        };
        module_files(&module)
    }

    fn entry(path: Option<&str>, under_cfg: bool, cfg_off: bool) -> ModuleFile {
        ModuleFile {
            path: path.map(str::to_owned),
            under_cfg,
            cfg_off,
            test_only: false,
        }
    }

    fn test_entry(path: &str, cfg_off: bool) -> ModuleFile {
        ModuleFile {
            test_only: true,
            ..entry(Some(path), true, cfg_off)
        }
    }

    #[test]
    fn a_plain_declaration_loads_one_file() {
        assert_eq!(files_of("mod a;"), [entry(None, false, false)]);
        assert_eq!(
            files_of("#[path = \"x.rs\"] mod a;"),
            [entry(Some("x.rs"), false, false)]
        );
    }

    #[test]
    fn each_cfg_attr_path_is_a_file_with_its_condition() {
        assert_eq!(
            files_of(
                "#[cfg_attr(feature = \"f\", path = \"on.rs\")]\n\
                 #[cfg_attr(not(feature = \"f\"), path = \"off.rs\")]\n\
                 #[cfg_attr(unix, doc = \"x\", cfg_attr(test, path = \"nested.rs\"))]\nmod a;"
            ),
            [
                entry(None, true, true),
                entry(Some("on.rs"), true, false),
                entry(Some("off.rs"), true, true),
                test_entry("nested.rs", true),
            ]
        );
        assert_eq!(
            files_of("#[cfg_attr(test, path = \"mock.rs\")] mod a;"),
            [entry(None, true, true), test_entry("mock.rs", false)]
        );
        assert_eq!(
            files_of("#[cfg_attr(not(feature = \"f\"), path = \"off.rs\")] mod a;"),
            [entry(None, true, false), entry(Some("off.rs"), true, true)]
        );
    }

    #[test]
    fn a_cfg_is_on_only_when_features_and_tests_decide_it() {
        assert_eq!(cfg_value_all_features("feature=\"a\""), Some(true));
        assert_eq!(
            cfg_value_all_features("all(test,feature=\"a\")"),
            Some(true)
        );
        assert_eq!(cfg_value_all_features("not(feature=\"a\")"), Some(false));
        assert_eq!(
            cfg_value_all_features("any(windows,feature=\"a\")"),
            Some(true)
        );
        assert_eq!(cfg_value_all_features("all(windows,feature=\"a\")"), None);
        assert_eq!(cfg_value_all_features("target_os=\"none\""), None);
        assert_eq!(cfg_value_all_features("not(windows)"), None);
    }
}
