//! Finds the rebuild-tier mutants of one source file.
//!
//! These operators are not type-safe by construction. `a + b` can be a type
//! with no `Sub`, a replaced body can have the wrong type, and a deleted arm can
//! leave a match without an arm for a value. One of them in a schema would break
//! the build of the whole crate. So each one runs in its own rebuild, and a
//! mutant that does not compile gets the status `compile_error`.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::ops::Range;

use ra_ap_syntax::ast::{self, HasName};
use ra_ap_syntax::{AstNode, SourceFile, SyntaxKind, SyntaxNode, SyntaxToken};

use crate::mutation::id::{IdInput, mutant_id};
use crate::mutation::schema::PROBE_PREFIX;
use crate::mutation::sites::{OffsetMap, attr_context, in_constant_context, range_of};
use crate::parse::{Edition, ParseOutcome, with_parsed};

/// Where a probe build puts its undefined name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeSpot {
    /// Around an expression, as `({ name; expr })`.
    Wrap(Range<usize>),
    /// At the start of a block, after this offset.
    AfterBrace(usize),
}

/// One rebuild-tier mutant: one splice of the original text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewrite {
    /// The stable id.
    pub id: u64,
    /// The operator name of the report.
    pub operator: &'static str,
    /// The byte range of the original text that the mutant replaces.
    pub range: Range<usize>,
    /// The new text of the range.
    pub replacement: String,
    /// True when a `cfg` attribute other than `cfg(test)` is on an enclosing item.
    pub under_cfg: bool,
    /// True when the mutant is in a constant context.
    pub constant: bool,
    /// The place of the probe.
    pub probe: ProbeSpot,
}

/// The result of the walk over one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileRewrites {
    /// The mutants, in the order of their ranges.
    pub rewrites: Vec<Rewrite>,
    /// The functions with a return type that has no known replacement.
    pub unknown_return_types: u32,
}

/// Finds the mutants of one file.
pub fn find_rewrites(
    source: &str,
    edition: Edition,
    file_key: &str,
) -> Result<FileRewrites, ParseOutcome> {
    let offsets = OffsetMap::new(source);
    with_parsed(source, edition, |file, text| {
        let mut walk = Walk {
            offsets: &offsets,
            text,
            file_key,
            occurrences: BTreeMap::new(),
            constant: false,
            found: FileRewrites::default(),
        };
        walk.file(file);
        walk.found.rewrites.sort_by_key(|r| r.range.start);
        walk.found
    })
}

struct Walk<'a> {
    offsets: &'a OffsetMap,
    text: &'a str,
    file_key: &'a str,
    occurrences: BTreeMap<(String, &'static str), u32>,
    constant: bool,
    found: FileRewrites,
}

/// The parts of one new mutant, in normalized offsets.
struct Part<'a> {
    site: &'a SyntaxNode,
    operator: &'static str,
    range: Range<usize>,
    replacement: String,
    under_cfg: bool,
    probe: ProbeSpot,
}

