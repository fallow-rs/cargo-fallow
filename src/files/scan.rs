//! The scan of one source file for the `files` check.
//!
//! The scan gives the module declarations that the walk follows, the files
//! that a literal `include!`, `include_str!` or `include_bytes!` call reads,
//! and the reason when the syntax cannot tell which files the file reaches.

use std::path::Path;

use ra_ap_syntax::ast::{self, HasModuleItem, HasName};
use ra_ap_syntax::{
    AstNode, AstToken, NodeOrToken, SyntaxKind, SyntaxNode, SyntaxToken, WalkEvent,
};

use crate::attrs::plain_name;
use crate::modules::{ModuleDecl, normalize};

/// The macros that read a file at compile time.
const INCLUDE_MACROS: [&str; 3] = ["include", "include_str", "include_bytes"];

/// The environment variable of the build output folder. An include from this
/// folder reads no source file.
const OUT_DIR: &str = "OUT_DIR";

/// What one file reaches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    /// The module declarations, also the ones in a macro call.
    pub modules: Vec<ModuleDecl>,
    /// The files that a literal include reads, relative to the workspace root.
    pub includes: Vec<String>,
    /// The files that a literal `include!` call compiles as code. Their
    /// modules and includes resolve from this file.
    pub code_includes: Vec<String>,
    /// The `.rs` literals in macro tokens, relative to the workspace root. A
    /// macro can give such a literal to an include call.
    pub credited: Vec<String>,
    /// The reason when the syntax cannot tell each file that the file reaches.
    pub undecided: Option<&'static str>,
    /// True when the reason can reach the files of other packages too: a macro
    /// can run in each crate that uses it.
    pub workspace_wide: bool,
}

impl Scan {
    fn undecide(&mut self, reason: &'static str) {
        self.undecided.get_or_insert(reason);
    }

    fn undecide_workspace(&mut self, reason: &'static str) {
        self.undecide(reason);
        self.workspace_wide = true;
    }
}

/// True when the scan of `text` can find nothing, also when the text does
/// not parse: the text has no `mod` word, no call of an include macro, no
/// `.rs` string literal and no escape that can hide one. The walk then needs
/// no parse of the file.
pub fn reaches_nothing(text: &str) -> bool {
    !has_word(text, "mod", |_| true)
        && !INCLUDE_MACROS
            .iter()
            .any(|name| has_word(text, name, |rest| skip_comments(rest).starts_with('!')))
        && !text.contains(".rs\"")
        && !has_hiding_escape(text)
}

/// True when `word` occurs in `text` as a whole identifier, and `after`
/// accepts the text that follows it.
fn has_word(text: &str, word: &str, after: impl Fn(&str) -> bool) -> bool {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(word).any(|(at, _)| {
        let rest = &text[at + word.len()..];
        !text[..at].chars().next_back().is_some_and(is_ident)
            && !rest.chars().next().is_some_and(is_ident)
            && after(rest)
    })
}

/// The text after the white space and the comments at its start.
fn skip_comments(mut text: &str) -> &str {
    loop {
        text = text.trim_start();
        if let Some(rest) = text.strip_prefix("//") {
            text = rest.split_once('\n').map_or("", |(_, after)| after);
        } else if let Some(rest) = text.strip_prefix("/*") {
            // A nested comment ends later. The first `*/` can then only find
            // a `!` too many, and that makes the walk parse the file.
            text = rest.split_once("*/").map_or("", |(_, after)| after);
        } else {
            return text;
        }
    }
}

/// True when `text` has an escape that can write a letter or a `.` in a
/// string, for example `\x73` or `\u{2e}`, or a line continuation. Only
/// `\"`, `\\`, `\'`, `\n`, `\r`, `\t` and `\0` are safe.
fn has_hiding_escape(text: &str) -> bool {
    text.match_indices('\\').any(|(at, _)| {
        !text[at + 1..]
            .chars()
            .next()
            .is_some_and(|next| matches!(next, '"' | '\\' | '\'' | 'n' | 'r' | 't' | '0'))
    })
}

