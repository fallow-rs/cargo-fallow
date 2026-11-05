//! The header of each named item of a file: the tokens that a user of the
//! item can see, without the body.
//!
//! A header keeps the `cfg` and `derive` attributes, the visibility, the
//! signature, the generics and the `where` clause. It leaves out the doc
//! comments, the other attributes, a function body, the value of a `const` or
//! a `static`, and the member lists of a type, a trait and a module: each
//! member has its own header. A header is one line of tokens with one space
//! between two tokens, so a change of the layout is no change.
//!
//! A change that no caller can see is no change either:
//!
//! - the pattern of a parameter, for example its name;
//! - the name of a generic parameter or a lifetime, which the header replaces
//!   with its place in the list;
//! - the module part of a path, so `std::vec::Vec<u8>` and `Vec<u8>` are one
//!   type;
//! - the order of the names in `#[derive(..)]`;
//! - a trailing comma.

use ra_ap_syntax::ast::{self, HasAttrs, HasGenericParams, HasName};
use ra_ap_syntax::{AstNode, NodeOrToken, SyntaxKind, SyntaxNode, SyntaxToken};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::attrs::{attr_body, split_list};
use crate::parse::{Edition, with_parsed};
use crate::structure::source::{LineIndex, is_space_or_comment};

/// The facts of one named item.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fact {
    /// True when `#[doc(hidden)]` is on the item.
    pub hidden: bool,
    /// The normalized header.
    pub header: String,
}

/// The facts of one file.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    /// The facts of each named item, by the line of the name and the name.
    pub items: FxHashMap<(u32, String), Fact>,
    /// The lines of each `use` item and each `impl` block with
    /// `#[doc(hidden)]`.
    pub hidden_lines: FxHashSet<u32>,
}

/// Reads the facts of `text`. A file that does not parse gives no facts.
pub fn facts(text: &str, edition: Edition) -> Facts {
    with_parsed(text, edition, |file, normalized| {
        let index = LineIndex::new(normalized);
        let mut found = Facts::default();
        for node in file.syntax().descendants() {
            if matches!(node.kind(), SyntaxKind::USE | SyntaxKind::IMPL) {
                if is_hidden(&node) {
                    let first = index.position(node.text_range().start()).0;
                    let last = index.end_line(node.text_range().end());
                    found.hidden_lines.extend(first..=last);
                }
                continue;
            }
            let Some(name) = ast::AnyHasName::cast(node.clone()).and_then(|named| named.name())
            else {
                continue;
            };
            let Some(skip) = skipped_children(node.kind()) else {
                continue;
            };
            let line = index.position(name.syntax().text_range().start()).0;
            let fact = Fact {
                hidden: is_hidden(&node),
                header: header(&node, skip),
            };
            found
                .items
                .insert((line, crate::attrs::plain_name(&name)), fact);
        }
        found
    })
    .unwrap_or_default()
}

/// The child kinds that the header of an item of `kind` leaves out, or
/// `None` for a kind with no header.
fn skipped_children(kind: SyntaxKind) -> Option<&'static [SyntaxKind]> {
    Some(match kind {
        SyntaxKind::FN => &[SyntaxKind::BLOCK_EXPR],
        SyntaxKind::STRUCT | SyntaxKind::UNION => {
            &[SyntaxKind::RECORD_FIELD_LIST, SyntaxKind::TUPLE_FIELD_LIST]
        }
        SyntaxKind::ENUM => &[SyntaxKind::VARIANT_LIST],
        SyntaxKind::TRAIT => &[SyntaxKind::ASSOC_ITEM_LIST],
        SyntaxKind::MODULE => &[SyntaxKind::ITEM_LIST],
        SyntaxKind::MACRO_RULES | SyntaxKind::MACRO_DEF => &[SyntaxKind::TOKEN_TREE],
        SyntaxKind::CONST | SyntaxKind::STATIC => &[EXPRESSION],
        SyntaxKind::TYPE_ALIAS | SyntaxKind::VARIANT | SyntaxKind::RECORD_FIELD => &[],
        _ => return None,
    })
}

/// A marker for the value of a `const` or a `static`: each expression node
/// after the `=` token.
const EXPRESSION: SyntaxKind = SyntaxKind::EOF;

