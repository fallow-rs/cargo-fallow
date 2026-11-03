//! The bounded parse of workspace source.
//!
//! `ra_ap_syntax` parses with recursion, once per nesting level, and it has no
//! depth limit. Two problems follow. A stack overflow aborts the process, and
//! the parse time grows about with the square of the depth: 16 KiB of nested
//! `<` takes more than a minute. So one small file can stop the whole run.
//! Every parse of workspace source goes through [`parse_source`]. `.clippy.toml`
//! forbids a direct call of the parser everywhere else.
//!
//! [`parse_source`] has three guards:
//!
//! 1. A token scan refuses a source that nests deeper than [`MAX_NESTING`].
//!    This limit is far above real code: in 50076 files of crates.io source,
//!    the deepest brackets are 45 levels, the deepest angle brackets 64
//!    levels, and the longest prefix operator run 7 tokens.
//! 2. The parse runs on a new thread with a stack sized from the depth that
//!    the token scan finds, not from the source length. A 2 MB file of flat
//!    items gets a small stack, and a deep source gets a large one. Other
//!    recursive shapes, such as a long `return return` run, pass the nesting
//!    limit. They can be slow, but the depth counts them, so they cannot
//!    overflow the stack. A long postfix chain, such as `a.x.x.x` or `a???`,
//!    also counts as too deep. Its parse time grows with the square of its
//!    length.
//! 3. A tree deeper than [`MAX_TREE_DEPTH`] is too deep. `ra_ap_syntax` drops
//!    each tree on its own thread with a default stack, and the drop recurses
//!    once for each level. A long operator chain gives such a tree. The tool
//!    keeps that tree and never drops it.
//!
//! The module uses only `std`, `ra_ap_parser` and `ra_ap_syntax`, so the fuzz
//! target in `fuzz/` can include it.

use std::borrow::Cow;
use std::thread;

pub use ra_ap_syntax::Edition;

use ra_ap_parser::{LexedStr, SyntaxKind};
use ra_ap_syntax::SourceFile;

/// The deepest nesting that a source can have and still get a parse.
///
/// The same limit applies to brackets, to angle brackets and to a run of
/// prefix operators.
const MAX_NESTING: u32 = 512;

/// The longest postfix chain that a source can have and still get a parse.
///
/// The parse time of a chain of field accesses, `?` operators, path segments
/// or calls grows with the square of its length: 10000 `.x` take 4 s in a
/// debug build. Method calls count 2 each (the `.` and the `(`), so a chain of
/// 1024 calls still parses.
const MAX_CHAIN: u32 = 2048;

/// The deepest syntax tree that the tool drops.
///
/// `ra_ap_syntax` drops each parsed tree on its `ParseNodeDropper` thread,
/// which has the default stack of 2 MiB. The drop recurses once for each tree
/// level. A measurement found an overflow at about 4300 levels in a debug
/// build. A long left chain, such as `1 + 1 + ...` or `a.f().f()...`, passes
/// the token scan and the parse, but gives one level for each operator. The
/// deepest shape at [`MAX_NESTING`], 512 nested `Box<`, gives 2566 levels.
const MAX_TREE_DEPTH: u32 = 3072;

/// Stack bytes reserved for each level of the scan depth (see [`Depth`]).
///
/// Method: for each recursive shape, a binary search finds the smallest thread
/// stack, in 4 KiB steps, on which `SourceFile::parse` and the tree depth walk
/// finish. Each try runs in a child process, because a stack overflow aborts
/// the process. The cost of one level is the difference of the smallest stacks
/// at two sizes of the shape (128 and 512 levels, or 500 and 2000 for a run),
/// divided by the difference of their scan depths. The shapes are nested `{`,
/// `(`, `[`, `Box<`, `HashMap<u8, `, `-`, `&`, `if`, `match`, `for S {} in`,
/// `else if`, and runs of `return`, `a =`, `|a, b|`, `fn() ->` and `a @`.
///
/// Result with `ra_ap_syntax` 0.0.358 and Rust 1.99. Nested `{` is the most
/// expensive shape on each platform:
///
/// | Build   | macOS arm64 | Linux x86_64 |
/// |---------|-------------|--------------|
/// | release | 1152 bytes  | 992 bytes    |
/// | debug   | 4437 bytes  | 4096 bytes   |
///
/// The values below add a margin of more than 3 times. AddressSanitizer pads
/// every frame, so the fuzz build reserves 8 times the debug value.
const STACK_PER_LEVEL: usize = if cfg!(fuzzing) {
    8 * 16 * 1024
} else if cfg!(debug_assertions) {
    16 * 1024
} else {
    4 * 1024
};

