//! Finds the mutation sites of one source file.
//!
//! The operators are type-safe by construction, because each replacement
//! uses the same trait or the same `bool` operands as the original. The module
//! skips each place where a runtime switch cannot compile or where a mutant has
//! no use: constant contexts, patterns, attributes and test code. The parser
//! leaves macro bodies and macro arguments as token trees, so they contain no
//! expression nodes and need no skip rule.

use std::collections::BTreeMap;
use std::ops::Range;

use ra_ap_syntax::ast::{self, HasAttrs, HasName};
use ra_ap_syntax::{AstNode, NodeOrToken, SourceFile, SyntaxKind, SyntaxNode, SyntaxToken};

use crate::mutation::id::{IdInput, mutant_id};
use crate::parse::{Edition, ParseOutcome, with_parsed};

/// The shape of a mutation site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// A binary expression. The mutants replace the operator.
    Binary {
        /// The original offset where the left operand ends.
        lhs_end: usize,
        /// The original offset where the right operand starts.
        rhs_start: usize,
    },
    /// The condition of an `if` or a `while`. The mutant negates it.
    Condition,
}

/// One mutant of a site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mutant {
    /// The stable id. It does not change when unrelated code moves.
    pub id: u64,
    /// The replacement operator, or [`NEGATE`].
    pub replacement: &'static str,
    /// The operator name of the report.
    pub operator: &'static str,
}

/// The replacement text of a negated condition.
pub const NEGATE: &str = "!";

/// One place in a source file with one or more mutants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    /// The whole expression, as a byte range of the original source.
    pub range: Range<usize>,
    /// The shape of the site.
    pub shape: Shape,
    /// The mutants of the site.
    pub mutants: Vec<Mutant>,
    /// True when a `cfg` attribute other than `cfg(test)` is on an enclosing
    /// item. Then the build can skip the site, and no compile checks it.
    pub under_cfg: bool,
}

/// The number of operators that the walk leaves out, by reason.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Skipped {
    /// A constant context or a pattern.
    pub const_context: u32,
    /// Test code.
    pub test_code: u32,
    /// A `let` chain.
    pub let_chain: u32,
    /// Operator tokens in macro arguments and macro bodies, outside test code.
    pub macro_tokens: u32,
}

/// The result of the walk over one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileSites {
    /// The sites, sorted by start offset, outer sites first.
    pub sites: Vec<Site>,
    /// The operators that the walk left out.
    pub skipped: Skipped,
    /// The modules that the file declares with `mod name;`.
    pub modules: Vec<ModuleDecl>,
    /// The original offset after the last inner attribute of the file.
    pub after_inner_attrs: usize,
}

/// A `mod name;` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleDecl {
    /// The module name, without a `r#` prefix.
    pub name: String,
    /// The names of the enclosing inline modules, outermost first.
    pub inline_path: Vec<String>,
    /// The value of a `#[path]` attribute.
    pub path_attr: Option<String>,
    /// True when a `cfg` attribute other than `cfg(test)` is on the module.
    pub under_cfg: bool,
}

/// Finds the sites of one file.
///
/// `file_key` is part of each mutant id: use a path relative to the workspace
/// root, so the id does not depend on the checkout.
pub fn find_sites(
    source: &str,
    edition: Edition,
    file_key: &str,
) -> Result<FileSites, ParseOutcome> {
    let offsets = OffsetMap::new(source);
    with_parsed(source, edition, |file, text| {
        let mut walk = Walk {
            offsets: &offsets,
            found: FileSites::default(),
            pending: Vec::new(),
        };
        walk.file(file);
        walk.finish(text, file_key)
    })
}

/// Maps an offset in the text with LF line endings to an offset in the
/// original source.
struct OffsetMap {
    /// The normalized offset of the line feed of each CRLF.
    crlf: Vec<usize>,
}

impl OffsetMap {
    fn new(original: &str) -> Self {
        let crlf = original
            .match_indices("\r\n")
            .enumerate()
            .map(|(removed, (offset, _))| offset - removed)
            .collect();
        Self { crlf }
    }