/// Scans the parsed file at `path`, relative to the workspace root.
pub fn scan(file: &ast::SourceFile, path: &str) -> Scan {
    let mut found = Scan::default();
    collect_declarations(file.items(), &mut Vec::new(), &mut found);
    if declares_module_in_block(file) {
        found.undecide("a function or a block declares a module");
    }
    let folder = Path::new(path).parent().unwrap_or_else(|| Path::new(""));
    // An include call and macro tokens occur only in a macro call, a macro
    // definition, the token tree of an attribute or code that does not
    // parse, so the scan reads only
    // the tokens of these nodes. A file that a generator wrote can have
    // millions of other tokens.
    let mut walk = file.syntax().preorder();
    while let Some(event) = walk.next() {
        let WalkEvent::Enter(node) = event else {
            continue;
        };
        if !is_macro_like(&node) {
            continue;
        }
        walk.skip_subtree();
        scan_tokens(&node, folder, &mut found);
    }
    found.includes.sort();
    found.includes.dedup();
    found
}

/// True when a function or a block of the file declares a `mod name;`.
fn declares_module_in_block(file: &ast::SourceFile) -> bool {
    file.syntax()
        .descendants()
        .filter_map(ast::Module::cast)
        .filter(|module| module.item_list().is_none())
        .any(|module| {
            module.syntax().parent().is_some_and(|parent| {
                !matches!(
                    parent.kind(),
                    SyntaxKind::SOURCE_FILE | SyntaxKind::ITEM_LIST
                )
            })
        })
}

/// True when the node can hold an include call or macro tokens.
fn is_macro_like(node: &SyntaxNode) -> bool {
    matches!(
        node.kind(),
        SyntaxKind::MACRO_CALL
            | SyntaxKind::MACRO_RULES
            | SyntaxKind::MACRO_DEF
            | SyntaxKind::ATTR
            | SyntaxKind::ERROR
    )
}

/// Records the include calls and the macro token paths in the tokens of the
/// node.
fn scan_tokens(node: &SyntaxNode, folder: &Path, found: &mut Scan) {
    let tokens: Vec<SyntaxToken> = node
        .descendants_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .filter(|token| !token.kind().is_trivia())
        .collect();
    for (index, token) in tokens.iter().enumerate() {
        let rest = &tokens[index + 1..];
        if INCLUDE_MACROS.contains(&token.text()) {
            include_call(token.text(), rest, folder, found);
        }
        if in_macro_tokens(token) {
            macro_tokens(token, rest, found);
            // A macro can give a literal to an include call, so a `.rs`
            // literal in macro tokens counts as an include.
            if token.kind() == SyntaxKind::STRING
                && let Some(value) = string_value(token)
                && is_rust_path(&value)
            {
                found.credited.extend(normalize(&folder.join(value)));
            }
        }
    }
}

/// The `mod name;` declarations of the items, also in inline modules. A
/// `path` in a `cfg_attr` attribute gives one declaration for each value, so
/// each file that a configuration can use counts.
fn collect_declarations(
    items: impl Iterator<Item = ast::Item>,
    inline: &mut Vec<String>,
    found: &mut Scan,
) {
    for item in items {
        let ast::Item::Module(module) = item else {
            continue;
        };
        let Some(name) = module.name() else {
            continue;
        };
        let name = plain_name(&name);
        if let Some(list) = module.item_list() {
            inline.push(name);
            collect_declarations(list.items(), inline, found);
            inline.pop();
            continue;
        }
        let decl = ModuleDecl {
            name,
            inline_path: inline.clone(),
            inline_folder: None,
            path_attr: None,
            under_cfg: false,
            cfg_off: false,
            test_only: false,
        };
        found.modules.extend(decl.for_module(&module));
    }
}

/// True for a token in the token tree of a macro call or a macro definition.
/// The parser does not read the items in such a tree.
fn in_macro_tokens(token: &SyntaxToken) -> bool {
    let mut inside = false;
    for node in token.parent_ancestors() {
        match node.kind() {
            SyntaxKind::TOKEN_TREE => inside = true,
            SyntaxKind::ATTR => return false,
            _ => {}
        }
    }
    inside
}