/// Stack for the frames around the parse, for the thread start and for the
/// visit.
///
/// A visit can recurse once for each tree level, up to [`MAX_TREE_DEPTH`].
/// The same value in each build leaves about 5 KiB for each tree level. The
/// stack is a reservation, so the unused part costs no memory.
const BASE_STACK: usize = if cfg!(fuzzing) { 64 << 20 } else { 16 << 20 };

/// The largest stack that a parse asks for.
///
/// The stack is a reservation: a parse touches only the pages that its depth
/// uses. A larger need is refused, and the source counts as too deep. Linux
/// can refuse a reservation of many GiB, so the cap stays at 1 GiB. A release
/// build reaches the cap at a scan depth of about 260000, a debug build at
/// about 65000.
const MAX_STACK: usize = if cfg!(target_pointer_width = "64") {
    1 << 30
} else {
    256 << 20
};

/// The stack size is a multiple of this value. Some platforms refuse a stack
/// size that is not a multiple of the page size.
const STACK_ALIGN: usize = 64 * 1024;

/// The result of a bounded parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseOutcome {
    /// The source parsed with no error.
    Parsed,
    /// The parser recovered from one or more errors.
    Failed {
        /// The number of errors.
        error_count: u32,
    },
    /// The parser panicked, so it did not finish the file.
    Panicked,
    /// The source was not parsed. It nests deeper than [`MAX_NESTING`], the
    /// stack for its scan depth is larger than [`MAX_STACK`], the thread
    /// with that stack did not start, or the tree is deeper than
    /// [`MAX_TREE_DEPTH`].
    TooDeep,
}

/// Parses `source` as a Rust file of `edition` without a stack overflow.
///
/// See [`ParseOutcome`] for the results.
pub fn parse_source(source: &str, edition: Edition) -> ParseOutcome {
    match with_parsed(source, edition, |_, _| ()) {
        Ok(()) => ParseOutcome::Parsed,
        Err(outcome) => outcome,
    }
}

/// Parses `source` and runs `visit` on the tree, with the same guards as
/// [`parse_source`].
///
/// `visit` gets the tree and the text that the parser read. That text has LF
/// line endings, so a text range of the tree can differ from an offset in
/// `source`. `visit` runs only when the source parsed with no error. The tree
/// cannot leave the sized stack, so `visit` must return plain data.
pub fn with_parsed<T: Send>(
    source: &str,
    edition: Edition,
    visit: impl FnOnce(&SourceFile, &str) -> T + Send,
) -> Result<T, ParseOutcome> {
    let source = normalize_line_endings(strip_byte_order_mark(source));
    let Some(stack) = scan_depth(&source, edition).and_then(stack_need) else {
        return Err(ParseOutcome::TooDeep);
    };
    run_with_stack(stack, || parse_and_visit(&source, edition, visit))
        .and_then(std::convert::identity)
}

/// Removes a byte order mark at the start of the text. rustc removes it before it
/// lexes a file, and the parser reports it as an error.
pub fn strip_byte_order_mark(source: &str) -> &str {
    source.strip_prefix('\u{feff}').unwrap_or(source)
}

/// Replaces each CRLF with LF. rustc does this before it lexes a file. The
/// parser does not, and it reports a bare `\r` in a string literal as an error.
fn normalize_line_endings(source: &str) -> Cow<'_, str> {
    if source.contains('\r') {
        Cow::Owned(source.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(source)
    }
}