    fn original(&self, normalized: usize) -> usize {
        normalized
            + self
                .crlf
                .partition_point(|&line_feed| line_feed < normalized)
    }
}

/// A site before the sort and before its mutants have ids.
struct Pending {
    /// The range in the normalized text.
    range: Range<usize>,
    /// The operands, in normalized offsets, or `None` for a condition.
    operands: Option<(usize, usize)>,
    replacements: &'static [&'static str],
    under_cfg: bool,
}

struct Walk<'a> {
    offsets: &'a OffsetMap,
    found: FileSites,
    pending: Vec<Pending>,
}

impl Walk<'_> {
    fn file(&mut self, file: &SourceFile) {
        if let Some(end) = file
            .attrs()
            .filter(|attr| attr.excl_token().is_some())
            .map(|attr| usize::from(attr.syntax().text_range().end()))
            .max()
        {
            self.found.after_inner_attrs = self.offsets.original(end);
        }
        for element in file.syntax().descendants_with_tokens() {
            match element {
                NodeOrToken::Node(node) => self.node(&node),
                NodeOrToken::Token(token) => self.macro_token(&token),
            }
        }
    }

    /// Counts an operator in a token tree. A token tree holds the single
    /// characters of an operator as separate tokens.
    fn macro_token(&mut self, token: &SyntaxToken) {
        let Some(parent) = token.parent() else {
            return;
        };
        let is_operator = parent.kind() == SyntaxKind::TOKEN_TREE
            && token
                .next_token()
                .is_some_and(|next| starts_operator(token.kind(), next.kind()));
        if is_operator && matches!(context_of(&parent), Context::Mutable { .. }) {
            self.found.skipped.macro_tokens += 1;
        }
    }

    fn node(&mut self, node: &SyntaxNode) {
        if let Some(binary) = ast::BinExpr::cast(node.clone()) {
            self.binary(&binary);
        } else if let Some(if_expr) = ast::IfExpr::cast(node.clone()) {
            if let Some(condition) = if_expr.condition() {
                self.condition(&condition);
            }
        } else if let Some(while_expr) = ast::WhileExpr::cast(node.clone()) {
            if let Some(condition) = while_expr.condition() {
                self.condition(&condition);
            }
        } else if let Some(module) = ast::Module::cast(node.clone()) {
            self.module(&module);
        }
    }

    fn binary(&mut self, binary: &ast::BinExpr) {
        let Some(token) = binary.op_token() else {
            return;
        };
        let Some(replacements) = swaps(token.kind()) else {
            return;
        };
        let (Some(lhs), Some(rhs)) = (binary.lhs(), binary.rhs()) else {
            return;
        };
        let Some(under_cfg) = self.allowed(binary.syntax()) else {
            return;
        };
        if matches!(token.kind(), SyntaxKind::AMP2 | SyntaxKind::PIPE2)
            && (has_chain_let(&lhs) || has_chain_let(&rhs))
        {
            self.found.skipped.let_chain += 1;
            return;
        }
        let operands = (
            usize::from(lhs.syntax().text_range().end()),
            usize::from(rhs.syntax().text_range().start()),
        );
        self.pending.push(Pending {
            range: range_of(binary.syntax()),
            operands: Some(operands),
            replacements,
            under_cfg,
        });
    }

    fn condition(&mut self, condition: &ast::Expr) {
        let Some(under_cfg) = self.allowed(condition.syntax()) else {
            return;
        };
        // `!(a == b)` is the mutant `a != b`, and `!(a < b)` is the mutant
        // `a >= b` for a total order. A second copy adds a run and inflates the
        // score.
        if is_comparison(condition) {
            return;
        }
        if has_chain_let(condition) {
            self.found.skipped.let_chain += 1;
            return;
        }
        self.pending.push(Pending {
            range: range_of(condition.syntax()),
            operands: None,
            replacements: &[NEGATE],
            under_cfg,
        });
    }

    /// The `under_cfg` flag of a node that can hold a site, or `None` after a
    /// count of the skip reason.
    fn allowed(&mut self, node: &SyntaxNode) -> Option<bool> {
        match context_of(node) {
            Context::Mutable { under_cfg } => Some(under_cfg),
            Context::Constant => {
                self.found.skipped.const_context += 1;
                None
            }
            Context::Test => {
                self.found.skipped.test_code += 1;
                None
            }
        }
    }

    fn module(&mut self, module: &ast::Module) {
        if module.semicolon_token().is_none() {
            return;
        }
        let Context::Mutable { under_cfg } = context_of(module.syntax()) else {
            return;
        };
        let Some(name) = module.name() else {
            return;
        };
        let mut inline_path: Vec<String> = module
            .syntax()
            .ancestors()
            .skip(1)
            .filter_map(ast::Module::cast)
            .filter_map(|outer| outer.name())
            .map(|outer| plain_name(&outer))
            .collect();
        inline_path.reverse();
        self.found.modules.push(ModuleDecl {
            name: plain_name(&name),
            inline_path,
            path_attr: module.attrs().find_map(|attr| path_attribute(&attr)),
            under_cfg,
        });
    }

    fn finish(mut self, text: &str, file_key: &str) -> FileSites {
        // Two sites with one range are a condition and its binary expression.
        // The condition is the outer one, because its arm negates the whole.
        self.pending.sort_by_key(|site| {
            (
                site.range.start,
                std::cmp::Reverse(site.range.end),
                site.operands.is_some(),
            )
        });
        let mut occurrences: BTreeMap<&str, u32> = BTreeMap::new();
        for site in &self.pending {
            let site_text = text.get(site.range.clone()).unwrap_or_default();
            let occurrence = occurrences.entry(site_text).or_default();
            let mutants = site
                .replacements
                .iter()
                .map(|&replacement| {
                    let operator = operator_name(replacement);
                    Mutant {
                        id: mutant_id(IdInput {
                            file: file_key,
                            site_text,
                            occurrence: *occurrence,
                            operator,
                            replacement,
                        }),
                        replacement,
                        operator,
                    }
                })
                .collect();
            *occurrence += 1;
            self.found.sites.push(Site {
                range: self.offsets.original(site.range.start)
                    ..self.offsets.original(site.range.end),
                shape: site
                    .operands
                    .map_or(Shape::Condition, |(lhs_end, rhs_start)| Shape::Binary {
                        lhs_end: self.offsets.original(lhs_end),
                        rhs_start: self.offsets.original(rhs_start),
                    }),
                mutants,
                under_cfg: site.under_cfg,
            });
        }
        self.found
    }
}