fn is_hidden(node: &SyntaxNode) -> bool {
    ast::AnyHasAttrs::cast(node.clone()).is_some_and(|owner| {
        owner
            .attrs()
            .any(|attr| attr_body(&attr).starts_with("doc(hidden"))
    })
}

fn header(node: &SyntaxNode, skip: &[SyntaxKind]) -> String {
    let mut emitter = Emitter {
        tokens: Vec::new(),
        generics: generic_names(node),
    };
    let mut after_eq = false;
    for child in node.children_with_tokens() {
        match child {
            NodeOrToken::Token(token) => {
                if token.kind() == SyntaxKind::EQ && skip.contains(&EXPRESSION) {
                    after_eq = true;
                } else if token.kind() != SyntaxKind::SEMICOLON {
                    emitter.token(&token);
                }
            }
            NodeOrToken::Node(inner) if !after_eq && !skip.contains(&inner.kind()) => {
                emitter.top_node(&inner);
            }
            NodeOrToken::Node(_) => {}
        }
    }
    drop_trailing_commas(&emitter.tokens).join(" ")
}

/// The generic parameters of the item and of the `impl` block or the trait
/// around it, mapped to their place: `T0`, `T1` and `'l0`.
fn generic_names(node: &SyntaxNode) -> FxHashMap<String, String> {
    let mut names = FxHashMap::default();
    // A member uses the generics of its `impl` block, trait or type.
    let owner = node.ancestors().skip(1).find(|ancestor| {
        matches!(
            ancestor.kind(),
            SyntaxKind::IMPL
                | SyntaxKind::TRAIT
                | SyntaxKind::STRUCT
                | SyntaxKind::ENUM
                | SyntaxKind::UNION
        )
    });
    for scope in owner.iter().chain(std::iter::once(node)) {
        let Some(list) = ast::AnyHasGenericParams::cast(scope.clone())
            .and_then(|item| item.generic_param_list())
        else {
            continue;
        };
        for param in list.generic_params() {
            if let Some(name) = param_name(&param) {
                let index = names.len();
                let placeholder = if name.starts_with('\'') {
                    format!("'l{index}")
                } else {
                    format!("T{index}")
                };
                names.insert(name, placeholder);
            }
        }
    }
    names
}

/// The name of a generic parameter: `T`, `N` or `'a`.
fn param_name(param: &ast::GenericParam) -> Option<String> {
    match param {
        ast::GenericParam::LifetimeParam(lifetime) => {
            lifetime.lifetime().map(|name| name.text().to_string())
        }
        ast::GenericParam::TypeParam(param) => param.name().map(|name| name.text().to_string()),
        ast::GenericParam::ConstParam(param) => param.name().map(|name| name.text().to_string()),
    }
}

/// Writes the normalized tokens of a header.
struct Emitter {
    tokens: Vec<String>,
    generics: FxHashMap<String, String>,
}

impl Emitter {
    /// A direct child of the item: an attribute, a doc comment or a part of
    /// the signature.
    fn top_node(&mut self, node: &SyntaxNode) {
        match node.kind() {
            SyntaxKind::DOC_COMMENT => {}
            SyntaxKind::ATTR => self.attr(node),
            _ => self.node(node),
        }
    }

    fn attr(&mut self, node: &SyntaxNode) {
        let Some(attr) = ast::Attr::cast(node.clone()) else {
            return;
        };
        let body = attr_body(&attr);
        if body.starts_with("cfg(") {
            self.node(node);
        } else if let Some(list) = body
            .strip_prefix("derive(")
            .and_then(|rest| rest.strip_suffix(')'))
        {
            let mut names: Vec<&str> = split_list(list)
                .into_iter()
                .map(|name| name.rsplit("::").next().unwrap_or(name))
                .collect();
            names.sort_unstable();
            self.tokens.push(format!("#[derive({})]", names.join(",")));
        }
    }

    fn node(&mut self, node: &SyntaxNode) {
        match node.kind() {
            // The pattern of a parameter is not part of the signature.
            SyntaxKind::PARAM => {
                match ast::Param::cast(node.clone()).and_then(|param| param.ty()) {
                    Some(ty) => self.node(ty.syntax()),
                    None => self.children(node),
                }
            }
            // Only the last segment of a path counts.
            SyntaxKind::PATH
                if node
                    .parent()
                    .is_none_or(|parent| parent.kind() != SyntaxKind::PATH) =>
            {
                match ast::Path::cast(node.clone()).and_then(|path| path.segment()) {
                    Some(segment) => self.children(segment.syntax()),
                    None => self.children(node),
                }
            }
            _ => self.children(node),
        }
    }