/// Returns true when the tokens of `source` nest deeper than [`MAX_NESTING`].
#[cfg(test)]
fn nests_too_deep(source: &str, edition: Edition) -> bool {
    scan_depth(source, edition).is_none()
}

/// Returns the scan depth of `source` (see [`Depth`]), or `None` when the
/// tokens nest deeper than [`MAX_NESTING`] or chain longer than [`MAX_CHAIN`].
///
/// The scan reads tokens, so a bracket in a string or a comment does not count.
/// A `<` can be a comparison with no matching `>`, so the angle bracket count
/// starts again at each `;`, `{` and `}`.
fn scan_depth(source: &str, edition: Edition) -> Option<u32> {
    let tokens = LexedStr::new(edition, source);
    let (mut brackets, mut angles, mut prefix_run) = (0_u32, 0_u32, 0_u32);
    let mut chains = Chains::default();
    let mut depth = Depth::default();
    for index in 0..tokens.len() {
        let kind = tokens.kind(index);
        match kind {
            SyntaxKind::L_PAREN | SyntaxKind::L_BRACK | SyntaxKind::L_CURLY => brackets += 1,
            SyntaxKind::R_PAREN | SyntaxKind::R_BRACK | SyntaxKind::R_CURLY => {
                brackets = brackets.saturating_sub(1);
            }
            _ => {}
        }
        match kind {
            SyntaxKind::L_ANGLE => angles += 1,
            SyntaxKind::R_ANGLE => angles = angles.saturating_sub(1),
            SyntaxKind::SEMICOLON | SyntaxKind::L_CURLY | SyntaxKind::R_CURLY => angles = 0,
            _ => {}
        }
        match kind {
            SyntaxKind::MINUS | SyntaxKind::BANG | SyntaxKind::AMP | SyntaxKind::STAR => {
                prefix_run += 1;
            }
            SyntaxKind::WHITESPACE | SyntaxKind::COMMENT => {}
            _ => prefix_run = 0,
        }
        if brackets.max(angles).max(prefix_run) > MAX_NESTING || chains.step(kind) > MAX_CHAIN {
            return None;
        }
        depth.step(kind);
    }
    Some(depth.deepest)
}

/// An upper bound of the parser recursion, counted in tokens.
///
/// The parser recurses for a bracket and for each construct that does not end
/// before the next token, such as `return`, `=`, a closure head or a prefix
/// operator. It goes back to a loop at a `;`, at a `,` of a list and after the
/// `}` of a statement or an item. So the scan depth counts each token since the
/// last of these points, plus the same count of each open bracket level. A long
/// file of flat items has a small depth, and each recursive shape adds at least
/// one level for each token.
///
/// Some counts are larger than the real recursion. A `,` does not end the count
/// while a `<` of the same bracket level is open, or inside closure parameters,
/// because the parser can still be in a nested `<` or closure there. A `}` does
/// not end the count when the next token can continue an expression, such as
/// `else`, `.` or an operator.
///
/// Each bracket level has its own `<` count, so a `;` or a `{}` inside a
/// generic argument, as in `Foo<[u8; 1], Foo<...>>`, does not clear the count
/// of the level around it. The `>` of `->` closes no `<`. A `=>` ends a match
/// arm pattern, so it clears the `<` count of its level: an open `<` there is
/// a comparison.
#[derive(Default)]
struct Depth {
    /// The count of each open bracket level. The current level is not in the
    /// list.
    open: Vec<Run>,
    /// The sum of the token counts in `open`.
    below: u32,
    /// The count of the current level.
    current: Run,
    /// The last token closed a `{`.
    after_curly: bool,
    /// The last token that is not trivia.
    previous: Option<SyntaxKind>,
    /// The largest depth so far.
    deepest: u32,
}

/// The tokens of one level since the last point where the parser loops.
#[derive(Default, Clone, Copy)]
struct Run {
    tokens: u32,
    /// The `|` tokens. An odd count can be inside closure parameters.
    pipes: u32,
    /// The `<` tokens that no `>` closed yet.
    angles: u32,
}