fn range_of(node: &SyntaxNode) -> Range<usize> {
    let range = node.text_range();
    usize::from(range.start())..usize::from(range.end())
}

/// The replacements of a binary operator, or `None` when it has no mutant.
const fn swaps(kind: SyntaxKind) -> Option<&'static [&'static str]> {
    Some(match kind {
        SyntaxKind::AMP2 => &["||"],
        SyntaxKind::PIPE2 => &["&&"],
        SyntaxKind::EQ2 => &["!="],
        SyntaxKind::NEQ => &["=="],
        SyntaxKind::L_ANGLE => &["<=", ">="],
        SyntaxKind::LTEQ => &["<", ">"],
        SyntaxKind::R_ANGLE => &[">=", "<="],
        SyntaxKind::GTEQ => &[">", "<"],
        _ => return None,
    })
}

/// True for the two characters of an operator with a mutant. A lone `<` or `>`
/// in a token tree can be a generic bracket, so it does not count.
const fn starts_operator(first: SyntaxKind, second: SyntaxKind) -> bool {
    matches!(
        (first, second),
        (SyntaxKind::AMP, SyntaxKind::AMP)
            | (SyntaxKind::PIPE, SyntaxKind::PIPE)
            | (
                SyntaxKind::EQ | SyntaxKind::BANG | SyntaxKind::L_ANGLE | SyntaxKind::R_ANGLE,
                SyntaxKind::EQ
            )
    )
}