/// Reads `mod name;`, the other module forms and `path = "..."` in macro
/// tokens. A form that the scan cannot follow makes the file undecided.
fn macro_tokens(token: &SyntaxToken, rest: &[SyntaxToken], found: &mut Scan) {
    let kinds: Vec<SyntaxKind> = rest.iter().take(3).map(SyntaxToken::kind).collect();
    if token.kind() == SyntaxKind::MOD_KW {
        match kinds.as_slice() {
            [SyntaxKind::IDENT, SyntaxKind::SEMICOLON, ..] => {
                let name = rest[0].text();
                found.modules.push(ModuleDecl {
                    name: name.strip_prefix("r#").unwrap_or(name).to_owned(),
                    inline_path: Vec::new(),
                    inline_folder: None,
                    path_attr: None,
                    under_cfg: false,
                    cfg_off: false,
                    test_only: false,
                });
            }
            [SyntaxKind::IDENT, SyntaxKind::L_CURLY, ..] => nested_file_modules(&rest[1..], found),
            [SyntaxKind::DOLLAR, SyntaxKind::IDENT, SyntaxKind::L_CURLY] => {
                nested_file_modules(&rest[2..], found);
            }
            _ => found.undecide_workspace("a macro declares a module with a name from its input"),
        }
        return;
    }
    if token.text() == "path"
        && let [SyntaxKind::EQ, SyntaxKind::STRING, ..] = kinds.as_slice()
        && let Some(value) = string_value(&rest[1])
        && is_rust_path(&value)
    {
        let name = Path::new(&value)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        found.modules.push(ModuleDecl {
            name,
            inline_path: Vec::new(),
            inline_folder: None,
            path_attr: Some(value),
            under_cfg: false,
            cfg_off: false,
            test_only: false,
        });
    }
}

/// Makes the file undecided when the inline module body that starts at the
/// brace declares a module file: the scan does not know the inline folder.
fn nested_file_modules(tokens: &[SyntaxToken], found: &mut Scan) {
    let body = group(tokens);
    let declares_file = body.iter().enumerate().any(|(index, token)| {
        token.kind() == SyntaxKind::MOD_KW
            && body.get(index + 2..).is_some_and(|after| {
                after.first().map(SyntaxToken::kind) != Some(SyntaxKind::L_CURLY)
            })
    });
    if declares_file {
        found.undecide("a macro declares a module file in an inline module");
    }
}

/// Reads one include call that starts after the macro name.
fn include_call(name: &str, rest: &[SyntaxToken], folder: &Path, found: &mut Scan) {
    let [bang, open, ..] = rest else {
        return;
    };
    if bang.kind() != SyntaxKind::BANG
        || !matches!(
            open.kind(),
            SyntaxKind::L_PAREN | SyntaxKind::L_BRACK | SyntaxKind::L_CURLY
        )
    {
        return;
    }
    let arguments = group(&rest[1..]);
    if let [literal] = arguments
        && let Some(value) = string_value(literal)
    {
        let path = normalize(&folder.join(value));
        if name == "include" {
            found.code_includes.extend(path.clone());
        }
        found.includes.extend(path);
        return;
    }
    // A text include of macro input reads a literal of the call site, and
    // the scan counts each `.rs` literal in macro tokens. A code include of
    // macro input can also declare modules, so it stays undecided.
    if arguments
        .iter()
        .any(|token| token.kind() == SyntaxKind::DOLLAR)
    {
        if name == "include" {
            found.undecide_workspace("a macro gives its input to an `include!` call");
        }
        return;
    }
    let text: String = arguments.iter().map(SyntaxToken::text).collect();
    if text.contains(OUT_DIR) {
        return;
    }
    // A computed `include!` can read any source file. A computed text include
    // counts only when it names a `.rs` file.
    if name == "include" || text.contains(".rs\"") {
        found.undecide("an include call computes its path");
    }
}

/// True for a path with the `.rs` extension.
fn is_rust_path(value: &str) -> bool {
    Path::new(value)
        .extension()
        .is_some_and(|extension| extension == "rs")
}

/// The tokens after an opening bracket, up to the matching closing bracket.
fn group(tokens: &[SyntaxToken]) -> &[SyntaxToken] {
    let mut depth = 0_usize;
    for (index, token) in tokens.iter().enumerate() {
        match token.kind() {
            SyntaxKind::L_PAREN | SyntaxKind::L_BRACK | SyntaxKind::L_CURLY => depth += 1,
            SyntaxKind::R_PAREN | SyntaxKind::R_BRACK | SyntaxKind::R_CURLY => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return &tokens[1..index];
                }
            }
            _ => {}
        }
    }
    tokens.get(1..).unwrap_or_default()
}