impl Depth {
    /// Reads one token.
    fn step(&mut self, kind: SyntaxKind) {
        if kind.is_trivia() {
            return;
        }
        let previous = self.previous.replace(kind);
        if std::mem::take(&mut self.after_curly)
            && self.current.angles == 0
            && !continues_after_block(kind)
        {
            self.current = Run::default();
        }
        self.current.tokens = self.current.tokens.saturating_add(1);
        match kind {
            SyntaxKind::L_PAREN | SyntaxKind::L_BRACK | SyntaxKind::L_CURLY => {
                self.below = self.below.saturating_add(self.current.tokens);
                self.open.push(std::mem::take(&mut self.current));
            }
            SyntaxKind::R_PAREN | SyntaxKind::R_BRACK | SyntaxKind::R_CURLY => {
                if let Some(parent) = self.open.pop() {
                    self.below = self.below.saturating_sub(parent.tokens);
                    self.current = parent;
                }
                self.after_curly = kind == SyntaxKind::R_CURLY;
            }
            SyntaxKind::SEMICOLON => self.current = Run::default(),
            SyntaxKind::COMMA
                if self.current.angles == 0 && self.current.pipes.is_multiple_of(2) =>
            {
                self.current = Run::default();
            }
            SyntaxKind::PIPE => self.current.pipes = self.current.pipes.saturating_add(1),
            SyntaxKind::L_ANGLE => self.current.angles = self.current.angles.saturating_add(1),
            SyntaxKind::R_ANGLE => match previous {
                Some(SyntaxKind::MINUS) => {}
                Some(SyntaxKind::EQ) => self.current.angles = 0,
                _ => self.current.angles = self.current.angles.saturating_sub(1),
            },
            _ => {}
        }
        self.deepest = self
            .deepest
            .max(self.below.saturating_add(self.current.tokens));
    }
}

/// Returns true when a token of `kind` after a `}` can continue the same
/// expression, so the parser can still be inside it.
///
/// A name, a keyword other than `else`, `as` and `in`, a `#`, a `;`, a `,` or
/// a closing bracket cannot. The next item or statement starts there, or the
/// enclosing list or bracket continues.
fn continues_after_block(kind: SyntaxKind) -> bool {
    match kind {
        SyntaxKind::ELSE_KW | SyntaxKind::AS_KW | SyntaxKind::IN_KW => true,
        SyntaxKind::POUND
        | SyntaxKind::SEMICOLON
        | SyntaxKind::COMMA
        | SyntaxKind::R_CURLY
        | SyntaxKind::R_PAREN
        | SyntaxKind::R_BRACK => false,
        _ => !kind.is_any_identifier(),
    }
}

/// The postfix chain length at each open bracket level.
///
/// A `.`, `?`, `:`, `(` or `[` adds 1 to the chain of its level. A name, a
/// keyword, a literal or a closing bracket continues the chain. Each other
/// token ends it. The tokens inside a bracket pair count for the inner level.
#[derive(Default)]
struct Chains {
    /// The chain of each open level. The outer level is not in the list.
    open: Vec<u32>,
    /// The chain of the outer level.
    outer: u32,
}

impl Chains {
    /// Reads one token. Returns the chain length of its level.
    fn step(&mut self, kind: SyntaxKind) -> u32 {
        let current = self.open.last_mut().unwrap_or(&mut self.outer);
        match kind {
            // The lexer gives `::` as two `:` tokens.
            SyntaxKind::DOT | SyntaxKind::QUESTION | SyntaxKind::COLON => *current += 1,
            SyntaxKind::L_PAREN | SyntaxKind::L_BRACK => {
                *current += 1;
                let length = *current;
                self.open.push(0);
                return length;
            }
            SyntaxKind::L_CURLY => {
                *current = 0;
                self.open.push(0);
                return 0;
            }
            SyntaxKind::R_PAREN | SyntaxKind::R_BRACK | SyntaxKind::R_CURLY => {
                self.open.pop();
            }
            _ if kind.is_trivia() || kind.is_any_identifier() || kind.is_literal() => {}
            _ => *current = 0,
        }
        self.open.last().copied().unwrap_or(self.outer)
    }
}