/// True when the expression is a `let`, or a chain with a `let` at its top.
fn has_chain_let(expr: &ast::Expr) -> bool {
    match expr {
        ast::Expr::LetExpr(_) => true,
        ast::Expr::BinExpr(binary) => {
            binary.lhs().is_some_and(|lhs| has_chain_let(&lhs))
                || binary.rhs().is_some_and(|rhs| has_chain_let(&rhs))
        }
        ast::Expr::ParenExpr(paren) => paren.expr().is_some_and(|inner| has_chain_let(&inner)),
        _ => false,
    }
}

/// What the enclosing nodes allow for a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Context {
    /// A site can go here.
    Mutable { under_cfg: bool },
    /// The compiler evaluates this code, or the code is not an expression.
    Constant,
    /// Test code, compiled only for tests.
    Test,
}

/// Reads the enclosing nodes of `node`, and `node` itself.
fn context_of(node: &SyntaxNode) -> Context {
    let (mut constant, mut under_cfg) = (false, false);
    let mut child: Option<SyntaxNode> = None;
    for ancestor in node.ancestors() {
        match attributes_of(&ancestor) {
            AttrClass::Test => return Context::Test,
            AttrClass::Cfg => under_cfg = true,
            AttrClass::Other => {}
        }
        constant = constant || is_constant_node(&ancestor, child.as_ref());
        child = Some(ancestor);
    }
    if constant {
        Context::Constant
    } else {
        Context::Mutable { under_cfg }
    }
}

fn is_constant_node(node: &SyntaxNode, child: Option<&SyntaxNode>) -> bool {
    let kind = node.kind();
    if ast::Pat::can_cast(kind) {
        return true;
    }
    match kind {
        SyntaxKind::CONST
        | SyntaxKind::STATIC
        | SyntaxKind::CONST_ARG
        | SyntaxKind::CONST_PARAM
        | SyntaxKind::ARRAY_TYPE
        | SyntaxKind::VARIANT
        | SyntaxKind::ATTR => true,
        SyntaxKind::FN => ast::Fn::cast(node.clone()).is_some_and(|f| f.const_token().is_some()),
        SyntaxKind::BLOCK_EXPR => {
            ast::BlockExpr::cast(node.clone()).is_some_and(|b| b.const_token().is_some())
        }
        SyntaxKind::ARRAY_EXPR => {
            ast::ArrayExpr::cast(node.clone()).is_some_and(|array| in_repeat_length(&array, child))
        }
        _ => false,
    }
}

/// True when `child` is the length of a `[value; length]` expression.
fn in_repeat_length(array: &ast::ArrayExpr, child: Option<&SyntaxNode>) -> bool {
    let Some(child) = child else {
        return false;
    };
    array.semicolon_token().is_some()
        && array
            .exprs()
            .nth(1)
            .is_some_and(|length| length.syntax() == child)
}

/// The strongest class of the attributes on one node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttrClass {
    Test,
    Cfg,
    Other,
}

fn attributes_of(node: &SyntaxNode) -> AttrClass {
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
fn attr_body(attr: &ast::Attr) -> String {
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

fn classify(body: &str) -> AttrClass {
    if let Some(condition) = body.strip_prefix("cfg(") {
        let condition = condition.strip_suffix(')').unwrap_or(condition);
        let only_tests = condition == "test"
            || (condition.starts_with("all(")
                && has_word(condition, "test")
                && !has_word(condition, "not"));
        return if only_tests {
            AttrClass::Test
        } else {
            AttrClass::Cfg
        };
    }
    if body == "test" || body == "bench" || body.ends_with("::test") {
        return AttrClass::Test;
    }
    AttrClass::Other
}

fn has_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|part| part == word)
}

