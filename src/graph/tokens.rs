//! Names in token streams: macro calls, `macro_rules!` bodies, attributes and
//! code blocks in documentation.
//!
//! The parser keeps these as tokens, not as paths. This module finds each
//! chain `a::b::c` and each single name in the tokens. The graph gives credit
//! to each of them, because the syntax cannot show what the tokens become.

use ra_ap_parser::{LexedStr, SyntaxKind};
use ra_ap_syntax::{NodeOrToken, SyntaxNode, T};
use rustc_hash::FxHashMap;

use crate::parse::Edition;

/// A name or a path that a token stream contains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenPath {
    /// The segments. A `$crate` segment becomes `crate`.
    pub segments: Vec<String>,
    /// True when the chain starts with `::`.
    pub global: bool,
    /// True for a single name that can be a field or a method: after `.`,
    /// or before `:`, `,` or `}` as in a struct expression.
    pub member: bool,
    /// For a field name in a struct expression `Path { field: .. }`, the path
    /// of the struct.
    pub record: Option<Vec<String>>,
    /// True for a chain after `#`. In `quote!` tokens, `#name` is a variable
    /// of the code that builds the tokens, not a name in the output.
    pub interpolated: bool,
    /// True for a chain in the tokens of a nested call of an output macro,
    /// for example `quote! { .. }` in the tokens of `vec![..]`.
    pub quoted: bool,
    /// True for a name that the tokens define and do not use: the name after
    /// `fn`, `struct`, `enum`, `trait`, `type`, `const`, `static` or `mod`, a
    /// parameter, a field declaration or a variant declaration.
    pub defined: bool,
    /// True for a member name before `(`: a method call `.name(..)`.
    pub call: bool,
    /// True for a single name in a `::{..}` group, as in `use a::{b, c}`.
    /// The name is no field and no method.
    pub grouped: bool,
}

/// The macros whose tokens are the output of a proc-macro. The output goes
/// into the crate that calls the proc-macro.
pub const OUTPUT_MACROS: &[&str] = &[
    "quote",
    "quote_spanned",
    "parse_quote",
    "parse_quote_spanned",
];

/// A keyword before the name of an item that the tokens define.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keyword {
    Fn,
    Struct,
    Enum,
    /// `trait`, `type`, `const`, `static` or `mod`.
    Item,
    Mut,
}

/// A token with only the facts that the scan needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Piece<'a> {
    Ident(&'a str),
    Colon,
    Dollar,
    Pound,
    Bang,
    Semi,
    Star,
    Lt,
    Gt,
    Keyword(Keyword),
    Dot,
    Comma,
    OpenBrace,
    CloseBrace,
    OpenParen,
    Open,
    Close,
    Other,
}

/// The paths and names in the tokens below `node`.
pub fn paths_in_tree(node: &SyntaxNode) -> Vec<TokenPath> {
    let tokens: Vec<_> = node
        .descendants_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .collect();
    let pieces: Vec<Piece<'_>> = tokens
        .iter()
        .filter(|token| !token.kind().is_trivia())
        .flat_map(|token| pieces_of(token.kind(), token.text()))
        .collect();
    let mut paths = chains(&pieces);
    for token in &tokens {
        if token.kind() == SyntaxKind::STRING {
            paths.extend(path_in_string(token.text()));
        }
    }
    paths
}

/// The paths and names in a code block of documentation.
pub fn paths_in_code(code: &str, edition: Edition) -> Vec<TokenPath> {
    let lexed = LexedStr::new(edition, code);
    let pieces: Vec<Piece<'_>> = (0..lexed.len())
        .filter(|&index| !lexed.kind(index).is_trivia())
        .flat_map(|index| pieces_of(lexed.kind(index), lexed.text(index)))
        .collect();
    chains(&pieces)
}

/// The pieces of one token. A `::` token gives two colons, so the scan sees
/// the same input from the lexer and from the parser.
fn pieces_of(kind: SyntaxKind, text: &str) -> Vec<Piece<'_>> {
    if kind == T![::] {
        return vec![Piece::Colon, Piece::Colon];
    }
    vec![piece(kind, text)]
}