impl Walk<'_> {
    fn file(&mut self, file: &SourceFile) {
        for node in file.syntax().descendants() {
            let Some(under_cfg) = allowed(&node) else {
                continue;
            };
            self.constant = in_constant_context(&node);
            if let Some(binary) = ast::BinExpr::cast(node.clone()) {
                self.binary(&binary, under_cfg);
            } else if let Some(prefix) = ast::PrefixExpr::cast(node.clone()) {
                self.prefix(&prefix, under_cfg);
            } else if let Some(function) = ast::Fn::cast(node.clone()) {
                self.function(&function, under_cfg);
            } else if let Some(matched) = ast::MatchExpr::cast(node) {
                self.match_arms(&matched, under_cfg);
            }
        }
    }

    fn binary(&mut self, binary: &ast::BinExpr, under_cfg: bool) {
        let Some(token) = binary.op_token() else {
            return;
        };
        let Some(swaps) = arithmetic_swaps(token.kind()) else {
            return;
        };
        for &replacement in swaps {
            self.push(Part {
                site: binary.syntax(),
                operator: "arithmetic-operator",
                range: token_range(&token),
                replacement: replacement.to_owned(),
                under_cfg,
                probe: ProbeSpot::Wrap(range_of(binary.syntax())),
            });
        }
    }

    fn prefix(&mut self, prefix: &ast::PrefixExpr, under_cfg: bool) {
        let Some(token) = prefix.op_token() else {
            return;
        };
        if !matches!(token.kind(), SyntaxKind::BANG | SyntaxKind::MINUS) {
            return;
        }
        // The schema tier negates a condition, and `!(!a)` is the same mutant.
        let is_condition = prefix.syntax().parent().is_some_and(|parent| {
            matches!(parent.kind(), SyntaxKind::IF_EXPR | SyntaxKind::WHILE_EXPR)
        });
        if is_condition && token.kind() == SyntaxKind::BANG {
            return;
        }
        self.push(Part {
            site: prefix.syntax(),
            operator: "unary-operator",
            range: token_range(&token),
            replacement: String::new(),
            under_cfg,
            probe: ProbeSpot::Wrap(range_of(prefix.syntax())),
        });
    }

    fn function(&mut self, function: &ast::Fn, under_cfg: bool) {
        // A body of an unsafe function can hold invariants that a new return
        // value breaks, and that is undefined behavior and not a test result.
        if function.unsafe_token().is_some() {
            return;
        }
        let Some(body) = function.body() else {
            return;
        };
        let return_type =
            function
                .ret_type()
                .and_then(|ret| ret.ty())
                .map_or_else(String::new, |ty| {
                    ty.syntax()
                        .text()
                        .to_string()
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .collect()
                });
        let Some(values) = return_values(&return_type) else {
            self.found.unknown_return_types += 1;
            return;
        };
        let range = range_of(body.syntax());
        let body_text = self.text.get(range.clone()).unwrap_or_default();
        let inner = body_text
            .trim_start_matches('{')
            .trim_end_matches('}')
            .trim();
        for &value in values {
            let is_same = inner == value || (value == "()" && inner.is_empty());
            if is_same {
                continue;
            }
            self.push(Part {
                site: function.syntax(),
                operator: "return-value",
                range: range.clone(),
                replacement: format!("{{ {value} }}"),
                under_cfg,
                probe: ProbeSpot::AfterBrace(range.start + 1),
            });
        }
    }

    /// Deletes each arm of a match that has a catch-all arm. Without that arm
    /// a deleted arm does not compile, so the rebuild would only cost time.
    fn match_arms(&mut self, matched: &ast::MatchExpr, under_cfg: bool) {
        let Some(list) = matched.match_arm_list() else {
            return;
        };
        let arms: Vec<ast::MatchArm> = list.arms().collect();
        if !arms.iter().any(is_catch_all) {
            return;
        }
        for arm in arms.iter().filter(|arm| !is_catch_all(arm)) {
            let Some(expr) = arm.expr() else {
                continue;
            };
            self.push(Part {
                site: arm.syntax(),
                operator: "deleted-match-arm",
                range: range_of(arm.syntax()),
                replacement: String::new(),
                under_cfg,
                probe: ProbeSpot::Wrap(range_of(expr.syntax())),
            });
        }
    }

    fn push(&mut self, part: Part<'_>) {
        let site_text = self
            .text
            .get(range_of(part.site))
            .unwrap_or_default()
            .to_owned();
        // A site can have some mutants, so the count is per site and operator,
        // and each replacement of the site uses the count of the site.
        let key = (format!("{site_text}\0{}", part.replacement), part.operator);
        let occurrence = self.occurrences.entry(key).or_default();
        let id = mutant_id(IdInput {
            file: self.file_key,
            site_text: &site_text,
            occurrence: *occurrence,
            operator: part.operator,
            replacement: &part.replacement,
        });
        *occurrence += 1;
        let map = |range: Range<usize>| {
            self.offsets.original(range.start)..self.offsets.original(range.end)
        };
        let probe = match part.probe {
            ProbeSpot::Wrap(range) => ProbeSpot::Wrap(map(range)),
            ProbeSpot::AfterBrace(offset) => ProbeSpot::AfterBrace(self.offsets.original(offset)),
        };
        self.found.rewrites.push(Rewrite {
            id,
            operator: part.operator,
            range: map(part.range),
            replacement: part.replacement,
            under_cfg: part.under_cfg,
            constant: self.constant,
            probe,
        });
    }
}