/// The stack for a parse with a scan depth of `depth`, or `None` when that is
/// more than [`MAX_STACK`].
fn stack_need(depth: u32) -> Option<usize> {
    let need = usize::try_from(depth)
        .ok()?
        .checked_mul(STACK_PER_LEVEL)?
        .checked_add(BASE_STACK)?
        .checked_next_multiple_of(STACK_ALIGN)?;
    (need <= MAX_STACK).then_some(need)
}

/// Runs `parse` on a new thread with `stack` bytes of stack.
///
/// The parsed syntax tree is also dropped on that thread, because the drop
/// recurses as deep as the parse.
fn run_with_stack<T: Send>(
    stack: usize,
    parse: impl FnOnce() -> T + Send,
) -> Result<T, ParseOutcome> {
    thread::scope(|scope| {
        let spawned = thread::Builder::new()
            .name("cargo-fallow-parse".to_owned())
            .stack_size(stack)
            .spawn_scoped(scope, parse);
        match spawned {
            // A joined panic does not reach the scope, so the run continues.
            Ok(handle) => handle.join().map_err(|_| ParseOutcome::Panicked),
            Err(_) => Err(ParseOutcome::TooDeep),
        }
    })
}

/// Parses `source` on the current thread and runs `visit` on the tree. Call it
/// only through [`run_with_stack`].
fn parse_and_visit<T>(
    source: &str,
    edition: Edition,
    visit: impl FnOnce(&SourceFile, &str) -> T,
) -> Result<T, ParseOutcome> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one parse site; it runs on the sized stack of `with_parsed`"
    )]
    let parsed = SourceFile::parse(source, edition);
    if tree_depth(&parsed.syntax_node()) > MAX_TREE_DEPTH {
        // The drop of a tree recurses once for each level, and `ra_ap_syntax`
        // drops each tree on its own thread with a default stack. So the tool
        // keeps a tree that is too deep and never drops it.
        #[expect(
            clippy::mem_forget,
            reason = "a drop of this tree overflows the stack of the drop thread"
        )]
        std::mem::forget(parsed);
        return Err(ParseOutcome::TooDeep);
    }
    match parsed.errors().len() {
        0 => Ok(visit(&parsed.tree(), source)),
        count => Err(ParseOutcome::Failed {
            error_count: u32::try_from(count).unwrap_or(u32::MAX),
        }),
    }
}

/// The number of nodes on the longest path from `root` to a leaf.
fn tree_depth(root: &ra_ap_syntax::SyntaxNode) -> u32 {
    let (mut depth, mut deepest) = (0_u32, 0_u32);
    for event in root.preorder() {
        match event {
            ra_ap_syntax::WalkEvent::Enter(_) => {
                depth += 1;
                deepest = deepest.max(depth);
            }
            ra_ap_syntax::WalkEvent::Leave(_) => depth -= 1,
        }
    }
    deepest
}

#[cfg(test)]
mod tests {
    use ra_ap_syntax::AstNode;

    use super::*;

    const EDITION: Edition = Edition::Edition2024;
    const TOO_DEEP: usize = MAX_NESTING as usize + 1;
    const AT_LIMIT: usize = MAX_NESTING as usize;

    #[test]
    fn valid_and_invalid_source() {
        assert_eq!(parse_source("fn f() {}", EDITION), ParseOutcome::Parsed);
        assert!(matches!(
            parse_source("fn f( {", EDITION),
            ParseOutcome::Failed { .. }
        ));
    }

    #[test]
    fn parser_counts_each_error() {
        let outcome = parse_source("fn a( {}\nfn b( {}\n", EDITION);
        assert!(
            matches!(outcome, ParseOutcome::Failed { error_count } if error_count >= 2),
            "{outcome:?}"
        );
    }

    #[test]
    fn crlf_in_a_string_literal_is_not_an_error() {
        let source = "pub const S: &str = \"a\r\nb\";\r\n";
        assert_eq!(parse_source(source, EDITION), ParseOutcome::Parsed);
    }

