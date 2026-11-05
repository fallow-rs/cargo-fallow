//! The header of each named item of a file: the tokens that a user of the
//! item can see, without the body.
//!
//! A header keeps the `cfg` and `derive` attributes, the visibility, the
//! signature, the generics and the `where` clause. It leaves out the doc
//! comments, the other attributes, a function body, the value of a `const` or
//! a `static`, and the member lists of a type, a trait and a module: each
//! member has its own header. A header is one line of tokens with one space
//! between two tokens, so a change of the layout is no change.

use ra_ap_syntax::ast::{self, HasAttrs, HasName};
use ra_ap_syntax::{AstNode, NodeOrToken, SyntaxKind, SyntaxNode};

use crate::attrs::attr_body;
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

/// The facts of the items of one file, by the line of the name and the name.
pub type Facts = rustc_hash::FxHashMap<(u32, String), Fact>;

/// Reads the facts of each named item of `text`. A file that does not parse
/// gives no facts.
pub fn facts(text: &str, edition: Edition) -> Facts {
    with_parsed(text, edition, |file, normalized| {
        let index = LineIndex::new(normalized);
        let mut found = Facts::default();
        for node in file.syntax().descendants() {
            let Some(named) = ast::AnyHasName::cast(node.clone()) else {
                continue;
            };
            let Some(name) = named.name() else {
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
            found.insert((line, crate::attrs::plain_name(&name)), fact);
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
    let mut tokens: Vec<String> = Vec::new();
    let mut after_eq = false;
    for child in node.children_with_tokens() {
        match child {
            NodeOrToken::Token(token) => {
                if is_space_or_comment(token.kind()) {
                    continue;
                }
                if token.kind() == SyntaxKind::EQ && skip.contains(&EXPRESSION) {
                    after_eq = true;
                    continue;
                }
                if token.kind() == SyntaxKind::SEMICOLON {
                    continue;
                }
                tokens.push(token.text().to_owned());
            }
            NodeOrToken::Node(inner) => {
                if skip.contains(&inner.kind()) || after_eq {
                    continue;
                }
                match inner.kind() {
                    SyntaxKind::DOC_COMMENT => {}
                    SyntaxKind::ATTR => {
                        let kept = ast::Attr::cast(inner.clone()).is_some_and(|attr| {
                            let body = attr_body(&attr);
                            body.starts_with("cfg(") || body.starts_with("derive(")
                        });
                        if kept {
                            push_tokens(&inner, &mut tokens);
                        }
                    }
                    _ => push_tokens(&inner, &mut tokens),
                }
            }
        }
    }
    drop_trailing_commas(&tokens).join(" ")
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

fn push_tokens(node: &SyntaxNode, tokens: &mut Vec<String>) {
    for token in node
        .descendants_with_tokens()
        .filter_map(NodeOrToken::into_token)
    {
        if !is_space_or_comment(token.kind()) {
            tokens.push(token.text().to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str, name: &str) -> Fact {
        facts(text, Edition::Edition2024)
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
            "# [ cfg ( unix ) ] pub fn f < T : Copy > ( a : T ) -> T where T : Default"
        );
    }

    #[test]
    fn a_type_header_skips_its_members_and_a_const_skips_its_value() {
        let text = "#[derive(Debug)]\npub struct S<T> { pub a: T }\npub enum E { A, B }\npub const C: u8 = 1 + 2;\npub trait Tr: Copy { fn m(&self); }\n";
        assert_eq!(
            one(text, "S").header,
            "# [ derive ( Debug ) ] pub struct S < T >"
        );
        assert_eq!(one(text, "E").header, "pub enum E");
        assert_eq!(one(text, "C").header, "pub const C : u8");
        assert_eq!(one(text, "Tr").header, "pub trait Tr : Copy");
        assert_eq!(one(text, "a").header, "pub a : T");
        assert_eq!(one(text, "m").header, "fn m ( & self )");
    }

    #[test]
    fn doc_hidden_marks_the_item() {
        let text = "#[doc(hidden)]\npub fn secret() {}\npub fn open() {}\n";
        assert!(one(text, "secret").hidden);
        assert!(!one(text, "open").hidden);
    }
}