/// The `under_cfg` flag of a node that can hold a mutant, or `None` for a node
/// where a mutant has no use: tests, attributes, patterns and types.
fn allowed(node: &SyntaxNode) -> Option<bool> {
    let blocked = node.ancestors().any(|ancestor| {
        let kind = ancestor.kind();
        ast::Pat::can_cast(kind)
            || ast::Type::can_cast(kind)
            || matches!(
                kind,
                SyntaxKind::ATTR | SyntaxKind::CONST_ARG | SyntaxKind::CONST_PARAM
            )
    });
    if blocked {
        return None;
    }
    attr_context(node)
}

fn token_range(token: &SyntaxToken) -> Range<usize> {
    let range = token.text_range();
    usize::from(range.start())..usize::from(range.end())
}

const fn arithmetic_swaps(kind: SyntaxKind) -> Option<&'static [&'static str]> {
    Some(match kind {
        SyntaxKind::PLUS => &["-"],
        SyntaxKind::MINUS => &["+"],
        SyntaxKind::STAR | SyntaxKind::PERCENT => &["/"],
        SyntaxKind::SLASH => &["*"],
        SyntaxKind::AMP => &["|"],
        SyntaxKind::PIPE | SyntaxKind::CARET => &["&"],
        SyntaxKind::SHL => &[">>"],
        SyntaxKind::SHR => &["<<"],
        SyntaxKind::PLUSEQ => &["-="],
        SyntaxKind::MINUSEQ => &["+="],
        SyntaxKind::STAREQ | SyntaxKind::PERCENTEQ => &["/="],
        SyntaxKind::SLASHEQ => &["*="],
        SyntaxKind::AMPEQ => &["|="],
        SyntaxKind::PIPEEQ | SyntaxKind::CARETEQ => &["&="],
        SyntaxKind::SHLEQ => &[">>="],
        SyntaxKind::SHREQ => &["<<="],
        _ => return None,
    })
}

/// The replacement bodies of a return type without whitespace, or `None` when
/// the type has no known value. The text `` is a function with no return type.
fn return_values(ty: &str) -> Option<&'static [&'static str]> {
    const UNSIGNED: &[&str] = &["0", "1"];
    const SIGNED: &[&str] = &["0", "1", "-1"];
    const FLOAT: &[&str] = &["0.0", "1.0", "-1.0"];
    Some(match ty {
        "" | "()" => &["()"],
        "bool" => &["true", "false"],
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize" => UNSIGNED,
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" => SIGNED,
        "f32" | "f64" => FLOAT,
        "String" => &["String::new()", "\"xyzzy\".into()"],
        _ if ty.starts_with('&') && ty.ends_with("str") && !ty.contains("mut") => {
            &["\"\"", "\"xyzzy\""]
        }
        _ => return generic_return_values(ty),
    })
}

fn generic_return_values(ty: &str) -> Option<&'static [&'static str]> {
    let head = ty.split('<').next().unwrap_or(ty);
    Some(match head.rsplit("::").next().unwrap_or(head) {
        "Option" => &["None"],
        "Result" => &["Ok(Default::default())"],
        "Vec" => &["Vec::new()"],
        _ => return None,
    })
}

fn is_catch_all(arm: &ast::MatchArm) -> bool {
    if arm.guard().is_some() {
        return false;
    }
    match arm.pat() {
        Some(ast::Pat::WildcardPat(_)) => true,
        Some(ast::Pat::IdentPat(ident)) => ident
            .name()
            .is_some_and(|name| name.text().chars().next().is_some_and(char::is_lowercase)),
        _ => false,
    }
}

/// Applies one mutant to the original text.
pub fn apply(original: &str, range: &Range<usize>, replacement: &str) -> String {
    let mut text = String::with_capacity(original.len() + replacement.len());
    text.push_str(&original[..range.start]);
    text.push_str(replacement);
    text.push_str(&original[range.end..]);
    text
}