    #[test]
    fn edition_selects_the_keywords() {
        let source = "fn async() {}";
        assert_eq!(
            parse_source(source, Edition::Edition2015),
            ParseOutcome::Parsed
        );
        assert!(matches!(
            parse_source(source, Edition::Edition2021),
            ParseOutcome::Failed { .. }
        ));
    }

    #[test]
    fn every_nesting_shape_at_the_limit_parses_on_the_sized_stack() {
        for (prefix, open, close) in [
            ("type T = ", "Box<", ">"),
            ("type T = ", "(", ")"),
            ("fn f() ", "{", "}"),
            ("const C: u8 = ", "(", ")"),
            ("const C: u8 = ", "[", "]"),
        ] {
            let source = format!(
                "{prefix}{}u8{};",
                open.repeat(AT_LIMIT),
                close.repeat(AT_LIMIT)
            );
            let outcome = parse_source(&source, EDITION);
            assert!(
                !matches!(outcome, ParseOutcome::TooDeep | ParseOutcome::Panicked),
                "{open}: {outcome:?}"
            );
        }
        let unclosed = format!("type T = {}", "<".repeat(AT_LIMIT));
        assert!(matches!(
            parse_source(&unclosed, EDITION),
            ParseOutcome::Failed { .. }
        ));
    }

    #[test]
    fn nesting_over_the_limit_is_too_deep() {
        for unit in ["(", "[", "{", "<", "Box<", "&", "-", "!", "*"] {
            let source = format!("type T = {}", unit.repeat(TOO_DEEP));
            assert_eq!(
                parse_source(&source, EDITION),
                ParseOutcome::TooDeep,
                "{unit}"
            );
        }
    }

    #[test]
    fn closed_levels_and_statements_do_not_add_up() {
        let siblings = "fn f() { (a); [b]; }\n".repeat(TOO_DEEP);
        assert!(!nests_too_deep(&siblings, EDITION));
        let comparisons = "const C: bool = a < b;\n".repeat(TOO_DEEP);
        assert!(!nests_too_deep(&comparisons, EDITION));
    }

    #[test]
    fn brackets_in_strings_and_comments_do_not_count() {
        let open = "{".repeat(TOO_DEEP);
        let source = format!("// {open}\nconst S: &str = \"{open}\";\n");
        assert_eq!(parse_source(&source, EDITION), ParseOutcome::Parsed);
    }

    #[test]
    fn recursive_shape_that_passes_the_scan_has_enough_stack() {
        let source = format!("fn f() {{ {} }}", "return ".repeat(2000));
        assert!(!nests_too_deep(&source, EDITION));
        assert_ne!(parse_source(&source, EDITION), ParseOutcome::Panicked);
    }

    #[test]
    fn long_left_chains_give_a_deep_tree_and_are_too_deep() {
        // The parser reads these chains in a loop, so the scan and the parse
        // pass. The tree is still one level deeper for each operator.
        for unit in [" + 1", ".f()", " as u8", "[0]", " = 1"] {
            let source = format!("fn f() -> u32 {{ 1{} }}", unit.repeat(20_000));
            assert_eq!(
                parse_source(&source, EDITION),
                ParseOutcome::TooDeep,
                "{unit}"
            );
        }
        let broken = format!("fn f( {{ 1{} }}", " + 1".repeat(20_000));
        assert_eq!(parse_source(&broken, EDITION), ParseOutcome::TooDeep);
    }

    #[test]
    fn long_postfix_chains_are_too_deep_without_a_parse() {
        // The parse time of these chains grows with the square of the length.
        for unit in [
            ".x", "?", ".0", "::x", "(1)", "(1 + 1)", "?.x", ".await", "[a.b]",
        ] {
            let source = format!("fn f() {{ a{} }}", unit.repeat(5000));
            assert!(nests_too_deep(&source, EDITION), "{unit}");
            assert_eq!(
                parse_source(&source, EDITION),
                ParseOutcome::TooDeep,
                "{unit}"
            );
        }
    }