fn piece(kind: SyntaxKind, text: &str) -> Piece<'_> {
    match kind {
        SyntaxKind::IDENT => Piece::Ident(text.strip_prefix("r#").unwrap_or(text)),
        T![crate] => Piece::Ident("crate"),
        T![self] => Piece::Ident("self"),
        T![super] => Piece::Ident("super"),
        T![Self] => Piece::Ident("Self"),
        T![:] => Piece::Colon,
        T![$] => Piece::Dollar,
        T![#] => Piece::Pound,
        T![!] => Piece::Bang,
        T![;] => Piece::Semi,
        T![*] => Piece::Star,
        T![<] => Piece::Lt,
        T![>] => Piece::Gt,
        T![fn] => Piece::Keyword(Keyword::Fn),
        T![struct] => Piece::Keyword(Keyword::Struct),
        T![enum] => Piece::Keyword(Keyword::Enum),
        T![trait] | T![type] | T![const] | T![static] | T![mod] => Piece::Keyword(Keyword::Item),
        T![mut] => Piece::Keyword(Keyword::Mut),
        T![.] => Piece::Dot,
        T![,] => Piece::Comma,
        T!['{'] => Piece::OpenBrace,
        T!['}'] => Piece::CloseBrace,
        T!['('] => Piece::OpenParen,
        T!['['] => Piece::Open,
        T![')'] | T![']'] => Piece::Close,
        _ => Piece::Other,
    }
}

