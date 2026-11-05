//! The normalized token stream of a file.
//!
//! The stream comes from the tokens of the parsed tree, so it is the same
//! stream that the other checks see. Whitespace and comments, also doc
//! comments, are not tokens. With `ignore-imports`, the tokens of `use` items
//! and `extern crate` items are left out.
//!
//! Each token has a `test` flag: true inside an item with a test attribute,
//! for example `#[cfg(test)]` or `#[test]`. `production = true` leaves these
//! tokens out.
//!
//! Each token becomes a hash of its class and, unless the mode blinds the
//! class, its text. `strict` and `mild` keep every text. `weak` blinds string
//! and character literals. `semantic` also blinds names, lifetimes and numbers.
//! Keywords and punctuation are never blinded.

use ra_ap_syntax::{NodeOrToken, SyntaxKind, SyntaxNode, WalkEvent};
use xxhash_rust::xxh3::xxh3_64;

use crate::attrs::{AttrClass, attributes_of};
use crate::structure::config::Mode;
use crate::structure::source::{LineIndex, is_space_or_comment};

/// One token of the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    /// The hash of the normalized token.
    pub hash: u64,
    /// The first line. It starts at 1.
    pub line: u32,
    /// The first column. It starts at 1.
    pub column: u32,
    /// The last line.
    pub end_line: u32,
    /// The column after the last character.
    pub end_column: u32,
    /// True inside an item with a test attribute.
    pub test: bool,
}

/// True for a node with a test attribute, for example `#[cfg(test)]`,
/// `#[test]` or `#[cfg_attr(test, test)]`.
pub fn is_test_item(node: &SyntaxNode) -> bool {
    attributes_of(node) == AttrClass::Test
}

/// The number of lines with a part of a test item below `root`. A line counts
/// once, also when two test items share it.
pub fn test_lines(root: &SyntaxNode, index: &LineIndex<'_>) -> u32 {
    let mut ranges = Vec::new();
    let mut walk = root.preorder();
    while let Some(event) = walk.next() {
        let WalkEvent::Enter(node) = event else {
            continue;
        };
        if is_test_item(&node) {
            let range = node.text_range();
            ranges.push((
                index.position(range.start()).0,
                index.position(range.end()).0,
            ));
            walk.skip_subtree();
        }
    }
    ranges.sort_unstable();
    let (mut lines, mut covered) = (0, 0);
    for (first, last) in ranges {
        let first = first.max(covered + 1);
        if last >= first {
            lines += last - first + 1;
            covered = last;
        }
    }
    lines
}

/// The class of a token for the normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Name,
    Text,
    Number,
    Other,
}

const fn class(kind: SyntaxKind) -> Class {
    match kind {
        SyntaxKind::IDENT | SyntaxKind::LIFETIME_IDENT => Class::Name,
        SyntaxKind::STRING
        | SyntaxKind::BYTE_STRING
        | SyntaxKind::C_STRING
        | SyntaxKind::CHAR
        | SyntaxKind::BYTE => Class::Text,
        SyntaxKind::INT_NUMBER | SyntaxKind::FLOAT_NUMBER => Class::Number,
        _ => Class::Other,
    }
}

const fn blinds(mode: Mode, class: Class) -> bool {
    matches!(
        (mode, class),
        (Mode::Weak, Class::Text) | (Mode::Semantic, Class::Name | Class::Text | Class::Number)
    )
}

/// The hash of one token in `mode`.
fn token_hash(mode: Mode, kind: SyntaxKind, text: &str) -> u64 {
    let class = class(kind);
    let tag: u8 = match class {
        Class::Name => 1,
        Class::Text => 2,
        Class::Number => 3,
        Class::Other => 4,
    };
    let mut bytes = Vec::with_capacity(text.len() + 1);
    bytes.push(tag);
    if !blinds(mode, class) {
        bytes.extend_from_slice(text.as_bytes());
    }
    xxh3_64(&bytes)
}

