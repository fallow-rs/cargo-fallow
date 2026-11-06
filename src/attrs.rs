//! Helpers that read attributes and names from the syntax tree.
//!
//! The mutation walk and the module graph use these helpers. They read the
//! syntax only. A `cfg` condition has a value only when the condition alone
//! decides it, see [`cfg_constant`] and [`cfg_value_all_features`].

use ra_ap_syntax::ast::{self, HasAttrs};
use ra_ap_syntax::{AstNode, AstToken, SyntaxNode};

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

/// The value of a `cfg` condition with no whitespace in every build, or
/// `None` when a build setting decides it. Only constant forms have a value,
/// for example `any()`, `all()`, `true` and `not(any())`.
pub fn cfg_constant(condition: &str) -> Option<bool> {
    match condition {
        "true" => return Some(true),
        "false" => return Some(false),
        _ => {}
    }
    let list = |prefix: &str| {
        condition
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(')'))
            .map(|inner| {
                split_list(inner)
                    .into_iter()
                    .map(cfg_constant)
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
        return cfg_constant(inner).map(|value| !value);
    }
    None
}

/// The value of a `path = ".."` meta. The parser reads each string form, also
/// a raw string, and the value has its escapes resolved like rustc does.
fn path_value(meta: &ast::Meta) -> Option<String> {
    let ast::Meta::KeyValueMeta(pair) = meta else {
        return None;
    };
    let key = pair.path()?;
    if key.qualifier().is_some() || key.segment()?.name_ref()?.text() != "path" {
        return None;
    }
    let ast::Expr::Literal(literal) = pair.expr()? else {
        return None;
    };
    let string = ast::String::cast(literal.token())?;
    string.value().ok().map(std::borrow::Cow::into_owned)
}

/// The text of a node without whitespace.
fn compact(node: &SyntaxNode) -> String {
    node.text()
        .to_string()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
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

impl ModuleFile {
    /// The default file of a name, with no `cfg` attribute.
    pub const fn plain() -> Self {
        Self {
            path: None,
            under_cfg: false,
            cfg_off: false,
            test_only: false,
        }
    }

    /// `other`, with the flags of both files.
    pub fn join(&self, other: &Self) -> Self {
        Self {
            path: other.path.clone(),
            under_cfg: self.under_cfg || other.under_cfg,
            cfg_off: self.cfg_off || other.cfg_off,
            test_only: self.test_only || other.test_only,
        }
    }
}

/// The files that a `mod name;` declaration can load.
///
/// The first file comes from a plain `#[path]` attribute, or it is the default
/// file of the name. Each `path` in a `cfg_attr` attribute adds one file. The
/// syntax does not tell which condition holds, so each file is a candidate.
/// A condition that is false in every build, for example `any()`, adds no
/// file. On an inline module, each value is a folder for the nested modules.
pub fn module_files(module: &ast::Module) -> Vec<ModuleFile> {
    let mut plain = None;
    let mut conditional = Vec::new();
    for meta in module.attrs().filter_map(|attr| attr.meta()) {
        if plain.is_none() {
            plain = path_value(&meta);
        }
        cfg_attr_paths(&meta, None, &mut conditional);
    }
    // A condition that no build makes true loads no file.
    conditional.retain(|(_, condition)| cfg_constant(condition) != Some(false));
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

/// Collects each `path` value of a `cfg_attr` meta with its condition, without
/// whitespace. A nested `cfg_attr` joins its condition to `outer`.
fn cfg_attr_paths(meta: &ast::Meta, outer: Option<&str>, into: &mut Vec<(String, String)>) {
    let ast::Meta::CfgAttrMeta(cfg_attr) = meta else {
        return;
    };
    let Some(predicate) = cfg_attr.cfg_predicate() else {
        return;
    };
    let condition = compact(predicate.syntax());
    let condition = outer.map_or_else(
        || condition.clone(),
        |outer| format!("all({outer},{condition})"),
    );
    for inner in cfg_attr.metas() {
        match path_value(&inner) {
            Some(path) => into.push((path, condition.clone())),
            None => cfg_attr_paths(&inner, Some(&condition), into),
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
    use super::{ModuleFile, cfg_constant, cfg_value_all_features, module_files};
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
    fn a_raw_or_escaped_path_literal_gives_its_value() {
        assert_eq!(
            files_of("#[path = r\"plat/raw.rs\"] mod a;"),
            [entry(Some("plat/raw.rs"), false, false)]
        );
        assert_eq!(
            files_of("#[cfg_attr(unix, path = r#\"plat/raw.rs\"#)] mod a;"),
            [
                entry(None, true, true),
                entry(Some("plat/raw.rs"), true, true)
            ]
        );
        assert_eq!(
            files_of("#[path = \"plat\\\\win.rs\"] mod a;"),
            [entry(Some("plat\\win.rs"), false, false)]
        );
        assert_eq!(
            files_of("#[path = \"es\\u{63}.rs\"] mod a;"),
            [entry(Some("esc.rs"), false, false)]
        );
        // Only the key `path` names a file.
        assert_eq!(
            files_of("#[doc = \"x.rs\"] #[a::path = \"y.rs\"] mod a;"),
            [entry(None, false, false)]
        );
    }

    #[test]
    fn a_condition_that_no_build_makes_true_loads_no_file() {
        for never in ["any()", "not(all())", "false", "all(unix,any())"] {
            assert_eq!(
                files_of(&format!("#[cfg_attr({never}, path = \"x.rs\")] mod a;")),
                [entry(None, false, false)],
                "{never}"
            );
        }
        // `not(feature = "f")` is false only with all features, so the file stays.
        assert_eq!(
            files_of("#[cfg_attr(any(not(feature = \"f\")), path = \"x.rs\")] mod a;"),
            [entry(None, true, false), entry(Some("x.rs"), true, true)]
        );
        assert_eq!(
            files_of("#[cfg_attr(not(any()), path = \"x.rs\")] mod a;"),
            [entry(None, true, true), entry(Some("x.rs"), true, false)]
        );
    }

    #[test]
    fn a_constant_cfg_has_a_value_and_a_setting_has_none() {
        assert_eq!(cfg_constant("any()"), Some(false));
        assert_eq!(cfg_constant("all()"), Some(true));
        assert_eq!(cfg_constant("not(any())"), Some(true));
        assert_eq!(cfg_constant("any(unix,all())"), Some(true));
        assert_eq!(cfg_constant("all(unix,false)"), Some(false));
        assert_eq!(cfg_constant("unix"), None);
        assert_eq!(cfg_constant("not(feature=\"f\")"), None);
        assert_eq!(cfg_constant("any(unix,any())"), None);
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
