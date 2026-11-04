//! The scan of one source file for the `files` check.
//!
//! The scan gives the module declarations that the walk follows, the files
//! that a literal `include!`, `include_str!` or `include_bytes!` call reads,
//! and the reason when the syntax cannot tell which files the file reaches.

use std::path::Path;

use ra_ap_syntax::ast::{self, HasAttrs, HasModuleItem, HasName};
use ra_ap_syntax::{AstNode, AstToken, NodeOrToken, SyntaxKind, SyntaxToken};

use crate::attrs::{attr_body, path_attribute, plain_name};
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

/// Scans the parsed file at `path`, relative to the workspace root.
pub fn scan(file: &ast::SourceFile, path: &str) -> Scan {
    let mut found = Scan::default();
    collect_declarations(file.items(), &mut Vec::new(), &mut found);
    let in_block = file
        .syntax()
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
        });
    if in_block {
        found.undecide("a function or a block declares a module");
    }
    let tokens: Vec<SyntaxToken> = file
        .syntax()
        .descendants_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .filter(|token| !token.kind().is_trivia())
        .collect();
    let folder = Path::new(path).parent().unwrap_or_else(|| Path::new(""));
    for (index, token) in tokens.iter().enumerate() {
        let rest = &tokens[index + 1..];
        if INCLUDE_MACROS.contains(&token.text()) {
            include_call(token.text(), rest, folder, &mut found);
        }
        if in_macro_tokens(token) {
            macro_tokens(token, rest, &mut found);
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
    found.includes.sort();
    found.includes.dedup();
    found
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
            let moved = module.attrs().any(|attr| {
                path_attribute(&attr).is_some()
                    || cfg_attr_paths(&attr_body(&attr)).next().is_some()
            });
            if moved {
                found.undecide("a `path` attribute is on an inline module");
            }
            inline.push(name);
            collect_declarations(list.items(), inline, found);
            inline.pop();
            continue;
        }
        let decl = |path_attr: Option<String>| ModuleDecl {
            name: name.clone(),
            inline_path: inline.clone(),
            path_attr,
            under_cfg: false,
            cfg_off: false,
        };
        found
            .modules
            .push(decl(module.attrs().find_map(|attr| path_attribute(&attr))));
        for attr in module.attrs() {
            found
                .modules
                .extend(cfg_attr_paths(&attr_body(&attr)).map(|path| decl(Some(path))));
        }
    }
}

/// The `path = "..."` values in a `cfg_attr(..)` body without whitespace.
fn cfg_attr_paths(body: &str) -> impl Iterator<Item = String> + '_ {
    let inner = body.strip_prefix("cfg_attr(").unwrap_or_default();
    inner.match_indices("path=\"").filter_map(move |(at, key)| {
        let value = &inner[at + key.len()..];
        value.find('"').map(|end| value[..end].to_owned())
    })
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
                    path_attr: None,
                    under_cfg: false,
                    cfg_off: false,
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
            path_attr: Some(value),
            under_cfg: false,
            cfg_off: false,
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