/// Finds the chains of names joined by `::`. A `$name` is a macro variable and
/// does not count, but `$crate` does.
fn chains(pieces: &[Piece<'_>]) -> Vec<TokenPath> {
    let mut records: FxHashMap<usize, Vec<String>> = FxHashMap::default();
    let quoted = quoted_pieces(pieces);
    let defined = defined_pieces(pieces);
    let grouped = grouped_pieces(pieces);
    let mut paths = Vec::new();
    let mut index = 0;
    while index < pieces.len() {
        index = match chain_start(pieces, index) {
            Some(first) => {
                let next = push_chain(pieces, index, first, &mut records, &mut paths);
                if let Some(path) = paths.last_mut() {
                    path.quoted = quoted[index];
                    path.defined = defined[index] && path.segments.len() == 1;
                    path.grouped = grouped[index] && path.segments.len() == 1;
                }
                next
            }
            None => index + 1,
        };
    }
    paths
}

/// For each piece, true when it is in the tokens of a call of an output
/// macro: a name of [`OUTPUT_MACROS`], then `!` and a group.
fn quoted_pieces(pieces: &[Piece<'_>]) -> Vec<bool> {
    let mut quoted = vec![false; pieces.len()];
    for index in 0..pieces.len() {
        let call = matches!(pieces[index], Piece::Ident(name) if OUTPUT_MACROS.contains(&name))
            && pieces.get(index + 1) == Some(&Piece::Bang)
            && matches!(
                pieces.get(index + 2),
                Some(Piece::Open | Piece::OpenParen | Piece::OpenBrace)
            );
        if !call {
            continue;
        }
        let mut depth = 0_u32;
        for (offset, piece) in pieces.iter().enumerate().skip(index + 2) {
            match piece {
                Piece::Open | Piece::OpenParen | Piece::OpenBrace => depth += 1,
                Piece::Close | Piece::CloseBrace => depth = depth.saturating_sub(1),
                _ => {}
            }
            if depth == 0 {
                break;
            }
            quoted[offset] = true;
        }
    }
    quoted
}

/// For each piece, true when the innermost group around it is a `::{..}`
/// group of a path.
fn grouped_pieces(pieces: &[Piece<'_>]) -> Vec<bool> {
    let mut grouped = vec![false; pieces.len()];
    let mut stack: Vec<bool> = Vec::new();
    for (index, piece) in pieces.iter().enumerate() {
        match piece {
            Piece::OpenBrace => stack.push(
                index >= 2
                    && pieces[index - 1] == Piece::Colon
                    && pieces[index - 2] == Piece::Colon,
            ),
            Piece::Open | Piece::OpenParen => stack.push(false),
            Piece::CloseBrace | Piece::Close => {
                stack.pop();
            }
            _ => grouped[index] = stack.last() == Some(&true),
        }
    }
    grouped
}

/// For each piece, true when it is a name that the tokens define. The scan
/// marks the name after an item keyword. It also marks the parameters of a
/// function, the fields of a struct and the variants and fields of an enum.
fn defined_pieces(pieces: &[Piece<'_>]) -> Vec<bool> {
    let mut defined = vec![false; pieces.len()];
    let opaque = opaque_pieces(pieces);
    for (index, piece) in pieces.iter().enumerate() {
        let Piece::Keyword(keyword) = *piece else {
            continue;
        };
        // In `*const T`, the name after `const` is a type, not a definition.
        // The tokens of a nested macro call can mean anything.
        if keyword == Keyword::Mut
            || opaque[index]
            || (index > 0 && pieces[index - 1] == Piece::Star)
        {
            continue;
        }
        let mut name = index + 1;
        if pieces.get(name) == Some(&Piece::Keyword(Keyword::Mut)) {
            name += 1;
        }
        let after_name = match pieces.get(name) {
            Some(Piece::Ident(_)) => {
                defined[name] = true;
                name + 1
            }
            // In `fn #name(..)`, the name is a variable of the code that
            // builds the tokens, but the item still defines its parts.
            Some(Piece::Pound) if matches!(pieces.get(name + 1), Some(Piece::Ident(_))) => name + 2,
            _ => continue,
        };
        let Some((open, body)) = item_body(pieces, after_name, keyword) else {
            continue;
        };
        for (inner, depth, opener) in group_names(pieces, open) {
            let field = single_colon_after(pieces, inner);
            let mark = match body {
                // An upper case parameter can be a constant pattern, a use.
                Keyword::Fn => depth == 1 && field && !starts_upper_name(pieces[inner]),
                Keyword::Struct => depth == 1 && field,
                Keyword::Enum if depth == 1 => variant_position(pieces, inner),
                // The fields of a variant, not of a struct expression in a
                // discriminant: the `{` follows the name of a variant.
                Keyword::Enum => depth == 2 && field && variant_position(pieces, opener - 1),
                Keyword::Item | Keyword::Mut => false,
            };
            defined[inner] |= mark;
        }
    }
    defined
}

/// True when the name at `index` is in the position of a variant name in an
/// enum body: after `{`, `,` or the `]` of an attribute.
fn variant_position(pieces: &[Piece<'_>], index: usize) -> bool {
    matches!(pieces[index], Piece::Ident(_))
        && index > 0
        && matches!(
            pieces[index - 1],
            Piece::OpenBrace | Piece::Comma | Piece::Close
        )
}

/// True for a name that starts with an upper case letter.
fn starts_upper_name(piece: Piece<'_>) -> bool {
    matches!(piece, Piece::Ident(name) if name.chars().next().is_some_and(char::is_uppercase))
}

/// For each piece, true when it is in the tokens of a nested macro call that
/// is not an output macro: `name!(..)`. Those tokens are not Rust code.
fn opaque_pieces(pieces: &[Piece<'_>]) -> Vec<bool> {
    let mut opaque = vec![false; pieces.len()];
    let mut stack: Vec<bool> = Vec::new();
    for (index, piece) in pieces.iter().enumerate() {
        match piece {
            Piece::Open | Piece::OpenParen | Piece::OpenBrace => {
                let call = index >= 2
                    && pieces[index - 1] == Piece::Bang
                    && matches!(pieces[index - 2], Piece::Ident(name) if !OUTPUT_MACROS.contains(&name));
                let outer = stack.last().copied().unwrap_or(false);
                stack.push(outer || call);
            }
            Piece::Close | Piece::CloseBrace => {
                stack.pop();
            }
            _ => opaque[index] = stack.last().copied().unwrap_or(false),
        }
    }
    opaque
}

/// The index of the group that holds the parts of an item, and the keyword
/// of the item: the parameters of a function, or the body of a struct or an
/// enum. The scan starts after the name and skips the generic parameters.
/// For a struct or an enum, it also skips a `(..)` group, for example the
/// `Fn(u8)` of a where clause.
fn item_body(pieces: &[Piece<'_>], start: usize, keyword: Keyword) -> Option<(usize, Keyword)> {
    let mut angle = 0_u32;
    let mut skipped = 0_u32;
    for (index, piece) in pieces.iter().enumerate().skip(start) {
        if skipped > 0 {
            match piece {
                Piece::Open | Piece::OpenParen | Piece::OpenBrace => skipped += 1,
                Piece::Close | Piece::CloseBrace => skipped -= 1,
                _ => {}
            }
            continue;
        }
        match piece {
            Piece::Lt => angle += 1,
            Piece::Gt => angle = angle.saturating_sub(1),
            Piece::Open | Piece::OpenParen if angle == 0 => {
                if keyword == Keyword::Fn {
                    return Some((index, keyword));
                }
                skipped = 1;
            }
            Piece::OpenBrace if angle == 0 => {
                return matches!(keyword, Keyword::Struct | Keyword::Enum)
                    .then_some((index, keyword));
            }
            Piece::Semi | Piece::CloseBrace | Piece::Close if angle == 0 => return None,
            _ => {}
        }
    }
    None
}

/// The names in the group that opens at `open`, each with its depth and the
/// index of the innermost group around it. The depth is one for a name
/// directly in the group. A nested group in `(..)` or `[..]` gives no names,
/// because it holds types or attributes, not fields. A name in generic
/// arguments `<..>` is a type, so it is also left out.
fn group_names(pieces: &[Piece<'_>], open: usize) -> Vec<(usize, u32, usize)> {
    let mut names = Vec::new();
    let mut stack: Vec<(bool, usize)> = Vec::new();
    let mut angle = 0_u32;
    for (index, piece) in pieces.iter().enumerate().skip(open) {
        match piece {
            Piece::OpenBrace => stack.push((true, index)),
            Piece::Open | Piece::OpenParen => stack.push((index == open, index)),
            Piece::CloseBrace | Piece::Close => {
                stack.pop();
                if stack.is_empty() {
                    break;
                }
            }
            Piece::Lt => angle += 1,
            Piece::Gt => angle = angle.saturating_sub(1),
            Piece::Ident(_) if angle == 0 && stack.iter().all(|&(named, _)| named) => {
                let depth = u32::try_from(stack.len()).unwrap_or(u32::MAX);
                let opener = stack.last().map_or(open, |&(_, opener)| opener);
                names.push((index, depth, opener));
            }
            _ => {}
        }
    }
    names
}

/// True when a single `:` follows the piece at `index`, not a `::`.
fn single_colon_after(pieces: &[Piece<'_>], index: usize) -> bool {
    pieces.get(index + 1) == Some(&Piece::Colon) && pieces.get(index + 2) != Some(&Piece::Colon)
}

/// Adds the chain that starts with `first` at `index` to `paths`, and returns
/// the index of the piece after the chain. A struct expression after the
/// chain records its field names in `records`.
fn push_chain(
    pieces: &[Piece<'_>],
    index: usize,
    first: &str,
    records: &mut FxHashMap<usize, Vec<String>>,
    paths: &mut Vec<TokenPath>,
) -> usize {
    let (segments, next) = chain_segments(pieces, index, first);
    let member = chain_member(pieces, index, next, segments.len());
    if pieces.get(next) == Some(&Piece::OpenBrace) && starts_upper(&segments) {
        for field in record_fields(pieces, next) {
            records.insert(field, segments.clone());
        }
    }
    let record = records.remove(&index);
    paths.push(TokenPath {
        segments,
        global: chain_global(pieces, index),
        member: member && record.is_none(),
        record,
        interpolated: index > 0 && pieces[index - 1] == Piece::Pound,
        quoted: false,
        defined: false,
        call: member
            && index > 0
            && pieces[index - 1] == Piece::Dot
            && pieces.get(next) == Some(&Piece::OpenParen),
        grouped: false,
    });
    next
}

/// The first name of a chain at `index`: a name that does not follow `$`,
/// or `$crate`.
fn chain_start<'a>(pieces: &[Piece<'a>], index: usize) -> Option<&'a str> {
    let Piece::Ident(first) = pieces[index] else {
        return None;
    };
    let after_dollar = index > 0 && pieces[index - 1] == Piece::Dollar;
    (!after_dollar || first == "crate").then_some(first)
}

/// True when a `::` without a name before it comes before the chain at
/// `index`.
fn chain_global(pieces: &[Piece<'_>], index: usize) -> bool {
    index >= 2
        && pieces[index - 1] == Piece::Colon
        && pieces[index - 2] == Piece::Colon
        && !matches!(pieces.get(index.wrapping_sub(3)), Some(Piece::Ident(_)))
}

/// The segments of the chain that starts with `first` at `index`, and the
/// index of the piece after the chain.
fn chain_segments(pieces: &[Piece<'_>], index: usize, first: &str) -> (Vec<String>, usize) {
    let mut segments = vec![first.to_owned()];
    let mut next = index + 1;
    while let (Some(Piece::Colon), Some(Piece::Colon), Some(Piece::Ident(name))) =
        (pieces.get(next), pieces.get(next + 1), pieces.get(next + 2))
    {
        segments.push((*name).to_owned());
        next += 3;
    }
    (segments, next)
}

/// True when a chain of one name can be a field or a method: it follows
/// `.`, or comes before a single `:`, a `,` or a `}`.
fn chain_member(pieces: &[Piece<'_>], index: usize, next: usize, len: usize) -> bool {
    let single_colon =
        pieces.get(next) == Some(&Piece::Colon) && pieces.get(next + 1) != Some(&Piece::Colon);
    len == 1
        && ((index > 0 && pieces[index - 1] == Piece::Dot)
            || single_colon
            || matches!(pieces.get(next), Some(Piece::Comma | Piece::CloseBrace)))
}

/// True when the last segment of a path starts with an upper case letter, as
/// the name of a struct or a variant does.
fn starts_upper(segments: &[String]) -> bool {
    segments
        .last()
        .and_then(|last| last.chars().next())
        .is_some_and(char::is_uppercase)
}

/// The indexes of the field names of the struct expression whose `{` is at
/// `open`: a name at depth one, after `{` or `,` and before `:`, `,` or `}`.
fn record_fields(pieces: &[Piece<'_>], open: usize) -> Vec<usize> {
    let mut fields = Vec::new();
    let mut depth = 0_u32;
    for index in open..pieces.len() {
        match pieces[index] {
            Piece::OpenBrace | Piece::Open | Piece::OpenParen => depth += 1,
            Piece::CloseBrace | Piece::Close => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            Piece::Ident(_) if depth == 1 => {
                let after_start = matches!(pieces[index - 1], Piece::OpenBrace | Piece::Comma);
                let before_end = matches!(
                    pieces.get(index + 1),
                    Some(Piece::Colon | Piece::Comma | Piece::CloseBrace)
                ) && pieces.get(index + 2) != Some(&Piece::Colon);
                if after_start && before_end {
                    fields.push(index);
                }
            }
            _ => {}
        }
    }
    fields
}

/// A path in a string literal, as in `#[serde(with = "a::b")]`.
fn path_in_string(literal: &str) -> Option<TokenPath> {
    let text = literal.strip_prefix('"')?.strip_suffix('"')?;
    let looks_like_path = !text.is_empty()
        && text.split("::").all(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
    looks_like_path.then(|| TokenPath {
        segments: text.split("::").map(str::to_owned).collect(),
        global: false,
        member: false,
        record: None,
        interpolated: false,
        quoted: false,
        defined: false,
        call: false,
        grouped: false,
    })
}

/// The Rust code blocks of a Markdown text, as rustdoc compiles them.
///
/// A line that starts with `# ` is hidden in the documentation, but rustdoc
/// compiles it, so the `#` goes. A block with `ignore`, `text` or another
/// language is not Rust code. A `compile_fail` block does not compile, so it
/// gives no use. Each block has the zero-based index of its first code line.
pub fn rust_code_blocks_at(markdown: &str) -> Vec<(u32, String)> {
    let mut blocks = Vec::new();
    let mut current: Option<(String, u32, String)> = None;
    for (index, line) in markdown.lines().enumerate() {
        let trimmed = line.trim_start();
        let fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        match (&mut current, fence) {
            (None, true) => {
                let info = trimmed.trim_start_matches(['`', '~']).trim().to_owned();
                let first = u32::try_from(index + 1).unwrap_or(u32::MAX);
                current = Some((info, first, String::new()));
            }
            (Some(_), true) => {
                if let Some((info, first, code)) = current.take()
                    && is_rust_block(&info)
                {
                    blocks.push((first, code));
                }
            }
            (Some((_, _, code)), false) => {
                let visible = trimmed
                    .strip_prefix("# ")
                    .or_else(|| (trimmed == "#").then_some(""))
                    .unwrap_or(line);
                code.push_str(visible);
                code.push('\n');
            }
            (None, false) => {}
        }
    }
    blocks
}

fn is_rust_block(info: &str) -> bool {
    const NOT_COMPILED: [&str; 3] = ["ignore", "text", "compile_fail"];
    const RUST_WORDS: [&str; 6] = [
        "rust",
        "no_run",
        "should_panic",
        "edition2015",
        "edition2018",
        "edition2021",
    ];
    let words: Vec<&str> = info
        .split([',', ' ', '\t'])
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .collect();
    if words.iter().any(|word| NOT_COMPILED.contains(word)) {
        return false;
    }
    words.iter().all(|word| {
        RUST_WORDS.contains(word) || word.starts_with("edition") || word.starts_with('{')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(paths: &[TokenPath]) -> Vec<String> {
        paths
            .iter()
            .map(|path| {
                let joined = path.segments.join("::");
                if path.global {
                    format!("::{joined}")
                } else {
                    joined
                }
            })
            .collect()
    }

    #[test]
    fn code_gives_chains_and_single_names() {
        let paths = paths_in_code(
            "let x = a::b::c(y); ::std::mem::drop(z);",
            Edition::Edition2021,
        );
        assert_eq!(
            names(&paths),
            ["x", "a::b::c", "y", "::std::mem::drop", "z"]
        );
    }

    #[test]
    fn macro_variables_do_not_count_but_dollar_crate_does() {
        let paths = paths_in_code("$x:expr => $crate::inner::run($x)", Edition::Edition2021);
        assert_eq!(names(&paths), ["expr", "crate::inner::run"]);
    }

    #[test]
    fn quote_variables_are_marked() {
        let paths = paths_in_code(
            "#[inline] fn f() { #(#items),* a::b(#depth) }",
            Edition::Edition2021,
        );
        let marks: Vec<(String, bool)> = paths
            .iter()
            .map(|path| (path.segments.join("::"), path.interpolated))
            .collect();
        assert_eq!(
            marks,
            [
                ("inline".to_owned(), false),
                ("f".to_owned(), false),
                ("items".to_owned(), true),
                ("a::b".to_owned(), false),
                ("depth".to_owned(), true),
            ]
        );
    }

    /// The names that the scan marks as defined, in order.
    fn defined(code: &str) -> Vec<String> {
        paths_in_code(code, Edition::Edition2021)
            .into_iter()
            .filter(|path| path.defined)
            .map(|path| path.segments.join("::"))
            .collect()
    }

    #[test]
    fn definitions_are_marked() {
        assert_eq!(
            defined(
                "pub fn px<F: Fn(u8)>(&self, cx: &App, mut n: u8) -> Pixels { px(cx.n) } \
                 struct Shape<T> { #[serde(default)] pub(crate) level: Vec<T>, size: a::B } \
                 enum Kind { #[default] Alpha, Beta(u8, c::D), Gamma { depth: u8 } } \
                 trait Tr {} type Al = u8; const LIMIT: u8 = 1; static mut COUNT: u8 = 0; \
                 mod inner {}"
            ),
            [
                "px", "cx", "n", "Shape", "level", "size", "Kind", "Alpha", "Beta", "Gamma",
                "depth", "Tr", "Al", "LIMIT", "COUNT", "inner",
            ]
        );
    }

    #[test]
    fn where_clauses_and_interpolated_names_keep_definitions() {
        assert_eq!(
            defined(
                "struct W<F> where F: Fn(u8) { wfield: F } fn #name(cursor: u8) {} \
                 struct #other { level: u8 } struct T(u8) where u8: Copy; fn after(n: u8) {}"
            ),
            ["W", "wfield", "cursor", "level", "T", "after", "n"]
        );
    }

    #[test]
    fn uses_next_to_definitions_are_not_marked() {
        assert!(
            defined(
                "impl a::Trait for X { } let p: *const Target = q; let r: *mut Other = s; \
                 let f: fn(u8) -> Out = g; Shape { level: 1 }; x.focus(&handle); \
                 struct Tuple(Field); fn call(Pat { inner }: Pat) {}"
            ) == ["Tuple", "call"],
            "only the item names are definitions"
        );
    }

    #[test]
    fn values_patterns_and_macro_tokens_are_not_definitions() {
        assert_eq!(
            defined(
                "enum E { A = items::Bits { bit: 1 }.other, B = pick::<u8, Target>(), C } \
                 fn generated(UNIT: (), plain: u8) { invoke!(fn helper); }"
            ),
            ["E", "A", "B", "C", "generated", "plain"]
        );
    }

    #[test]
    fn method_calls_and_use_groups_are_marked() {
        let paths = paths_in_code(
            "use a::{b, c::d}; x.call(1); x.field; x.index[0]; S { e, f: 1 }",
            Edition::Edition2021,
        );
        let marks: Vec<(String, bool, bool)> = paths
            .iter()
            .map(|path| (path.segments.join("::"), path.call, path.grouped))
            .collect();
        let expected = [
            ("a", false, false),
            ("b", false, true),
            ("c::d", false, false),
            ("x", false, false),
            ("call", true, false),
            ("x", false, false),
            ("field", false, false),
            ("x", false, false),
            ("index", false, false),
            ("S", false, false),
            ("e", false, false),
            ("f", false, false),
        ];
        let expected: Vec<(String, bool, bool)> = expected
            .iter()
            .map(|&(name, call, grouped)| (name.to_owned(), call, grouped))
            .collect();
        assert_eq!(marks, expected);
    }

    #[test]
    fn names_in_nested_output_macros_are_marked() {
        let paths = paths_in_code(
            "vec![a::x, quote! { b::y(c) }, d, syn::parse_quote!(e)]",
            Edition::Edition2021,
        );
        let marks: Vec<(String, bool)> = paths
            .iter()
            .map(|path| (path.segments.join("::"), path.quoted))
            .collect();
        assert_eq!(
            marks,
            [
                ("vec".to_owned(), false),
                ("a::x".to_owned(), false),
                ("quote".to_owned(), false),
                ("b::y".to_owned(), true),
                ("c".to_owned(), true),
                ("d".to_owned(), false),
                ("syn::parse_quote".to_owned(), false),
                ("e".to_owned(), true),
            ]
        );
    }

    #[test]
    fn member_names_and_record_fields_are_marked() {
        let paths = paths_in_code(
            "x.field; Link { text: a, more }; call(b, c)",
            Edition::Edition2021,
        );
        let marks: Vec<(String, bool, Option<String>)> = paths
            .iter()
            .map(|path| {
                (
                    path.segments.join("::"),
                    path.member,
                    path.record.as_ref().map(|record| record.join("::")),
                )
            })
            .collect();
        let link = Some("Link".to_owned());
        assert_eq!(
            marks,
            [
                ("x".to_owned(), false, None),
                ("field".to_owned(), true, None),
                ("Link".to_owned(), false, None),
                ("text".to_owned(), false, link.clone()),
                ("a".to_owned(), true, None),
                ("more".to_owned(), false, link),
                ("call".to_owned(), false, None),
                ("b".to_owned(), true, None),
                ("c".to_owned(), false, None),
            ]
        );
    }

    #[test]
    fn string_paths_count() {
        assert_eq!(
            path_in_string("\"a::b_c\"").map(|path| path.segments),
            Some(vec!["a".to_owned(), "b_c".to_owned()])
        );
        assert_eq!(path_in_string("\"two words\""), None);
        assert_eq!(path_in_string("\"\""), None);
    }

    #[test]
    fn code_blocks_follow_rustdoc() {
        let markdown = "text\n```\nuse a::B;\n# fn hidden() {}\n```\n```ignore\nskip\n```\n\
                        ```rust,no_run\nrun\n```\n```toml\n[x]\n```\n```compile_fail\nbad\n```\n";
        assert_eq!(
            rust_code_blocks_at(markdown),
            [
                (2, "use a::B;\nfn hidden() {}\n".to_owned()),
                (9, "run\n".to_owned())
            ]
        );
    }
}
