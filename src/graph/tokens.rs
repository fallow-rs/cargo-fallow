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
}

/// A token with only the facts that the scan needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Piece<'a> {
    Ident(&'a str),
    Colon,
    Dollar,
    Dot,
    Comma,
    OpenBrace,
    CloseBrace,
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
        T![.] => Piece::Dot,
        T![,] => Piece::Comma,
        T!['{'] => Piece::OpenBrace,
        T!['}'] => Piece::CloseBrace,
        T!['('] | T!['['] => Piece::Open,
        T![')'] | T![']'] => Piece::Close,
        _ => Piece::Other,
    }
}

/// Finds the chains of names joined by `::`. A `$name` is a macro variable and
/// does not count, but `$crate` does.
fn chains(pieces: &[Piece<'_>]) -> Vec<TokenPath> {
    let mut records: FxHashMap<usize, Vec<String>> = FxHashMap::default();
    let mut paths = Vec::new();
    let mut index = 0;
    while index < pieces.len() {
        let after_dollar = index > 0 && pieces[index - 1] == Piece::Dollar;
        let current = pieces[index];
        let Piece::Ident(first) = current else {
            index += 1;
            continue;
        };
        if after_dollar && first != "crate" {
            index += 1;
            continue;
        }
        let global = index >= 2
            && pieces[index - 1] == Piece::Colon
            && pieces[index - 2] == Piece::Colon
            && !matches!(pieces.get(index.wrapping_sub(3)), Some(Piece::Ident(_)));
        let mut segments = vec![first.to_owned()];
        let mut next = index + 1;
        while let (Some(Piece::Colon), Some(Piece::Colon), Some(Piece::Ident(name))) =
            (pieces.get(next), pieces.get(next + 1), pieces.get(next + 2))
        {
            segments.push((*name).to_owned());
            next += 3;
        }
        let single_colon =
            pieces.get(next) == Some(&Piece::Colon) && pieces.get(next + 1) != Some(&Piece::Colon);
        let member = segments.len() == 1
            && ((index > 0 && pieces[index - 1] == Piece::Dot)
                || single_colon
                || matches!(pieces.get(next), Some(Piece::Comma | Piece::CloseBrace)));
        if pieces.get(next) == Some(&Piece::OpenBrace) && starts_upper(&segments) {
            for field in record_fields(pieces, next) {
                records.insert(field, segments.clone());
            }
        }
        let record = records.remove(&index);
        paths.push(TokenPath {
            segments,
            global,
            member: member && record.is_none(),
            record,
        });
        index = next;
    }
    paths
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
            Piece::OpenBrace | Piece::Open => depth += 1,
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