fn path_attribute(attr: &ast::Attr) -> Option<String> {
    let body = attr_body(attr);
    let value = body.strip_prefix("path=\"")?.strip_suffix('"')?;
    Some(value.to_owned())
}

fn plain_name(name: &ast::Name) -> String {
    let text = name.syntax().text().to_string();
    text.strip_prefix("r#")
        .map_or_else(|| text.clone(), str::to_owned)
}

/// The operator name of a replacement.
fn operator_name(replacement: &str) -> &'static str {
    match replacement {
        NEGATE => "negate-condition",
        "&&" | "||" => "logical-operator",
        _ => "relational-operator",
    }
}

/// True when the expression is a comparison, also inside parentheses.
fn is_comparison(expr: &ast::Expr) -> bool {
    match expr {
        ast::Expr::BinExpr(binary) => binary.op_token().is_some_and(|token| {
            matches!(
                token.kind(),
                SyntaxKind::EQ2
                    | SyntaxKind::NEQ
                    | SyntaxKind::L_ANGLE
                    | SyntaxKind::LTEQ
                    | SyntaxKind::R_ANGLE
                    | SyntaxKind::GTEQ
            )
        }),
        ast::Expr::ParenExpr(paren) => paren.expr().is_some_and(|inner| is_comparison(&inner)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDITION: Edition = Edition::Edition2021;

    fn file(source: &str) -> FileSites {
        match find_sites(source, EDITION, "src/lib.rs") {
            Ok(found) => found,
            Err(outcome) => panic!("the source must parse: {outcome:?}"),
        }
    }

    /// The text of each site with its replacements.
    fn found(source: &str) -> Vec<(String, Vec<&'static str>)> {
        file(source)
            .sites
            .iter()
            .map(|site| {
                (
                    source[site.range.clone()].to_owned(),
                    site.mutants.iter().map(|m| m.replacement).collect(),
                )
            })
            .collect()
    }

    fn in_fn(body: &str) -> String {
        format!("fn f(a: bool, b: bool) -> bool {{ {body} }}")
    }

    fn assert_no_sites(source: &str) {
        assert_eq!(found(source), Vec::new(), "{source}");
    }

    #[test]
    fn each_operator_has_its_swaps() {
        for (op, swaps) in [
            ("&&", vec!["||"]),
            ("||", vec!["&&"]),
            ("==", vec!["!="]),
            ("!=", vec!["=="]),
            ("<", vec!["<=", ">="]),
            ("<=", vec!["<", ">"]),
            (">", vec![">=", "<="]),
            (">=", vec![">", "<"]),
        ] {
            let expr = format!("a {op} b");
            let source = in_fn(&expr);
            assert_eq!(found(&source), vec![(expr, swaps)], "{op}");
        }
    }

    #[test]
    fn other_binary_operators_have_no_site() {
        assert_no_sites("fn f(a: u8, b: u8) -> u8 { a + b - a * b / 2 % 3 ^ 1 | 2 & 3 << 1 >> 2 }");
        assert_no_sites("fn f(mut a: u8) { a = 1; a += 1; a >>= 1; }");
    }

    #[test]
    fn if_and_while_conditions_are_negated() {
        assert_eq!(
            found("fn f(a: bool) { if a {} while a {} }"),
            vec![
                ("a".to_owned(), vec![NEGATE]),
                ("a".to_owned(), vec![NEGATE])
            ]
        );
    }

    #[test]
    fn condition_comes_before_the_binary_site_at_the_same_range() {
        assert_eq!(
            found("fn f(a: bool, b: bool) { if a && b {} }"),
            vec![
                ("a && b".to_owned(), vec![NEGATE]),
                ("a && b".to_owned(), vec!["||"])
            ]
        );
    }

    #[test]
    fn negation_of_a_comparison_is_not_a_second_mutant() {
        for source in [
            "fn f(a: u8, b: u8) { if a == b {} }",
            "fn f(a: u8, b: u8) { if (a == b) {} }",
            "fn f(a: u8, b: u8) { while a < b {} }",
        ] {
            let sites = found(source);
            assert_eq!(sites.len(), 1, "{source}");
            assert!(!sites[0].1.contains(&NEGATE), "{source}");
        }
    }

    #[test]
    fn nested_sites_are_sorted_outer_first() {
        let names: Vec<String> = found(&in_fn("a == b && b != a"))
            .into_iter()
            .map(|(text, _)| text)
            .collect();
        assert_eq!(names, ["a == b && b != a", "a == b", "b != a"]);
    }

    #[test]
    fn constant_contexts_have_no_site() {
        for source in [
            "const C: bool = 1 == 2;",
            "static S: bool = 1 < 2;",
            "const fn f(a: bool) -> bool { if a { a && a } else { a } }",
            "fn f() -> bool { const { 1 == 2 } }",
            "fn f() { let _ = [0_u8; if 1 == 2 { 1 } else { 2 }]; }",
            "type T = [u8; (1 < 2) as usize];",
            "enum E { A = (1 == 2) as isize }",
            "struct G<const B: bool>; type H = G<{ 1 == 2 }>;",
            "struct D<const B: bool = { 1 == 2 }>;",
            "impl X { const C: bool = 1 == 2; }",
        ] {
            assert_no_sites(source);
            assert!(file(source).skipped.const_context > 0, "{source}");
        }
    }

    #[test]
    fn repeat_value_is_a_site_but_its_length_is_not() {
        assert_eq!(
            found("fn f(a: u8) { let _ = [a == 1; 1 + 2 * (3 > 2) as usize]; }"),
            vec![("a == 1".to_owned(), vec!["!="])]
        );
    }

    #[test]
    fn macro_arguments_and_bodies_have_no_site_and_are_counted() {
        let source = "macro_rules! m { () => { 1 == 2 } }\n\
                      fn f(a: u8) { assert!(a == 1 && a != 2); }";
        assert_no_sites(source);
        assert_eq!(file(source).skipped.macro_tokens, 4);
    }

    #[test]
    fn attribute_arguments_have_no_site() {
        assert_no_sites("#[doc = a == b] fn f() {}");
    }

    #[test]
    fn test_code_has_no_site() {
        for source in [
            "#[test] fn t() { let _ = 1 == 2; }",
            "#[tokio::test] async fn t() { let _ = 1 == 2; }",
            "#[cfg(test)] mod tests { fn g() { let _ = 1 == 2; } }",
            "#[cfg(all(test, unix))] fn g() { let _ = 1 == 2; }",
        ] {
            assert_no_sites(source);
            assert_eq!(file(source).skipped.test_code, 1, "{source}");
        }
    }

    #[test]
    fn cfg_not_test_is_a_site_under_cfg() {
        let found = file("#[cfg(not(test))] fn g() -> bool { 1 == 2 }");
        assert_eq!(found.sites.len(), 1);
        assert!(found.sites[0].under_cfg);
    }

    #[test]
    fn plain_code_is_not_under_cfg() {
        assert!(!file(&in_fn("a == b")).sites[0].under_cfg);
    }

    #[test]
    fn let_chain_keeps_comparisons_but_not_its_own_operators() {
        let source = "fn f(a: Option<u8>) { if let Some(x) = a && x == 1 {} }";
        assert_eq!(found(source), vec![("x == 1".to_owned(), vec!["!="])]);
        assert_eq!(file(source).skipped.let_chain, 2);
    }

    #[test]
    fn if_let_condition_is_not_negated() {
        assert_no_sites("fn f(a: Option<u8>) { if let Some(_) = a {} while let Some(_) = a {} }");
    }

    #[test]
    fn let_inside_a_nested_block_does_not_stop_the_condition() {
        let source = "fn f(a: Option<u8>) { if { if let Some(_) = a { true } else { false } } {} }";
        let texts: Vec<String> = found(source).into_iter().map(|(text, _)| text).collect();
        assert_eq!(texts.len(), 1);
        assert!(texts[0].starts_with("{ if let"));
    }

    #[test]
    fn ids_do_not_change_when_unrelated_code_moves() {
        let ids = |source: &str| -> Vec<u64> {
            file(source)
                .sites
                .iter()
                .flat_map(|site| site.mutants.iter().map(|m| m.id))
                .collect()
        };
        let first = ids("fn f(a: u8) -> bool { a == 1 }");
        let moved = ids("fn g() {}\n\nfn f(a: u8) -> bool { a  ==  1 }\n");
        assert_eq!(first.len(), 1);
        assert_ne!(first[0], 0);
        assert_eq!(ids("fn g() {}\nfn f(a: u8) -> bool { a == 1 }"), first);
        assert_ne!(moved, first, "the id reads the site text");
    }

    #[test]
    fn ids_differ_by_file_by_replacement_and_by_occurrence() {
        let source = "fn f(a: u8) -> (bool, bool) { (a < 1, a < 1) }";
        let sites = file(source).sites;
        let ids: Vec<u64> = sites
            .iter()
            .flat_map(|site| site.mutants.iter().map(|m| m.id))
            .collect();
        assert_eq!(ids.len(), 4);
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 4);
        let Ok(other) = find_sites(source, EDITION, "src/other.rs") else {
            panic!("the source parses");
        };
        assert_ne!(other.sites[0].mutants[0].id, ids[0]);
    }

    #[test]
    fn crlf_offsets_map_back_to_the_original_bytes() {
        let source = "fn f(a: u8) -> bool {\r\n    let _ = \"x\r\ny\";\r\n    a == 1\r\n}\r\n";
        let sites = file(source).sites;
        assert_eq!(&source[sites[0].range.clone()], "a == 1");
        let Shape::Binary { lhs_end, rhs_start } = sites[0].shape else {
            panic!("a binary site");
        };
        assert_eq!(&source[lhs_end..rhs_start], " == ");
    }

    #[test]
    fn crlf_and_lf_give_the_same_ids() {
        let lf = file("fn f(a: u8) -> bool {\n    a\n    == 1\n}\n");
        let crlf = file("fn f(a: u8) -> bool {\r\n    a\r\n    == 1\r\n}\r\n");
        assert_eq!(lf.sites[0].mutants, crlf.sites[0].mutants);
    }

    #[test]
    fn parse_error_is_returned() {
        assert!(matches!(
            find_sites("fn f( {", EDITION, "src/lib.rs"),
            Err(ParseOutcome::Failed { .. })
        ));
    }

    #[test]
    fn outlined_modules_are_listed() {
        let source = "mod a;\n#[cfg(unix)] mod r#try;\n#[path = \"x/y.rs\"] mod z;\n\
                      #[cfg(test)] mod tests;\nmod inline { mod nested; }\nmod other {}\n";
        let modules = file(source).modules;
        let names: Vec<(&str, bool, Option<&str>, usize)> = modules
            .iter()
            .map(|m| {
                (
                    m.name.as_str(),
                    m.under_cfg,
                    m.path_attr.as_deref(),
                    m.inline_path.len(),
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                ("a", false, None, 0),
                ("try", true, None, 0),
                ("z", false, Some("x/y.rs"), 0),
                ("nested", false, None, 1),
            ]
        );
    }

    #[test]
    fn insertion_point_follows_the_last_inner_attribute() {
        let source = "//! Docs.\n#![no_std]\n#![deny(warnings)]\n\nfn f() {}\n";
        assert_eq!(&source[file(source).after_inner_attrs..], "\n\nfn f() {}\n");
        assert_eq!(file("fn f() {}").after_inner_attrs, 0);
    }

    #[test]
    fn insertion_point_maps_back_with_crlf() {
        let source = "#![no_std]\r\n#![deny(warnings)]\r\nfn f() {}\r\n";
        assert_eq!(
            &source[file(source).after_inner_attrs..],
            "\r\nfn f() {}\r\n"
        );
    }
}