    fn children(&mut self, node: &SyntaxNode) {
        for child in node.children_with_tokens() {
            match child {
                NodeOrToken::Token(token) => self.token(&token),
                NodeOrToken::Node(inner) => self.node(&inner),
            }
        }
    }

    fn token(&mut self, token: &SyntaxToken) {
        if is_space_or_comment(token.kind()) {
            return;
        }
        let text = token.text();
        let mapped = self.generics.get(text).cloned();
        self.tokens.push(mapped.unwrap_or_else(|| text.to_owned()));
    }
}

/// The tokens without a comma before a closing delimiter or at the end: the
/// comma is layout only.
fn drop_trailing_commas(tokens: &[String]) -> Vec<&str> {
    tokens
        .iter()
        .enumerate()
        .filter(|(index, token)| {
            *token != ","
                || tokens
                    .get(index + 1)
                    .is_some_and(|next| !matches!(next.as_str(), ")" | ">" | "]" | "}"))
        })
        .map(|(_, token)| token.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str, name: &str) -> Fact {
        facts(text, Edition::Edition2024)
            .items
            .into_iter()
            .find(|((_, found), _)| found == name)
            .map(|(_, fact)| fact)
            .unwrap_or_default()
    }

    #[test]
    fn a_function_header_skips_the_body_the_docs_and_the_layout() {
        let text = "/// Doc.\n#[inline]\n#[cfg(unix)]\npub fn f<T: Copy>(\n    a: T,\n) -> T\nwhere\n    T: Default,\n{\n    a\n}\n";
        assert_eq!(
            one(text, "f").header,
            "# [ cfg ( unix ) ] pub fn f < T0 : Copy > ( T0 ) -> T0 where T0 : Default"
        );
    }

    #[test]
    fn a_rename_that_no_caller_sees_is_no_change() {
        let before = "pub fn f<T>(a: u8, x: std::vec::Vec<T>) {}\n#[derive(Debug, Clone)]\npub struct S<'a>(&'a u8);\n";
        let after = "pub fn f<U>(b: u8, y: Vec<U>) {}\n#[derive(Clone, Debug)]\npub struct S<'b>(&'b u8);\n";
        assert_eq!(one(before, "f").header, one(after, "f").header);
        assert_eq!(one(before, "S").header, one(after, "S").header);
        let method = |generic: &str| {
            format!("impl<{generic}> W<{generic}> {{ pub fn m(&self, v: {generic}) {{}} }}\n")
        };
        assert_eq!(one(&method("T"), "m").header, one(&method("Q"), "m").header);
        assert_ne!(
            one("pub fn f(a: u8) {}\n", "f").header,
            one("pub fn f(a: u16) {}\n", "f").header
        );
    }

    #[test]
    fn a_type_header_skips_its_members_and_a_const_skips_its_value() {
        let text = "#[derive(Debug)]\npub struct S<T> { pub a: T }\npub enum E { A, B }\npub const C: u8 = 1 + 2;\npub trait Tr: Copy { fn m(&self); }\n";
        assert_eq!(
            one(text, "S").header,
            "#[derive(Debug)] pub struct S < T0 >"
        );
        assert_eq!(one(text, "E").header, "pub enum E");
        assert_eq!(one(text, "C").header, "pub const C : u8");
        assert_eq!(one(text, "Tr").header, "pub trait Tr : Copy");
        assert_eq!(one(text, "a").header, "pub a : T0");
        assert_eq!(one(text, "m").header, "fn m ( & self )");
    }

    #[test]
    fn doc_hidden_marks_the_item_a_use_and_an_impl() {
        let text = "#[doc(hidden)]\npub fn secret() {}\npub fn open() {}\n#[doc(hidden)]\npub use a::B;\n#[doc(hidden)]\nimpl S {\n    pub fn m() {}\n}\n";
        assert!(one(text, "secret").hidden);
        assert!(!one(text, "open").hidden);
        let lines = facts(text, Edition::Edition2024).hidden_lines;
        assert!(lines.contains(&5) && lines.contains(&7) && !lines.contains(&3));
    }
}