/// The tokens of the tree below `root`.
pub fn tokens(
    root: &SyntaxNode,
    index: &LineIndex<'_>,
    mode: Mode,
    ignore_imports: bool,
) -> Vec<Token> {
    let mut found = Vec::new();
    // The test items around the walk, outermost first.
    let mut tests: Vec<SyntaxNode> = Vec::new();
    let mut walk = root.preorder_with_tokens();
    while let Some(event) = walk.next() {
        let element = match event {
            WalkEvent::Enter(element) => element,
            WalkEvent::Leave(NodeOrToken::Node(node)) => {
                if tests.last() == Some(&node) {
                    tests.pop();
                }
                continue;
            }
            WalkEvent::Leave(NodeOrToken::Token(_)) => continue,
        };
        match element {
            NodeOrToken::Node(node) => {
                if ignore_imports
                    && matches!(node.kind(), SyntaxKind::USE | SyntaxKind::EXTERN_CRATE)
                {
                    walk.skip_subtree();
                } else if is_test_item(&node) {
                    tests.push(node);
                }
            }
            NodeOrToken::Token(token) => {
                if is_space_or_comment(token.kind()) {
                    continue;
                }
                let range = token.text_range();
                let (line, column) = index.position(range.start());
                let (end_line, end_column) = index.position(range.end());
                found.push(Token {
                    hash: token_hash(mode, token.kind(), token.text()),
                    line,
                    column,
                    end_line,
                    end_column,
                    test: !tests.is_empty(),
                });
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use ra_ap_syntax::AstNode;

    use super::*;
    use crate::parse::{Edition, with_parsed};

    fn hashes(source: &str, mode: Mode, ignore_imports: bool) -> Vec<u64> {
        with_parsed(source, Edition::Edition2024, |file, text| {
            tokens(file.syntax(), &LineIndex::new(text), mode, ignore_imports)
                .into_iter()
                .map(|token| token.hash)
                .collect()
        })
        .unwrap_or_default()
    }

    /// The number of test tokens, all tokens and the test lines of `source`.
    fn test_parts(source: &str) -> (usize, usize, u32) {
        with_parsed(source, Edition::Edition2024, |file, text| {
            let index = LineIndex::new(text);
            let found = tokens(file.syntax(), &index, Mode::Mild, true);
            (
                found.iter().filter(|token| token.test).count(),
                found.len(),
                test_lines(file.syntax(), &index),
            )
        })
        .unwrap_or_default()
    }

    #[test]
    fn only_a_test_attribute_makes_test_tokens() {
        for production in [
            "#[cfg(not(test))]\nfn a() {}\n",
            "#[cfg(any(test, feature = \"x\"))]\nfn a() {}\n",
            "#[cfg(feature = \"x\")]\nfn a() {}\n",
        ] {
            assert_eq!(test_parts(production).0, 0, "{production}");
        }
        for test in [
            "#[cfg(all(test, unix))]\nfn a() {}\n",
            "#[cfg_attr(test, test)]\nfn a() {}\n",
            "#[tokio::test]\nasync fn a() {}\n",
        ] {
            let (tests, all, lines) = test_parts(test);
            assert_eq!((tests, lines), (all, 2), "{test}");
        }
    }

    #[test]
    fn an_inner_test_attribute_makes_each_token_a_test_token() {
        let (tests, all, _) = test_parts("#![cfg(test)]\nfn a() {}\n");
        assert_eq!(tests, all);
    }

    #[test]
    fn two_test_items_on_one_line_count_the_line_once() {
        let source = "fn p() {}\n#[test] fn a() {} #[test] fn b() {}\nfn q() {}\n";
        assert_eq!(test_parts(source).2, 1);
    }

    #[test]
    fn comments_and_whitespace_are_not_tokens() {
        let plain = hashes("fn a() { b(1) }", Mode::Mild, true);
        let spaced = hashes("/// doc\nfn a ( ) {\n  // c\n  b( 1 )\n}", Mode::Mild, true);
        assert_eq!(plain, spaced);
        assert_eq!(plain.len(), 10);
    }

    #[test]
    fn imports_are_left_out_only_with_the_setting() {
        assert_eq!(hashes("use a::b;", Mode::Mild, true).len(), 0);
        assert_eq!(hashes("extern crate a;", Mode::Mild, true).len(), 0);
        assert_eq!(hashes("use a::b;", Mode::Mild, false).len(), 5);
    }

    #[test]
    fn each_mode_blinds_its_classes() {
        let a = "fn a() { b(\"x\", 1) }";
        let b = "fn c() { d(\"y\", 2) }";
        assert_ne!(hashes(a, Mode::Mild, true), hashes(b, Mode::Mild, true));
        assert_ne!(hashes(a, Mode::Weak, true), hashes(b, Mode::Weak, true));
        assert_eq!(
            hashes(a, Mode::Semantic, true),
            hashes(b, Mode::Semantic, true)
        );
        let c = "fn a() { b(\"y\", 1) }";
        assert_eq!(hashes(a, Mode::Weak, true), hashes(c, Mode::Weak, true));
        assert_ne!(hashes(a, Mode::Strict, true), hashes(c, Mode::Strict, true));
    }

    #[test]
    fn keywords_stay_visible_in_semantic_mode() {
        let a = "fn a() -> bool { true }";
        let b = "fn a() -> bool { false }";
        assert_ne!(
            hashes(a, Mode::Semantic, true),
            hashes(b, Mode::Semantic, true)
        );
    }
}
