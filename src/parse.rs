//! The bounded parse of workspace source.
//!
//! `ra_ap_syntax` parses with recursion, once per nesting level, and it has no
//! depth limit. Two problems follow. A stack overflow aborts the process, and
//! the parse time grows about with the square of the depth: 16 KiB of nested
//! `<` takes more than a minute. So one small file can stop the whole run.
//! Every parse of workspace source goes through [`parse_source`]. `.clippy.toml`
//! forbids a direct call of the parser everywhere else.
//!
//! [`parse_source`] has two guards:
//!
//! 1. A token scan refuses a source that nests deeper than [`MAX_NESTING`].
//!    This limit is far above real code: in 50076 files of crates.io source,
//!    the deepest brackets are 45 levels, the deepest angle brackets 64
//!    levels, and the longest prefix operator run 7 tokens.
//! 2. The parse runs on a new thread with a stack sized from the source
//!    length. Other recursive shapes, such as a long `return return` run, pass
//!    the scan. They can be slow, but they cannot overflow the stack. This is
//!    the same stack model as `parse_bounded` in fallow.
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

/// Stack bytes reserved for each source byte.
///
/// A measurement with `ra_ap_syntax` 0.0.359 found the most expensive shape in
/// nested `{`. It uses about 1.2 KiB of stack for each source byte in a release
/// build. The values below add a margin. A debug build has much larger frames.
/// AddressSanitizer pads every frame, so the fuzz build reserves 8 times the
/// debug value.
const STACK_PER_SOURCE_BYTE: usize = if cfg!(fuzzing) {
    8 * 48 * 1024
} else if cfg!(debug_assertions) {
    48 * 1024
} else {
    4 * 1024
};

/// Stack for the frames around the parse and for the thread start.
const BASE_STACK: usize = 1 << 20;

/// The largest stack that a parse asks for.
///
/// The stack is a reservation: a parse touches only the pages that its depth
/// uses. A larger need is refused, and the source counts as too deep. In a
/// 64-bit release build, only a source over 16 MiB reaches the cap.
const MAX_STACK: usize = if cfg!(target_pointer_width = "64") {
    64 << 30
} else {
    1 << 30
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
    /// stack that the parse needs is larger than [`MAX_STACK`], or the thread
    /// with that stack did not start.
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
    let Some(stack) = stack_need(source.len()) else {
        return Err(ParseOutcome::TooDeep);
    };
    if nests_too_deep(&source, edition) {
        return Err(ParseOutcome::TooDeep);
    }
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
///
/// The scan reads tokens, so a bracket in a string or a comment does not count.
/// A `<` can be a comparison with no matching `>`, so the angle bracket count
/// starts again at each `;`, `{` and `}`.
fn nests_too_deep(source: &str, edition: Edition) -> bool {
    let tokens = LexedStr::new(edition, source);
    let (mut brackets, mut angles, mut prefix_run) = (0_u32, 0_u32, 0_u32);
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
        if brackets.max(angles).max(prefix_run) > MAX_NESTING {
            return true;
        }
    }
    false
}

/// The stack that a parse of `source_len` bytes can use, or `None` when that
/// is more than [`MAX_STACK`].
fn stack_need(source_len: usize) -> Option<usize> {
    let need = source_len
        .checked_mul(STACK_PER_SOURCE_BYTE)?
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
    match parsed.errors().len() {
        0 => Ok(visit(&parsed.tree(), source)),
        count => Err(ParseOutcome::Failed {
            error_count: u32::try_from(count).unwrap_or(u32::MAX),
        }),
    }
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
    fn need_grows_with_length_and_is_aligned() {
        assert_eq!(stack_need(0), Some(BASE_STACK));
        assert!(stack_need(1000) > stack_need(100));
        assert_eq!(stack_need(1).map(|need| need % STACK_ALIGN), Some(0));
        assert_eq!(stack_need(usize::MAX), None);
        assert_eq!(stack_need(MAX_STACK / STACK_PER_SOURCE_BYTE + 1), None);
    }

    #[test]
    fn source_over_the_cap_is_too_deep_without_a_parse() {
        let source = " ".repeat(MAX_STACK / STACK_PER_SOURCE_BYTE + 1);
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