/// The value of a string literal token, also a raw string.
fn string_value(token: &SyntaxToken) -> Option<String> {
    let literal = ast::String::cast(token.clone())?;
    literal.value().ok().map(std::borrow::Cow::into_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_without_a_module_an_include_or_a_rs_literal_reaches_nothing() {
        assert!(reaches_nothing(
            "// This file is @generated by codegen.rs\npub static WORDS: &[&str] = &[\"include\", \"model\"];\n"
        ));
        assert!(!reaches_nothing("mod tables;\n"));
        assert!(!reaches_nothing("pub(crate) mod\ttables;\n"));
        assert!(!reaches_nothing(
            "const T: &str = include_str!(\"t.txt\");\n"
        ));
        assert!(!reaches_nothing("include !(\"code.rs\");\n"));
        assert!(!reaches_nothing(
            "const B: &[u8] = include_bytes!(\"b.bin\");\n"
        ));
        assert!(!reaches_nothing("m!(\"other.rs\");\n"));
        assert!(!reaches_nothing(
            "const T: &str = include_str /* c */ !(\"a.txt\");\n"
        ));
        assert!(!reaches_nothing(
            "const T: &str = include_str // c\n!(\"a.txt\");\n"
        ));
        assert!(!reaches_nothing("m!(\"a.r\\x73\");\n"));
        assert!(!reaches_nothing("m!(\"a\\u{2e}rs\");\n"));
        assert!(!reaches_nothing("m!(\"a.rs\\\n\");\n"));
        assert!(reaches_nothing("const Q: &str = \"say \\\"hi\\\"\\n\";\n"));
        // A word that only starts or ends with a keyword is no keyword.
        assert!(reaches_nothing(
            "fn modify() { let unmod = 1; let includes = 2; }\n"
        ));
    }

    #[test]
    fn a_text_that_reaches_nothing_scans_to_nothing() {
        let text = "pub static WORDS: &[&str] = &[\"include\", \"x.rs\"];\nfn modify() {}\n";
        // `x.rs` is a literal outside macro tokens, so the scan credits nothing.
        assert_eq!(scan(&parse(text), "src/lib.rs"), Scan::default());
    }

    /// Parses a snippet.
    fn parse(text: &str) -> ast::SourceFile {
        ast::SourceFile::parse(text, ra_ap_syntax::Edition::Edition2024).tree()
    }

    fn names(scan: &Scan) -> Vec<(String, Option<String>)> {
        scan.modules
            .iter()
            .map(|module| (module.name.clone(), module.path_attr.clone()))
            .collect()
    }

    #[test]
    fn macro_module_and_cfg_attr_path_are_declarations() {
        let file = parse(
            "cfg_if! { if #[cfg(unix)] { mod unix; } }\n\
             #[cfg_attr(windows, path = \"win.rs\")]\nmod imp;\n",
        );
        let found = scan(&file, "src/lib.rs");
        assert_eq!(
            names(&found),
            [
                ("imp".to_owned(), None),
                ("imp".to_owned(), Some("win.rs".to_owned())),
                ("unix".to_owned(), None),
            ]
        );
        assert_eq!(found.undecided, None);
    }

    #[test]
    fn include_reads_a_literal_relative_to_the_file() {
        let file = parse(
            "include!(\"a.rs\");\nconst B: &[u8] = include_bytes!(r#\"../b.rs\"#);\n\
             const C: &str = include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/README.md\"));\n",
        );
        let found = scan(&file, "src/x/mod.rs");
        assert_eq!(found.includes, ["src/b.rs", "src/x/a.rs"]);
        assert_eq!(found.undecided, None);
    }

    #[test]
    fn an_include_in_an_attribute_token_tree_counts() {
        let file = parse(
            "#![cfg_attr(docsrs, doc = include_str!(\"extra.rs\"))]\n\
             #[my_attr(include!(\"more.rs\"))]\nfn f() {}\n",
        );
        let found = scan(&file, "src/lib.rs");
        assert_eq!(found.includes, ["src/extra.rs", "src/more.rs"]);
    }

    #[test]
    fn computed_code_include_is_undecided_unless_it_reads_the_build_output() {
        let out = parse("include!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));\n");
        assert_eq!(scan(&out, "src/lib.rs").undecided, None);
        let computed = parse("include!(concat!(\"a\", \".rs\"));\n");
        assert!(scan(&computed, "src/lib.rs").undecided.is_some());
        let generated = parse("macro_rules! m { ($n:ident) => { mod $n; } }\n");
        assert!(scan(&generated, "src/lib.rs").undecided.is_some());
    }

    #[test]
    fn a_name_in_a_string_or_a_comment_is_not_a_declaration() {
        let file = parse("// mod a;\nconst S: &str = \"mod b;\";\n");
        assert!(scan(&file, "src/lib.rs").modules.is_empty());
    }
}