    #[test]
    fn real_code_shapes_do_not_count_as_a_long_chain() {
        let builder = format!(
            "fn f() {{ b(){} }}",
            ".arg(\"x\").map(|v| v.len()).field".repeat(400)
        );
        assert!(!nests_too_deep(&builder, EDITION));
        let fields = format!(
            "fn eq(a: &S, b: &S) -> bool {{ true{} }}",
            " && a.x.y == b.x.y".repeat(1000)
        );
        assert!(!nests_too_deep(&fields, EDITION));
        assert_eq!(parse_source(&fields, EDITION), ParseOutcome::Parsed);
        let paths = "use a::b::c::d;\nfn f() { x::y::z(a.b.c) }\n".repeat(2000);
        assert!(!nests_too_deep(&paths, EDITION));
    }

    #[test]
    fn chain_under_the_tree_limit_parses() {
        let source = format!("fn f() -> u32 {{ 1{} }}", " + 1".repeat(1000));
        assert_eq!(parse_source(&source, EDITION), ParseOutcome::Parsed);
    }

    /// About 2 MB of flat items, the shape of a generated file: many short
    /// items and one long table.
    fn large_flat_file() -> String {
        use std::fmt::Write as _;
        let mut source = String::from("pub static TABLE: &[u32] = &[");
        for value in 0..50_000 {
            let _ = write!(source, "{value}, ");
        }
        source.push_str("];\n");
        let mut index = 0;
        while source.len() < 2 << 20 {
            let _ = write!(
                source,
                "pub const C{index}: [u32; 2] = [{index}, 1];\n\
                 pub fn f{index}(a: u32) -> u32 {{ if a > {index} {{ a }} else {{ a + 1 }} }}\n\
                 #[derive(Debug)]\n\
                 pub struct S{index} {{ pub a: Vec<u32>, b: Option<u8> }}\n\
                 impl S{index} {{ fn g(&self) -> usize {{ self.a.iter().map(|x| *x as usize).sum() }} }}\n"
            );
            index += 1;
        }
        source
    }

    #[test]
    fn large_flat_file_parses() {
        let source = large_flat_file();
        assert!(source.len() >= 2 << 20);
        assert_eq!(parse_source(&source, EDITION), ParseOutcome::Parsed);
    }

    #[test]
    fn need_grows_with_depth_and_is_aligned() {
        assert_eq!(stack_need(0), Some(BASE_STACK));
        assert!(stack_need(1000) > stack_need(100));
        assert_eq!(stack_need(1).map(|need| need % STACK_ALIGN), Some(0));
        assert_eq!(stack_need(u32::MAX), None);
        let over = u32::try_from(MAX_STACK / STACK_PER_LEVEL + 1).unwrap_or(u32::MAX);
        assert_eq!(stack_need(over), None);
    }

    #[test]
    fn reservation_is_bounded_by_depth_not_by_size() {
        let large = large_flat_file();
        let small = &large[large.len() - 4096..];
        let small = &small[small.find("pub const").unwrap_or(0)..];
        let (Some(large_depth), Some(small_depth)) =
            (scan_depth(&large, EDITION), scan_depth(small, EDITION))
        else {
            panic!("both sources pass the scan");
        };
        // The items of the large file are the same shape as the items of the
        // small file. Only the table adds a little depth for its `&[` prefix.
        assert!(
            large_depth <= small_depth + 8,
            "{large_depth} {small_depth}"
        );
        assert!(large_depth < 64, "{large_depth}");
        // The reservation stays small in each build, also at 2 MB.
        assert!(stack_need(large_depth) <= Some(BASE_STACK + 64 * STACK_PER_LEVEL));
        // A deeper source gets a larger reservation.
        let deep = format!("fn f() {}u8{}", "{".repeat(AT_LIMIT), "}".repeat(AT_LIMIT));
        assert!(scan_depth(&deep, EDITION) > Some(u32::try_from(AT_LIMIT).unwrap_or(0)));
    }

