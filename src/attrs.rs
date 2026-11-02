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
/// test macros of the wider ecosystem, also with a path before the name.
pub fn is_test_attribute(body: &str) -> bool {
    const NAMES: [&str; 5] = ["test", "bench", "rstest", "test_case", "quickcheck"];
    let name = body.split('(').next().unwrap_or(body);
    let last = name.rsplit("::").next().unwrap_or(name);
    NAMES.contains(&last)
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

/// The value of a `#[path = "..."]` attribute.
pub fn path_attribute(attr: &ast::Attr) -> Option<String> {
    let body = attr_body(attr);
    let value = body.strip_prefix("path=\"")?.strip_suffix('"')?;
    Some(value.to_owned())
}

/// The text of a name, without a `r#` prefix.
pub fn plain_name(name: &ast::Name) -> String {
    let text = name.syntax().text().to_string();
    text.strip_prefix("r#")
        .map_or_else(|| text.clone(), str::to_owned)
}