/// Writes a probe build of a file. Each mutant in `rewrites` gets an undefined
/// name at its place, so the compiler reports the mutants that a build reads.
pub fn render_probe(original: &str, rewrites: &[&Rewrite]) -> String {
    // Each entry is an offset and a text to insert there. An outer wrap opens
    // before an inner one and closes after it.
    let mut opens: Vec<(usize, usize, String)> = Vec::new();
    let mut closes: Vec<(usize, usize, &str)> = Vec::new();
    for rewrite in rewrites {
        let name = format!("{PROBE_PREFIX}{:x}", rewrite.id);
        match &rewrite.probe {
            ProbeSpot::Wrap(range) => {
                let mut text = String::new();
                // A write to a `String` cannot fail.
                let _ = write!(text, "({{ {name}; ");
                opens.push((range.start, usize::MAX - range.end, text));
                closes.push((range.end, range.start, " })"));
            }
            ProbeSpot::AfterBrace(offset) => {
                opens.push((*offset, usize::MAX, format!(" {name}; ")));
            }
        }
    }
    opens.sort();
    closes.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    let mut out = String::with_capacity(original.len() + opens.len() * 32);
    let (mut open_at, mut close_at, mut cursor) = (0, 0, 0);
    while open_at < opens.len() || close_at < closes.len() {
        let next_open = opens.get(open_at).map_or(usize::MAX, |entry| entry.0);
        let next_close = closes.get(close_at).map_or(usize::MAX, |entry| entry.0);
        let offset = next_open.min(next_close);
        out.push_str(&original[cursor..offset]);
        cursor = offset;
        // A close at an offset comes first: the expression before it ends there.
        if next_close <= next_open {
            out.push_str(closes[close_at].2);
            close_at += 1;
        } else {
            out.push_str(&opens[open_at].2);
            open_at += 1;
        }
    }
    out.push_str(&original[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(source: &str) -> FileRewrites {
        let Ok(found) = find_rewrites(source, Edition::Edition2021, "src/lib.rs") else {
            panic!("the source parses");
        };
        found
    }

    /// The operator and the text after the splice, without the id.
    fn results(source: &str) -> Vec<(&'static str, String)> {
        found(source)
            .rewrites
            .iter()
            .map(|r| (r.operator, apply(source, &r.range, &r.replacement)))
            .collect()
    }

    fn only_operator(source: &str, operator: &str) -> Vec<String> {
        results(source)
            .into_iter()
            .filter(|(name, _)| *name == operator)
            .map(|(_, text)| text)
            .collect()
    }

    #[test]
    fn arithmetic_operators_swap() {
        for (op, swapped) in [
            ("+", "-"),
            ("-", "+"),
            ("*", "/"),
            ("/", "*"),
            ("%", "/"),
            ("&", "|"),
            ("|", "&"),
            ("^", "&"),
            ("<<", ">>"),
            (">>", "<<"),
        ] {
            let source = format!("fn f(a: u8, b: u8) {{ let _ = a {op} b; }}");
            let expected = format!("fn f(a: u8, b: u8) {{ let _ = a {swapped} b; }}");
            assert_eq!(
                only_operator(&source, "arithmetic-operator"),
                [expected],
                "{op}"
            );
        }
    }

    #[test]
    fn compound_assignment_swaps() {
        for (op, swapped) in [
            ("+=", "-="),
            ("-=", "+="),
            ("*=", "/="),
            ("/=", "*="),
            ("%=", "/="),
            ("&=", "|="),
            ("|=", "&="),
            ("^=", "&="),
            ("<<=", ">>="),
            (">>=", "<<="),
        ] {
            let source = format!("fn f(mut a: u8) {{ a {op} 1; }}");
            let expected = format!("fn f(mut a: u8) {{ a {swapped} 1; }}");
            assert_eq!(
                only_operator(&source, "arithmetic-operator"),
                [expected],
                "{op}"
            );
        }
    }

    #[test]
    fn comparison_and_logic_stay_in_the_schema_tier() {
        let source = "fn f(a: u8, b: bool) { let _ = a == 1 && b || a < 2; }";
        assert!(only_operator(source, "arithmetic-operator").is_empty());
    }

    #[test]
    fn unary_operators_are_removed() {
        assert_eq!(
            only_operator("fn f(a: bool) { let _ = !a; }", "unary-operator"),
            ["fn f(a: bool) { let _ = a; }"]
        );
        assert_eq!(
            only_operator("fn f(a: i8) { let _ = -a; }", "unary-operator"),
            ["fn f(a: i8) { let _ = a; }"]
        );
        assert!(only_operator("fn f(a: &u8) { let _ = *a; }", "unary-operator").is_empty());
    }

    /// The schema tier has `!(!a)` for this condition, and that is the same mutant.
    #[test]
    fn a_negated_condition_is_not_a_second_mutant() {
        assert!(only_operator("fn f(a: bool) { if !a {} }", "unary-operator").is_empty());
        assert!(only_operator("fn f(a: bool) { while !a {} }", "unary-operator").is_empty());
        assert_eq!(
            only_operator("fn f(a: bool) { if b(!a) {} }", "unary-operator"),
            ["fn f(a: bool) { if b(a) {} }"]
        );
    }

    #[test]
    fn return_values_follow_the_type() {
        for (ty, values) in [
            ("bool", &["true", "false"][..]),
            ("u32", &["0", "1"]),
            ("i64", &["0", "1", "-1"]),
            ("f64", &["0.0", "1.0", "-1.0"]),
            ("String", &["String::new()", "\"xyzzy\".into()"]),
            ("&'static str", &["\"\"", "\"xyzzy\""]),
            ("Option<u8>", &["None"]),
            ("std::option::Option<u8>", &["None"]),
            ("Result<u8, E>", &["Ok(Default::default())"]),
            ("io::Result<()>", &["Ok(Default::default())"]),
            ("Vec<u8>", &["Vec::new()"]),
        ] {
            let source = format!("fn f() -> {ty} {{ todo!() }}");
            let expected: Vec<String> = values
                .iter()
                .map(|value| format!("fn f() -> {ty} {{ {value} }}"))
                .collect();
            assert_eq!(only_operator(&source, "return-value"), expected, "{ty}");
        }
    }

    #[test]
    fn unit_function_gets_an_empty_body() {
        assert_eq!(
            only_operator("fn f() { g(); }", "return-value"),
            ["fn f() { () }"]
        );
        assert!(only_operator("fn f() {}", "return-value").is_empty());
        assert!(only_operator("fn f() { () }", "return-value").is_empty());
    }

    #[test]
    fn a_body_with_the_same_value_gives_no_mutant() {
        assert_eq!(
            only_operator("fn f() -> bool { true }", "return-value"),
            ["fn f() -> bool { false }"]
        );
    }

    #[test]
    fn unknown_return_types_are_counted() {
        for source in [
            "fn f() -> Foo { x() }",
            "fn f() -> impl Iterator<Item = u8> { x() }",
            "fn f<T>() -> T { x() }",
            "fn f() -> &'static mut str { x() }",
        ] {
            let found = found(source);
            assert_eq!(found.unknown_return_types, 1, "{source}");
            assert!(found.rewrites.is_empty(), "{source}");
        }
    }

    #[test]
    fn unsafe_and_bodiless_functions_get_no_return_mutant() {
        assert!(only_operator("unsafe fn f() -> u8 { 1 }", "return-value").is_empty());
        assert!(only_operator("trait T { fn f() -> u8; }", "return-value").is_empty());
    }

    #[test]
    fn match_arms_are_deleted_only_with_a_catch_all() {
        let source = "fn f(x: u8) -> u8 { match x { 1 => 2, 3 => 4, _ => 5 } }";
        assert_eq!(
            only_operator(source, "deleted-match-arm"),
            [
                "fn f(x: u8) -> u8 { match x {  3 => 4, _ => 5 } }",
                "fn f(x: u8) -> u8 { match x { 1 => 2,  _ => 5 } }",
            ]
        );
        let binding = "fn f(x: u8) -> u8 { match x { 1 => 2, y => y } }";
        assert_eq!(only_operator(binding, "deleted-match-arm").len(), 1);
        let none = "fn f(x: bool) -> u8 { match x { true => 2, false => 3 } }";
        assert!(only_operator(none, "deleted-match-arm").is_empty());
        let guarded = "fn f(x: u8) -> u8 { match x { y if y > 1 => 2, _ => 3 } }";
        assert_eq!(only_operator(guarded, "deleted-match-arm").len(), 1);
        let upper = "fn f(x: Option<u8>) -> u8 { match x { Some(_) => 2, None => 3 } }";
        assert!(only_operator(upper, "deleted-match-arm").is_empty());
    }

    #[test]
    fn test_code_attributes_patterns_and_types_have_no_mutant() {
        for source in [
            "#[cfg(test)] mod t { fn f() -> u8 { 1 + 2 } }",
            "#[test] fn f() { let _ = 1 + 2; }",
            "#[doc = 1 + 2] struct S;",
            "struct S; fn f(_: [u8; 1 + 2]) {}",
        ] {
            assert!(
                only_operator(source, "arithmetic-operator").is_empty(),
                "{source}"
            );
        }
    }

    #[test]
    fn a_constant_context_is_marked() {
        let source = "const fn f(a: u8) -> u8 { a + 1 }\nfn g(a: u8) -> u8 { a + 1 }\nconst X: u8 = 1 + 2;\n";
        let flags: Vec<(usize, bool)> = found(source)
            .rewrites
            .iter()
            .filter(|rewrite| rewrite.operator == "arithmetic-operator")
            .map(|rewrite| (rewrite.range.start, rewrite.constant))
            .collect();
        assert_eq!(flags.len(), 3, "{flags:?}");
        let line_two = source.find('\n').unwrap_or(0);
        let line_three = source.rfind("const").unwrap_or(0);
        for (start, constant) in flags {
            let in_g = start > line_two && start < line_three;
            assert_eq!(constant, !in_g, "{start}");
        }
    }

    #[test]
    fn a_rebuild_can_mutate_a_constant_context() {
        assert_eq!(
            only_operator("const X: u8 = 1 + 2;", "arithmetic-operator"),
            ["const X: u8 = 1 - 2;"]
        );
        assert_eq!(
            only_operator("const fn f() -> u8 { 1 }", "return-value"),
            ["const fn f() -> u8 { 0 }"]
        );
    }

    #[test]
    fn a_cfg_marks_the_mutant() {
        let source = "#[cfg(any())] fn f() { let _ = 1 + 2; }";
        assert!(found(source).rewrites.iter().all(|r| r.under_cfg));
        assert!(
            found("fn f() { let _ = 1 + 2; }")
                .rewrites
                .iter()
                .all(|r| !r.under_cfg)
        );
    }

    #[test]
    fn equal_sites_have_different_ids_and_an_edit_elsewhere_keeps_them() {
        let source = "fn f(a: u8) { let _ = a + 1; let _ = a + 1; }";
        let ids: Vec<u64> = found(source)
            .rewrites
            .iter()
            .filter(|r| r.operator == "arithmetic-operator")
            .map(|r| r.id)
            .collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        let moved = found("\n\n// A comment.\nfn f(a: u8) { let _ = a + 1; let _ = a + 1; }");
        let moved_ids: Vec<u64> = moved
            .rewrites
            .iter()
            .filter(|r| r.operator == "arithmetic-operator")
            .map(|r| r.id)
            .collect();
        assert_eq!(ids, moved_ids);
    }

    #[test]
    fn crlf_offsets_map_to_the_original_text() {
        let source = "fn f(a: u8) {\r\n  let _ = a\r\n    + 1;\r\n}\r\n";
        let results = only_operator(source, "arithmetic-operator");
        assert_eq!(
            results,
            ["fn f(a: u8) {\r\n  let _ = a\r\n    - 1;\r\n}\r\n"]
        );
    }

    fn all_probes(source: &str) -> String {
        let found = found(source);
        let refs: Vec<&Rewrite> = found.rewrites.iter().collect();
        let mut text = render_probe(source, &refs);
        for rewrite in &found.rewrites {
            text = text.replace(&format!("{PROBE_PREFIX}{:x}", rewrite.id), "P");
        }
        text
    }

    #[test]
    fn probe_wraps_an_expression_and_opens_a_body() {
        assert_eq!(
            all_probes("fn f() -> u8 { 1 + 2 }"),
            "fn f() -> u8 { P;  P;  ({ P; 1 + 2 }) }"
        );
    }

    #[test]
    fn probe_nests_outer_around_inner() {
        let text = all_probes("const X: u8 = 1 + 2 * 3;");
        assert_eq!(text, "const X: u8 = ({ P; 1 + ({ P; 2 * 3 }) });");
    }

    #[test]
    fn probe_of_touching_expressions_closes_before_it_opens() {
        let text = all_probes("const X: bool = !!a;");
        assert_eq!(text, "const X: bool = ({ P; !({ P; !a }) });");
    }

    #[test]
    fn probe_of_an_arm_wraps_only_its_expression() {
        let text = all_probes("const X: u8 = match 1 { 1 => 2, _ => 3 };");
        assert_eq!(text, "const X: u8 = match 1 { 1 => ({ P; 2 }), _ => 3 };");
    }
}