    #[test]
    fn each_recursive_shape_adds_depth() {
        for (prefix, unit) in [
            ("fn f() { ", "return "),
            ("fn f() { ", "a = "),
            ("fn f() { ", "|a, b| "),
            ("fn f() { ", "move || "),
            ("fn f() { ", ".."),
            ("type T = ", "fn() -> "),
            ("type T = ", "HashMap<u8, "),
            ("fn f() { let ", "a @ "),
            ("fn f() { ", "if a {} else "),
            ("fn f() { ", "for S {} in "),
        ] {
            let source = format!("{prefix}{}", unit.repeat(400));
            let depth = scan_depth(&source, EDITION).unwrap_or(u32::MAX);
            assert!((400..u32::MAX).contains(&depth), "{unit}: {depth}");
        }
    }

    #[test]
    fn generic_arguments_with_inner_statements_add_depth() {
        // A `;` or `{}` inside a generic argument must not end the count of
        // the open `<` levels around it. An arrow `->` closes no `<`.
        for unit in [
            "Foo<[u8; 1], ",
            "Foo<{1}, ",
            "Foo<fn() -> u8, ",
            "Foo<|a| -> u8 { a }, ",
        ] {
            let source = format!("type T = {}u8{};", unit.repeat(2000), ", u8>".repeat(2000));
            let depth = scan_depth(&source, EDITION).unwrap_or(u32::MAX);
            assert!((2000..u32::MAX).contains(&depth), "{unit}: {depth}");
        }
    }

    #[test]
    fn deep_generic_arguments_do_not_overflow_the_stack() {
        for unit in ["Foo<[u8; 1], ", "Foo<{1}, "] {
            let source = format!(
                "type T = {}u8{};",
                unit.repeat(20_000),
                ", u8>".repeat(20_000)
            );
            assert_eq!(
                parse_source(&source, EDITION),
                ParseOutcome::TooDeep,
                "{unit}"
            );
        }
    }

    #[test]
    fn match_arms_with_comparisons_keep_a_small_depth() {
        let arms = "x if x < 3 => 1,\n".repeat(5000);
        let source = format!("fn f(x: u8) -> u8 {{ match x {{ {arms} _ => 0 }} }}");
        let depth = scan_depth(&source, EDITION).unwrap_or(u32::MAX);
        assert!(depth < 64, "{depth}");
    }

    #[test]
    fn depth_over_the_cap_is_too_deep_without_a_parse() {
        let run = MAX_STACK / STACK_PER_LEVEL + 1;
        let source = format!("fn f() {{ {} }}", "return ".repeat(run));
        assert_eq!(parse_source(&source, EDITION), ParseOutcome::TooDeep);
    }

    #[test]
    fn visit_gets_the_tree_and_the_normalized_text() {
        let result = with_parsed("fn f() {}\r\n", EDITION, |file, text| {
            (file.syntax().text_range().end(), text.to_owned())
        });
        let Ok((end, text)) = result else {
            panic!("the source parses");
        };
        assert_eq!(text, "fn f() {}\n");
        assert_eq!(usize::from(end), text.len());
    }

    #[test]
    fn visit_does_not_run_on_a_source_with_errors() {
        let result = with_parsed("fn f( {", EDITION, |_, _| panic!("no visit"));
        assert!(matches!(result, Err(ParseOutcome::Failed { .. })));
    }

    #[test]
    fn visit_does_not_run_on_a_source_that_nests_too_deep() {
        let source = "(".repeat(TOO_DEEP);
        let result = with_parsed(&source, EDITION, |_, _| panic!("no visit"));
        assert_eq!(result, Err(ParseOutcome::TooDeep));
    }

    #[test]
    fn visit_panic_is_reported() {
        let result = with_parsed("fn f() {}", EDITION, |_, _| -> u8 { panic!("visit panic") });
        assert_eq!(result, Err(ParseOutcome::Panicked));
    }

    #[test]
    fn parser_panic_is_reported() {
        let outcome = run_with_stack(BASE_STACK, || -> u8 { panic!("parser panic") });
        assert_eq!(outcome, Err(ParseOutcome::Panicked));
    }
}
