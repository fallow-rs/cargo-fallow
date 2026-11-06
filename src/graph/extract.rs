//! The facts of one source file: items, imports and references.
//!
//! The walk reads the syntax tree once, in preorder. A stack of frames holds
//! the context of each node: the inline module, the item that owns the code,
//! the `impl` block, a function body and test code. The facts are plain data,
//! because the syntax tree cannot leave the sized stack of the parse.

use ra_ap_syntax::ast::{self, HasAttrs, HasName, HasVisibility, VisibilityKind};
use ra_ap_syntax::{AstNode, AstToken, SyntaxKind, SyntaxNode, WalkEvent};

use crate::attrs::{AttrClass, attr_body, attributes_of, path_attribute, plain_name};
use crate::graph::tokens::{
    OUTPUT_MACROS, TokenPath, paths_in_code, paths_in_tree, rust_code_blocks_at,
};
use crate::modules::ModuleDecl;
use crate::parse::{Edition, ParseOutcome, with_parsed};

/// The kind of an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ItemKind {
    /// A module.
    Module,
    /// A free function.
    Function,
    /// A function in an `impl` or `trait` block.
    Method,
    /// A constant in an `impl` or `trait` block.
    AssocConst,
    /// A type in an `impl` or `trait` block.
    AssocType,
    /// A struct.
    Struct,
    /// An enum.
    Enum,
    /// A union.
    Union,
    /// A trait.
    Trait,
    /// A type alias.
    TypeAlias,
    /// A constant.
    Const,
    /// A static.
    Static,
    /// A field of a struct, a union or an enum variant.
    Field,
    /// An enum variant.
    Variant,
    /// A `use` item with one name.
    Use,
    /// A `macro_rules!` or `macro` definition.
    Macro,
    /// An `impl` block.
    Impl,
}

impl ItemKind {
    /// The name in the report. The names are the same as in hawk.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Module => "module",
            Self::Function => "function",
            Self::Method => "method",
            Self::AssocConst => "associated_const",
            Self::AssocType => "associated_type",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Union => "union",
            Self::Trait => "trait",
            Self::TypeAlias => "type_alias",
            Self::Const => "const",
            Self::Static => "static",
            Self::Field => "field",
            Self::Variant => "variant",
            Self::Use => "reexport",
            Self::Macro => "macro",
            Self::Impl => "impl",
        }
    }
}

/// The written visibility of an item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Vis {
    /// No visibility keyword.
    Private,
    /// `pub`.
    Public,
    /// `pub(crate)`.
    Crate,
    /// `pub(super)`.
    Super,
    /// `pub(self)` or `pub(in path)`.
    In(Vec<String>),
}

impl Vis {
    /// True for `pub(crate)`, `pub(super)` and `pub(in path)`.
    pub const fn is_restricted(&self) -> bool {
        matches!(self, Self::Crate | Self::Super | Self::In(_))
    }
}

/// Attributes and other facts that make an item a root of the liveness walk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ItemFlags {
    /// `#[test]`, the built-in `#[bench]` and the test macros of the wider
    /// ecosystem.
    pub test_fn: bool,
    /// `#[no_mangle]`, `#[export_name]` or `#[used]`.
    pub external: bool,
    /// `#[allow(dead_code)]` or `#[expect(dead_code)]`.
    pub allow_dead: bool,
    /// `#[macro_export]`.
    pub macro_export: bool,
    /// A `#[derive]` attribute is on the item.
    pub derive: bool,
    /// A derive that is not in the standard library is on the item. Only such
    /// a derive can declare helper attributes, for example `#[serde(..)]`.
    pub helper_derive: bool,
    /// The item is in an `impl Trait for Type` block.
    pub trait_impl: bool,
    /// The item is in a `trait` block.
    pub trait_item: bool,
    /// The item is a proc-macro entry point.
    pub proc_macro: bool,
    /// An attribute macro is on the item, for example `#[wasm_bindgen]`. Its
    /// output can use the item from any place.
    pub attr_macro: bool,
}

/// A range of text, with one-based lines and columns in characters. The end
/// is the position after the last character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// The line of the first character.
    pub line: u32,
    /// The column of the first character.
    pub column: u32,
    /// The line of the end.
    pub end_line: u32,
    /// The column of the end.
    pub end_column: u32,
}

/// One item of a file.
#[derive(Debug, Clone)]
pub struct RawItem {
    /// The name. A tuple field has its index as name.
    pub name: String,
    /// The kind.
    pub kind: ItemKind,
    /// The written visibility.
    pub vis: Vis,
    /// The inline modules around the item, outermost first.
    pub inline: Vec<String>,
    /// The index of the enclosing item: the struct of a field, the enum of a
    /// variant, the `impl` or `trait` block of a method.
    pub parent: Option<usize>,
    /// The self type path of an `impl` block.
    pub impl_self: Option<Vec<String>>,
    /// The trait path of an `impl Trait for Type` block.
    pub impl_trait: Option<Vec<String>>,
    /// The one-based line of the name.
    pub line: u32,
    /// The one-based column of the name.
    pub column: u32,
    /// The one-based last line of the item.
    pub end_line: u32,
    /// The written visibility keyword, when the item has one.
    pub vis_span: Option<Span>,
    /// True when the item exists only in test builds.
    pub test_only: bool,
    /// True when a `cfg` attribute other than a test condition is on the item
    /// or on an enclosing node.
    pub under_cfg: bool,
    /// The `cfg` conditions on the item and on each enclosing node of the
    /// file. See [`cfg_conditions`].
    pub cfg: Vec<String>,
    /// The root facts.
    pub flags: ItemFlags,
}

/// The name that a `use` item binds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    /// The last segment, or the name after `as`.
    Name(String),
    /// `as _`: the item is in scope only for its trait methods.
    Underscore,
    /// `*`.
    Glob,
}

/// One flat import: `use a::{b, c::*}` gives two.
#[derive(Debug, Clone)]
pub struct RawUse {
    /// The inline modules around the import.
    pub inline: Vec<String>,
    /// The visibility of the `use` item.
    pub vis: Vis,
    /// The path segments. Keywords stay as text: `crate`, `self`, `super`.
    pub path: Vec<String>,
    /// True when the path starts with `::`.
    pub global: bool,
    /// The binding.
    pub binding: Binding,
    /// The index of the item of a named `pub` re-export.
    pub item: Option<usize>,
    /// The one-based line of the `use` item.
    pub line: u32,
    /// The `cfg` conditions on the `use` item and on each enclosing node of
    /// the file.
    pub cfg: Vec<String>,
}

/// The way that code names something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefKind {
    /// A path in code.
    Path {
        /// The segments.
        segments: Vec<String>,
        /// True when the path starts with `::`.
        global: bool,
    },
    /// A method call `.name(..)`. The receiver type is unknown.
    Method(String),
    /// A field access `.name`. The receiver type is unknown.
    Field(String),
    /// A field in a struct expression or pattern.
    RecordField {
        /// The path of the struct or the variant.
        segments: Vec<String>,
        /// True when the path starts with `::`.
        global: bool,
        /// The field name.
        field: String,
    },
    /// A macro call `name!`.
    Macro(String),
    /// A name in tokens that can be a field or a method.
    Member(String),
}

/// Where a reference is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// In the body of a function or the value of a constant.
    Body,
    /// In a signature: parameter and return types, field types, bounds.
    Signature,
    /// In the tokens of a macro call or a `macro_rules!` body.
    Macro,
    /// In the tokens of a macro call or a macro body that can write a `pub`
    /// item: the tokens have `pub` or a `vis` fragment.
    PublicMacro,
    /// In the tokens of an attribute.
    Attr,
    /// In a code block of the documentation.
    Doc,
    /// In an intra-doc link, for example `` [`path`] ``. rustdoc resolves it
    /// from the module of the doc comment.
    DocLink,
}

/// One reference.
#[derive(Debug, Clone)]
pub struct RawRef {
    /// The inline modules around the reference.
    pub inline: Vec<String>,
    /// The index of the item that holds the reference.
    pub owner: Option<usize>,
    /// The reference.
    pub kind: RefKind,
    /// Where the reference is.
    pub origin: Origin,
    /// True in test code.
    pub test_only: bool,
    /// True when a `cfg` that can be off in a build with all features and
    /// tests is on an enclosing node.
    pub under_cfg: bool,
    /// The one-based line where the syntax node of the reference starts: the
    /// path, the method call, the macro call or the attribute owner. Zero when
    /// the place is not known, for a code block of an included Markdown file.
    pub line: u32,
    /// The `cfg` conditions on each enclosing node of the file.
    pub cfg: Vec<String>,
    /// True for a name in the tokens of an output macro such as `quote!`,
    /// but not for a `#name` variable.
    pub output: bool,
}

/// The facts of one file.
#[derive(Debug, Clone, Default)]
pub struct FileFacts {
    /// The items.
    pub items: Vec<RawItem>,
    /// The imports.
    pub uses: Vec<RawUse>,
    /// The references.
    pub refs: Vec<RawRef>,
    /// The `mod name;` declarations, also in test code.
    pub modules: Vec<ModuleDecl>,
    /// The files that `#[doc = include_str!(..)]` reads, relative to this file.
    pub doc_includes: Vec<String>,
    /// True when an inner attribute of the file allows the `dead_code` lint.
    pub allow_dead: bool,
    /// The `include!("path")` calls with one string literal.
    pub code_includes: Vec<CodeInclude>,
    /// The files that `#[doc = include_str!(..)]` reads, for their intra-doc
    /// links.
    pub doc_link_includes: Vec<DocInclude>,
}

/// A documentation file that an item includes. Its intra-doc links resolve
/// from the module of the item.
#[derive(Debug, Clone)]
pub struct DocInclude {
    /// The path in the string literal, relative to the file of the item.
    pub path: String,
    /// True when the path is relative to the folder of the package, through
    /// `env!("CARGO_MANIFEST_DIR")`.
    pub from_package: bool,
    /// The reference of each link, without its path.
    pub site: RawRef,
}

/// An `include!` call with one string literal: the compiler reads the file
/// relative to the file of the call.
#[derive(Debug, Clone)]
pub struct CodeInclude {
    /// The path in the string literal.
    pub path: String,
    /// The `include` macro reference at the call. The graph uses it when it
    /// cannot read or parse the file.
    pub site: RawRef,
}

/// Reads the facts of one file.
pub fn extract(source: &str, edition: Edition) -> Result<FileFacts, ParseOutcome> {
    extract_and(source, edition, |_| ()).map(|(facts, ())| facts)
}

/// The facts of a file, and the result of `also` on the same syntax tree. A
/// second reader of the file then needs no second parse.
pub fn extract_and<T: Send>(
    source: &str,
    edition: Edition,
    also: impl FnOnce(&ast::SourceFile) -> T + Send,
) -> Result<(FileFacts, T), ParseOutcome> {
    with_parsed(source, edition, |file, text| {
        let mut walker = Walker::new(text, edition);
        walker.walk(file.syntax());
        walker.docs(file.syntax());
        walker.facts.allow_dead = inner_allows_dead(file.syntax());
        let other = also(file);
        (walker.facts, other)
    })
}

/// The documentation code blocks of a Markdown file, as references.
pub fn doc_refs_of_markdown(markdown: &str, edition: Edition) -> Vec<RawRef> {
    doc_refs_at(markdown, edition, 0)
}

/// The documentation code blocks of a Markdown text that starts on `line`,
/// as references on the first code line of each block. A `line` of zero
/// means that the place is not known, and each reference gets zero.
fn doc_refs_at(markdown: &str, edition: Edition, line: u32) -> Vec<RawRef> {
    rust_code_blocks_at(markdown)
        .iter()
        .flat_map(|(first, code)| {
            let at = if line == 0 {
                0
            } else {
                line.saturating_add(*first)
            };
            paths_in_code(code, edition)
                .into_iter()
                .flat_map(move |path| doc_ref(path, at))
        })
        .collect()
}

fn doc_ref(path: TokenPath, line: u32) -> Vec<RawRef> {
    token_ref_kinds(path)
        .into_iter()
        .map(|kind| RawRef {
            inline: Vec::new(),
            owner: None,
            kind,
            origin: Origin::Doc,
            test_only: true,
            under_cfg: false,
            line,
            cfg: Vec::new(),
            output: false,
        })
        .collect()
}

/// The references of one token path: the path, and a member reference for a
/// name that can be a field or a method.
fn token_ref_kinds(path: TokenPath) -> Vec<RefKind> {
    if let (Some(record), Some(field)) = (path.record, path.segments.first()) {
        return vec![
            RefKind::Path {
                segments: path.segments.clone(),
                global: false,
            },
            RefKind::RecordField {
                segments: record,
                global: path.global,
                field: field.clone(),
            },
        ];
    }
    let member = path
        .member
        .then(|| path.segments.first().cloned())
        .flatten()
        .map(RefKind::Member);
    let mut kinds = vec![RefKind::Path {
        segments: path.segments,
        global: path.global,
    }];
    kinds.extend(member);
    kinds
}

#[derive(Debug, Clone)]
enum Frame {
    Module(String),
    Owner(usize),
    Impl(Option<Vec<String>>),
    Body,
    Test,
    Cfg,
    /// A `cfg` that can be off in a build with all features and tests.
    CfgUnsure,
    /// The `cfg` conditions of a node.
    Conditions(Vec<String>),
    LocalItems,
}

struct Walker<'a> {
    facts: FileFacts,
    lines: Vec<usize>,
    text: &'a str,
    edition: Edition,
    frames: Vec<(SyntaxNode, Frame)>,
    /// The start offset of the node that the walk is in.
    cursor: usize,
}

impl<'a> Walker<'a> {
    fn new(text: &'a str, edition: Edition) -> Self {
        let mut lines = vec![0];
        lines.extend(text.match_indices('\n').map(|(at, _)| at + 1));
        Self {
            facts: FileFacts::default(),
            lines,
            text,
            edition,
            frames: Vec::new(),
            cursor: 0,
        }
    }

    fn walk(&mut self, root: &SyntaxNode) {
        for event in root.preorder() {
            match event {
                WalkEvent::Enter(node) => self.enter(&node),
                WalkEvent::Leave(node) => {
                    while self.frames.last().is_some_and(|(owner, _)| *owner == node) {
                        self.frames.pop();
                    }
                }
            }
        }
    }

    fn push(&mut self, node: &SyntaxNode, frame: Frame) {
        self.frames.push((node.clone(), frame));
    }

    fn enter(&mut self, node: &SyntaxNode) {
        self.cursor = usize::from(node.text_range().start());
        let first_item = self.facts.items.len();
        if let Some(owner) = ast::AnyHasAttrs::cast(node.clone()) {
            self.attr_frames(node, &owner);
        }
        if Self::is_body(node) {
            self.push(node, Frame::Body);
        }
        self.kind_facts(node);
        self.doc_links(node, first_item);
    }

    /// Pushes the frames that the attributes of the node open, and records
    /// the references in the attributes.
    fn attr_frames(&mut self, node: &SyntaxNode, owner: &ast::AnyHasAttrs) {
        match attributes_of(node) {
            AttrClass::Test => self.push(node, Frame::Test),
            AttrClass::Cfg => self.push(node, Frame::Cfg),
            AttrClass::Other => {}
        }
        if cfg_can_be_off(owner) {
            self.push(node, Frame::CfgUnsure);
        }
        let conditions = cfg_conditions(owner);
        if !conditions.is_empty() {
            self.push(node, Frame::Conditions(conditions));
        }
        self.attr_refs(owner);
    }

    /// Reads the facts of the node for its syntax kind.
    fn kind_facts(&mut self, node: &SyntaxNode) {
        match node.kind() {
            SyntaxKind::STMT_LIST => self.push(node, Frame::LocalItems),
            SyntaxKind::USE => self.use_item(node),
            SyntaxKind::EXTERN_CRATE => self.extern_crate(node),
            SyntaxKind::MODULE => self.module(node),
            SyntaxKind::IMPL => self.impl_block(node),
            SyntaxKind::RECORD_FIELD | SyntaxKind::TUPLE_FIELD => self.field(node),
            SyntaxKind::VARIANT => self.variant(node),
            SyntaxKind::PATH => self.path(node),
            SyntaxKind::METHOD_CALL_EXPR => self.method_call(node),
            SyntaxKind::FIELD_EXPR => self.field_access(node),
            SyntaxKind::RECORD_EXPR_FIELD | SyntaxKind::RECORD_PAT_FIELD => self.record_field(node),
            SyntaxKind::RECORD_EXPR_FIELD_LIST => self.record_spread(node),
            SyntaxKind::MACRO_CALL => self.macro_call(node),
            SyntaxKind::MACRO_RULES | SyntaxKind::MACRO_DEF => self.macro_def(node),
            _ => self.plain_item(node),
        }
    }

    /// The intra-doc links of the documentation of the node: its doc comments
    /// and `#[doc]` attributes. For a module, the inner documentation of an
    /// inline module counts too. The item of the node owns the links. The
    /// inner documentation of a file has no owner: the graph gives it to the
    /// `mod` item of the file.
    fn doc_links(&mut self, node: &SyntaxNode, first_item: usize) {
        let file = node.kind() == SyntaxKind::SOURCE_FILE;
        if !file && self.facts.items.len() == first_item {
            return;
        }
        let owner = (!file).then_some(first_item);
        let mut docs = DocText::default();
        self.read_docs(node, &mut docs);
        if let Some(list) = ast::Module::cast(node.clone()).and_then(|module| module.item_list()) {
            self.read_docs(list.syntax(), &mut docs);
        }
        if docs.text.is_empty() && docs.includes.is_empty() {
            return;
        }
        let scopes = self.doc_scopes(node);
        let site = |inline: &[String], line: u32, kind: RefKind| RawRef {
            inline: inline.to_vec(),
            owner,
            kind,
            origin: Origin::DocLink,
            test_only: self.test_only(),
            under_cfg: self.cfg_off(),
            line,
            cfg: self.conditions(),
            output: false,
        };
        let self_path = self.doc_self_path(node);
        let mut refs = Vec::new();
        for (index, (mut segments, global)) in crate::graph::doc_links::link_paths(&docs.text) {
            if let Some(self_path) = &self_path
                && segments.first().is_some_and(|first| first == "Self")
            {
                segments.splice(0..1, self_path.iter().cloned());
            }
            let line = docs.lines.get(index).copied().unwrap_or(0);
            for scope in &scopes {
                let kind = RefKind::Path {
                    segments: segments.clone(),
                    global,
                };
                refs.push(site(scope, line, kind));
            }
        }
        let mut includes = Vec::new();
        for (path, line, from_package) in docs.includes {
            for scope in &scopes {
                let kind = RefKind::Path {
                    segments: Vec::new(),
                    global: false,
                };
                includes.push(DocInclude {
                    path: path.clone(),
                    from_package,
                    site: site(scope, line, kind),
                });
            }
        }
        self.facts.refs.extend(refs);
        self.facts.doc_link_includes.extend(includes);
    }

    /// The inline module paths that the doc links of the node resolve from.
    /// The syntax cannot show if the documentation of a module resolves from
    /// the module or from its parent, so each scope gets the links.
    fn doc_scopes(&self, node: &SyntaxNode) -> Vec<Vec<String>> {
        let here = self.inline();
        let Some(name) = ast::Module::cast(node.clone()).and_then(|module| module.name()) else {
            return vec![here];
        };
        let mut base = here;
        if self.frames.last().is_some_and(|(frame_node, frame)| {
            frame_node == node && matches!(frame, Frame::Module(_))
        }) {
            base.pop();
        }
        let mut inside = base.clone();
        inside.push(plain_name(&name));
        vec![base, inside]
    }

    /// The path of `Self` in the doc links of the node: the self type of the
    /// `impl` block, or the documented type. For a field or a variant, the
    /// type is the enclosing item.
    fn doc_self_path(&self, node: &SyntaxNode) -> Option<Vec<String>> {
        self.impl_self().or_else(|| {
            node.ancestors()
                .find(|ancestor| {
                    matches!(
                        ancestor.kind(),
                        SyntaxKind::STRUCT
                            | SyntaxKind::ENUM
                            | SyntaxKind::UNION
                            | SyntaxKind::TRAIT
                    )
                })
                .and_then(ast::AnyHasName::cast)
                .and_then(|item| item.name())
                .map(|name| vec![plain_name(&name)])
        })
    }

    /// Adds the doc comments and the `#[doc]` attributes that are direct
    /// children of the node.
    fn read_docs(&self, node: &SyntaxNode, docs: &mut DocText) {
        for child in node.children() {
            let line = self.line_at(usize::from(child.text_range().start()));
            match child.kind() {
                SyntaxKind::DOC_COMMENT => docs.push(&child.text().to_string(), line),
                SyntaxKind::ATTR => {
                    if let Some(meta) = ast::Attr::cast(child).and_then(|attr| attr.meta()) {
                        doc_meta(meta, line, docs);
                    }
                }
                _ => {}
            }
        }
    }

    /// True for the body of a function or the value of a constant or a static.
    fn is_body(node: &SyntaxNode) -> bool {
        let Some(parent) = node.parent() else {
            return false;
        };
        if let Some(function) = ast::Fn::cast(parent.clone()) {
            return function.body().is_some_and(|body| body.syntax() == node);
        }
        if let Some(constant) = ast::Const::cast(parent.clone()) {
            return constant.body().is_some_and(|body| body.syntax() == node);
        }
        ast::Static::cast(parent)
            .is_some_and(|item| item.body().is_some_and(|body| body.syntax() == node))
    }

    fn inline(&self) -> Vec<String> {
        self.frames
            .iter()
            .filter_map(|(_, frame)| match frame {
                Frame::Module(name) => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    fn test_only(&self) -> bool {
        self.frames
            .iter()
            .any(|(_, frame)| matches!(frame, Frame::Test))
    }

    /// True under a `cfg` that can be off in a build with all features and tests.
    fn cfg_off(&self) -> bool {
        self.frames
            .iter()
            .any(|(_, frame)| matches!(frame, Frame::CfgUnsure))
    }

    /// The `cfg` conditions of each enclosing node, outermost first.
    fn conditions(&self) -> Vec<String> {
        self.frames
            .iter()
            .filter_map(|(_, frame)| match frame {
                Frame::Conditions(list) => Some(list.iter().cloned()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    fn under_cfg(&self) -> bool {
        self.frames
            .iter()
            .any(|(_, frame)| matches!(frame, Frame::Cfg))
    }

    fn owner(&self) -> Option<usize> {
        self.frames.iter().rev().find_map(|(_, frame)| match frame {
            Frame::Owner(index) => Some(*index),
            _ => None,
        })
    }

    /// True inside a block, where an item is local and not a candidate.
    fn in_local_scope(&self) -> bool {
        self.frames
            .iter()
            .any(|(_, frame)| matches!(frame, Frame::LocalItems))
    }

    fn in_body(&self) -> bool {
        for (_, frame) in self.frames.iter().rev() {
            match frame {
                Frame::Body => return true,
                Frame::Owner(_) => return false,
                _ => {}
            }
        }
        false
    }

    fn impl_self(&self) -> Option<Vec<String>> {
        self.frames
            .iter()
            .rev()
            .find_map(|(_, frame)| match frame {
                Frame::Impl(self_ty) => Some(self_ty.clone()),
                _ => None,
            })?
    }

    /// The one-based line of a byte offset.
    fn line_at(&self, offset: usize) -> u32 {
        let line = self.lines.partition_point(|&start| start <= offset);
        u32::try_from(line).unwrap_or(u32::MAX)
    }

    fn line_col(&self, offset: usize) -> (u32, u32) {
        let line = self.lines.partition_point(|&start| start <= offset);
        let start = self.lines.get(line.saturating_sub(1)).copied().unwrap_or(0);
        let column = self
            .text
            .get(start..offset)
            .map_or(0, |part| part.chars().count());
        (
            u32::try_from(line).unwrap_or(u32::MAX),
            u32::try_from(column + 1).unwrap_or(u32::MAX),
        )
    }

    fn add_item(&mut self, node: &SyntaxNode, name: String, kind: ItemKind, vis: Vis) -> usize {
        let name_offset = node
            .children()
            .find(|child| child.kind() == SyntaxKind::NAME)
            .map_or_else(
                || node.text_range().start(),
                |child| child.text_range().start(),
            );
        self.add_item_at(node, usize::from(name_offset), name, kind, vis)
    }

    /// Adds an item with its name at `name_offset`.
    fn add_item_at(
        &mut self,
        node: &SyntaxNode,
        name_offset: usize,
        name: String,
        kind: ItemKind,
        vis: Vis,
    ) -> usize {
        let (line, column) = self.line_col(name_offset);
        let (end_line, _) = self.line_col(usize::from(node.text_range().end()));
        let vis_span = ast::AnyHasVisibility::cast(node.clone())
            .and_then(|owner| owner.visibility())
            .map(|keyword| {
                let range = keyword.syntax().text_range();
                let (line, column) = self.line_col(usize::from(range.start()));
                let (end_line, end_column) = self.line_col(usize::from(range.end()));
                Span {
                    line,
                    column,
                    end_line,
                    end_column,
                }
            });
        let flags = self.flags_of(node);
        let parent = self.owner();
        let index = self.facts.items.len();
        self.facts.items.push(RawItem {
            name,
            kind,
            vis,
            inline: self.inline(),
            parent,
            impl_self: None,
            impl_trait: None,
            line,
            column,
            end_line,
            vis_span,
            test_only: self.test_only(),
            under_cfg: self.under_cfg(),
            cfg: self.conditions(),
            flags,
        });
        index
    }

    fn flags_of(&self, node: &SyntaxNode) -> ItemFlags {
        let mut flags = ItemFlags::default();
        let mut unknown_attr = false;
        if let Some(owner) = ast::AnyHasAttrs::cast(node.clone()) {
            for attr in owner.attrs() {
                let body = attr_body(&attr);
                let name = body.split(['(', '=']).next().unwrap_or(&body);
                let last = name.rsplit("::").next().unwrap_or(name);
                flags.test_fn |= crate::attrs::is_test_attribute(&body);
                flags.external |= matches!(last, "no_mangle" | "export_name" | "used")
                    || body.starts_with("unsafe(no_mangle")
                    || body.starts_with("unsafe(export_name");
                flags.allow_dead |= allows_dead(&body);
                flags.macro_export |= last == "macro_export";
                flags.derive |= last == "derive";
                flags.helper_derive |= last == "derive" && derive_has_helpers(&body);
                flags.proc_macro |= matches!(
                    last,
                    "proc_macro" | "proc_macro_derive" | "proc_macro_attribute"
                );
                unknown_attr |= !is_builtin_attribute(name);
            }
        }
        // A derive helper such as `#[serde(..)]` needs a derive on the item or
        // on its parent, so it cannot be on a function, a module or an `impl`.
        let can_have_helpers = matches!(
            node.kind(),
            SyntaxKind::STRUCT
                | SyntaxKind::ENUM
                | SyntaxKind::UNION
                | SyntaxKind::RECORD_FIELD
                | SyntaxKind::TUPLE_FIELD
                | SyntaxKind::VARIANT
        );
        // A derive of the standard library declares no helper, so an unknown
        // attribute next to it is an attribute macro, for example
        // `#[napi(object)]` with `#[derive(Default)]`.
        let parent_helpers = self
            .owner()
            .is_some_and(|parent| self.facts.items[parent].flags.helper_derive);
        flags.attr_macro =
            unknown_attr && !(can_have_helpers && (flags.helper_derive || parent_helpers));
        if let Some(list) = ast::Module::cast(node.clone()).and_then(|module| module.item_list()) {
            flags.allow_dead |= inner_allows_dead(list.syntax());
        }
        for (_, frame) in self.frames.iter().rev() {
            if let Frame::Owner(index) = frame {
                let parent = &self.facts.items[*index];
                flags.trait_impl |= parent.kind == ItemKind::Impl && parent.impl_trait.is_some();
                flags.trait_item |= parent.kind == ItemKind::Trait;
                break;
            }
        }
        flags
    }

    fn plain_item(&mut self, node: &SyntaxNode) {
        if self.in_local_scope() {
            return;
        }
        let in_block = node.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                SyntaxKind::SOURCE_FILE | SyntaxKind::ITEM_LIST | SyntaxKind::ASSOC_ITEM_LIST
            )
        });
        if !in_block {
            return;
        }
        let in_assoc = node
            .parent()
            .is_some_and(|parent| parent.kind() == SyntaxKind::ASSOC_ITEM_LIST);
        let kind = match node.kind() {
            SyntaxKind::FN if in_assoc => ItemKind::Method,
            SyntaxKind::FN => ItemKind::Function,
            SyntaxKind::CONST if in_assoc => ItemKind::AssocConst,
            SyntaxKind::CONST => ItemKind::Const,
            SyntaxKind::TYPE_ALIAS if in_assoc => ItemKind::AssocType,
            SyntaxKind::TYPE_ALIAS => ItemKind::TypeAlias,
            SyntaxKind::STRUCT => ItemKind::Struct,
            SyntaxKind::ENUM => ItemKind::Enum,
            SyntaxKind::UNION => ItemKind::Union,
            SyntaxKind::TRAIT => ItemKind::Trait,
            SyntaxKind::STATIC => ItemKind::Static,
            _ => return,
        };
        let Some(named) = ast::AnyHasName::cast(node.clone()) else {
            return;
        };
        let Some(name) = named.name() else {
            return;
        };
        let vis = vis_of(node);
        let index = self.add_item(node, plain_name(&name), kind, vis);
        self.push(node, Frame::Owner(index));
    }

    fn module(&mut self, node: &SyntaxNode) {
        let Some(module) = ast::Module::cast(node.clone()) else {
            return;
        };
        let Some(name) = module.name() else {
            return;
        };
        let name = plain_name(&name);
        if self.in_local_scope() {
            return;
        }
        self.add_item(node, name.clone(), ItemKind::Module, vis_of(node));
        if module.item_list().is_some() {
            self.push(node, Frame::Module(name));
            return;
        }
        let under_cfg = self.under_cfg();
        self.facts.modules.push(ModuleDecl {
            name,
            inline_path: self.inline(),
            path_attr: module.attrs().find_map(|attr| path_attribute(&attr)),
            under_cfg,
            cfg_off: self.cfg_off(),
            test_only: false,
        });
    }

    fn impl_block(&mut self, node: &SyntaxNode) {
        let Some(block) = ast::Impl::cast(node.clone()) else {
            return;
        };
        let self_ty = block.self_ty().and_then(|ty| type_path(&ty));
        let trait_ty = block.trait_().and_then(|ty| type_path(&ty));
        if self.in_local_scope() {
            self.push(node, Frame::Impl(self_ty));
            return;
        }
        let index = self.add_item(node, String::new(), ItemKind::Impl, Vis::Private);
        self.facts.items[index].impl_self.clone_from(&self_ty);
        self.facts.items[index].impl_trait = trait_ty;
        self.push(node, Frame::Impl(self_ty));
        self.push(node, Frame::Owner(index));
    }

    fn field(&mut self, node: &SyntaxNode) {
        if self.in_local_scope() || self.owner().is_none() {
            return;
        }
        let name = match ast::RecordField::cast(node.clone()) {
            Some(field) => match field.name() {
                Some(name) => plain_name(&name),
                None => return,
            },
            None => node
                .parent()
                .map(|list| {
                    list.children()
                        .take_while(|child| child != node)
                        .count()
                        .to_string()
                })
                .unwrap_or_default(),
        };
        let index = self.add_item(node, name, ItemKind::Field, vis_of(node));
        self.push(node, Frame::Owner(index));
    }

    fn variant(&mut self, node: &SyntaxNode) {
        if self.in_local_scope() || self.owner().is_none() {
            return;
        }
        let Some(name) = ast::Variant::cast(node.clone()).and_then(|variant| variant.name()) else {
            return;
        };
        let index = self.add_item(node, plain_name(&name), ItemKind::Variant, Vis::Public);
        self.push(node, Frame::Owner(index));
    }

    fn use_item(&mut self, node: &SyntaxNode) {
        let Some(item) = ast::Use::cast(node.clone()) else {
            return;
        };
        let Some(tree) = item.use_tree() else {
            return;
        };
        let vis = vis_of(node);
        let local = self.in_local_scope();
        let mut flat = Vec::new();
        flatten_use(&tree, &[], false, &mut flat);
        for (path, global, binding, leaf) in flat {
            let reexport = matches!(vis, Vis::Public) && !local;
            let item_index = match (&binding, reexport) {
                (Binding::Name(name), true) => Some(self.add_item_at(
                    node,
                    leaf_name_offset(&leaf),
                    name.clone(),
                    ItemKind::Use,
                    vis.clone(),
                )),
                _ => None,
            };
            self.facts.uses.push(RawUse {
                inline: self.inline(),
                vis: vis.clone(),
                path,
                global,
                binding,
                item: item_index,
                line: self.line_at(self.cursor),
                cfg: self.conditions(),
            });
        }
    }

    /// `extern crate name as alias;` binds the crate in the module, the same as
    /// an import.
    fn extern_crate(&mut self, node: &SyntaxNode) {
        let Some(item) = ast::ExternCrate::cast(node.clone()) else {
            return;
        };
        let Some(name) = item.name_ref() else {
            return;
        };
        let name = name.text().to_string();
        let (path, global) = if name == "self" {
            (vec!["crate".to_owned()], false)
        } else {
            (vec![name.clone()], true)
        };
        let binding = match item.rename() {
            Some(rename) if rename.underscore_token().is_some() => Binding::Underscore,
            Some(rename) => match rename.name() {
                Some(alias) => Binding::Name(plain_name(&alias)),
                None => return,
            },
            None => Binding::Name(name),
        };
        self.facts.uses.push(RawUse {
            inline: self.inline(),
            vis: vis_of(node),
            path,
            global,
            binding,
            item: None,
            line: self.line_at(self.cursor),
            cfg: self.conditions(),
        });
    }

    fn add_ref(&mut self, kind: RefKind, origin: Origin) {
        let origin = match origin {
            Origin::Body if !self.in_body() => Origin::Signature,
            other => other,
        };
        self.facts.refs.push(RawRef {
            inline: self.inline(),
            owner: self.owner(),
            kind,
            origin,
            test_only: self.test_only(),
            under_cfg: self.cfg_off(),
            line: self.line_at(self.cursor),
            cfg: self.conditions(),
            output: false,
        });
    }

    fn path(&mut self, node: &SyntaxNode) {
        let Some(path) = ast::Path::cast(node.clone()) else {
            return;
        };
        let parent = node.parent().map(|parent| parent.kind());
        if matches!(
            parent,
            Some(SyntaxKind::PATH_META | SyntaxKind::TOKEN_TREE_META | SyntaxKind::KEY_VALUE_META)
        ) {
            self.attribute_path(&path);
            return;
        }
        if matches!(
            parent,
            Some(
                SyntaxKind::PATH
                    | SyntaxKind::USE_TREE
                    | SyntaxKind::VISIBILITY
                    | SyntaxKind::MACRO_CALL
            )
        ) {
            return;
        }
        let (segments, global) = self.segments_of(&path);
        if !segments.is_empty() {
            self.add_ref(RefKind::Path { segments, global }, Origin::Body);
        }
    }

    /// The path of an attribute, for example `#[tokio::main]`, is a reference
    /// when it has a crate prefix. A single name needs an import, which is a
    /// reference already. The attributes of the compiler and its tools are not.
    fn attribute_path(&mut self, path: &ast::Path) {
        let (segments, global) = path_segments(path);
        if segments.len() > 1 && !is_builtin_attribute(&segments.join("::")) {
            self.add_ref(RefKind::Path { segments, global }, Origin::Attr);
        }
    }

    /// The segments of a path. `Self` becomes the path of the self type of the
    /// enclosing `impl` block, when that type is a plain path.
    fn segments_of(&self, path: &ast::Path) -> (Vec<String>, bool) {
        let (mut segments, global) = path_segments(path);
        if segments.first().is_some_and(|first| first == "Self")
            && let Some(self_ty) = self.impl_self()
        {
            segments.splice(0..1, self_ty);
        }
        (segments, global)
    }

    fn method_call(&mut self, node: &SyntaxNode) {
        if let Some(name) = ast::MethodCallExpr::cast(node.clone()).and_then(|call| call.name_ref())
        {
            // A method chain spans lines: the place is the name, not the receiver.
            self.cursor = usize::from(name.syntax().text_range().start());
            self.add_ref(RefKind::Method(name.text().to_string()), Origin::Body);
        }
    }

    fn field_access(&mut self, node: &SyntaxNode) {
        if let Some(name) = ast::FieldExpr::cast(node.clone()).and_then(|access| access.name_ref())
        {
            self.cursor = usize::from(name.syntax().text_range().start());
            self.add_ref(RefKind::Field(name.text().to_string()), Origin::Body);
        }
    }

    fn record_field(&mut self, node: &SyntaxNode) {
        let (field, path) = if let Some(field) = ast::RecordExprField::cast(node.clone()) {
            let record = node.ancestors().find_map(ast::RecordExpr::cast);
            (
                field.field_name().map(|name| name.text().to_string()),
                record.and_then(|record| record.path()),
            )
        } else if let Some(field) = ast::RecordPatField::cast(node.clone()) {
            let record = node.ancestors().find_map(ast::RecordPat::cast);
            let name = field.field_name().map(|name| match name {
                ast::NameOrNameRef::Name(name) => name.text().to_string(),
                ast::NameOrNameRef::NameRef(name) => name.text().to_string(),
            });
            (name, record.and_then(|record| record.path()))
        } else {
            return;
        };
        let Some(field) = field else {
            return;
        };
        let (segments, global) = path.map(|path| self.segments_of(&path)).unwrap_or_default();
        if segments.is_empty() {
            self.add_ref(RefKind::Field(field), Origin::Body);
            return;
        }
        self.add_ref(
            RefKind::RecordField {
                segments,
                global,
                field,
            },
            Origin::Body,
        );
    }

    /// Struct update syntax, `S { a, ..base }`, reads each field of `S`, so
    /// each field must be visible here. The field name `..` stands for all
    /// fields.
    fn record_spread(&mut self, node: &SyntaxNode) {
        let Some(list) = ast::RecordExprFieldList::cast(node.clone()) else {
            return;
        };
        if list.dotdot_token().is_none() {
            return;
        }
        let Some(path) = node
            .parent()
            .and_then(ast::RecordExpr::cast)
            .and_then(|record| record.path())
        else {
            return;
        };
        let (segments, global) = self.segments_of(&path);
        if segments.is_empty() {
            return;
        }
        self.add_ref(
            RefKind::RecordField {
                segments,
                global,
                field: ALL_FIELDS.to_owned(),
            },
            Origin::Body,
        );
    }

    fn macro_call(&mut self, node: &SyntaxNode) {
        let Some(call) = ast::MacroCall::cast(node.clone()) else {
            return;
        };
        let mut quoted = false;
        if let Some(path) = call.path()
            && let Some(name) = path.segment().and_then(|segment| segment.name_ref())
        {
            quoted = OUTPUT_MACROS.contains(&name.text());
            let (segments, global) = path_segments(&path);
            if segments.len() > 1 {
                self.add_ref(RefKind::Path { segments, global }, Origin::Macro);
            }
            self.add_ref(RefKind::Macro(name.text().to_string()), Origin::Macro);
            if name.text() == "include"
                && let Some(file) = call.token_tree().as_ref().and_then(single_string)
                && let Some(site) = self.facts.refs.pop()
            {
                self.facts
                    .code_includes
                    .push(CodeInclude { path: file, site });
            }
        }
        if let Some(tree) = call.token_tree() {
            let origin = if writes_pub(tree.syntax()) {
                Origin::PublicMacro
            } else {
                Origin::Macro
            };
            self.token_refs(tree.syntax(), origin, quoted);
        }
    }

    fn macro_def(&mut self, node: &SyntaxNode) {
        let name = ast::AnyHasName::cast(node.clone()).and_then(|named| named.name());
        let Some(name) = name else {
            return;
        };
        let index = self.add_item(node, plain_name(&name), ItemKind::Macro, vis_of(node));
        self.push(node, Frame::Owner(index));
        let tree = node
            .children()
            .filter(|child| child.kind() == SyntaxKind::TOKEN_TREE)
            .collect::<Vec<_>>();
        for tree in tree {
            let origin = if writes_pub(&tree) {
                Origin::PublicMacro
            } else {
                Origin::Macro
            };
            self.token_refs(&tree, origin, false);
        }
    }

    /// The references of the names in a token tree. With `quoted` true, the
    /// tree is the tokens of an output macro such as `quote!`.
    fn token_refs(&mut self, tree: &SyntaxNode, origin: Origin, quoted: bool) {
        for mut path in paths_in_tree(tree) {
            if let (Some(first), Some(self_ty)) = (path.segments.first(), self.impl_self())
                && first == "Self"
            {
                let mut segments = self_ty;
                segments.extend(path.segments.into_iter().skip(1));
                path.segments = segments;
            }
            let output = (quoted || path.quoted) && !path.interpolated;
            for kind in token_ref_kinds(path) {
                self.add_ref(kind, origin);
                if let Some(reference) = self.facts.refs.last_mut() {
                    reference.output = output;
                }
            }
        }
    }

    fn attr_refs(&mut self, owner: &ast::AnyHasAttrs) {
        let start = self.cursor;
        for attr in owner.attrs() {
            self.cursor = usize::from(attr.syntax().text_range().start());
            if let Some(meta) = attr.meta() {
                self.meta_refs(meta);
            }
        }
        self.cursor = start;
    }

    /// The references of one attribute. The attributes inside `cfg_attr(..)`
    /// and `unsafe(..)` count, because one `cfg` branch can apply them.
    fn meta_refs(&mut self, meta: ast::Meta) {
        match meta {
            ast::Meta::TokenTreeMeta(meta) => {
                if let Some(tree) = meta.token_tree() {
                    self.token_refs(tree.syntax(), Origin::Attr, false);
                }
            }
            ast::Meta::KeyValueMeta(meta) => self.key_value(&meta),
            ast::Meta::CfgAttrMeta(meta) => {
                for inner in meta.metas() {
                    self.meta_refs(inner);
                }
            }
            ast::Meta::UnsafeMeta(meta) => {
                if let Some(inner) = meta.meta() {
                    self.meta_refs(inner);
                }
            }
            ast::Meta::CfgMeta(_) | ast::Meta::PathMeta(_) => {}
        }
    }

    fn key_value(&mut self, meta: &ast::KeyValueMeta) {
        let is_doc = meta
            .path()
            .is_some_and(|path| path.syntax().text() == "doc");
        let Some(expr) = meta.expr() else {
            return;
        };
        if is_doc && let ast::Expr::MacroExpr(call) = &expr {
            let include = call.macro_call().and_then(|call| {
                let name = call.path()?.segment()?.name_ref()?;
                (name.text() == "include_str").then_some(call.token_tree()?)
            });
            if let Some(tree) = include {
                self.facts
                    .doc_includes
                    .extend(string_literals(tree.syntax()));
            }
            return;
        }
        self.token_refs(expr.syntax(), Origin::Attr, false);
    }

    /// The code blocks of the doc comments of the file. Each run of doc
    /// comments with only whitespace between them is one Markdown text.
    fn docs(&mut self, root: &SyntaxNode) {
        let mut blocks: Vec<(String, u32)> = Vec::new();
        let mut last_end: Option<usize> = None;
        for token in root
            .descendants_with_tokens()
            .filter_map(ra_ap_syntax::NodeOrToken::into_token)
        {
            let Some(comment) = ast::AnyComment::cast(token.clone()) else {
                continue;
            };
            if comment.doc_kind().is_none() {
                continue;
            }
            let start = usize::from(token.text_range().start());
            let joined = last_end.is_some_and(|end| {
                self.text
                    .get(end..start)
                    .is_some_and(|gap| gap.trim().is_empty())
            });
            if !joined || blocks.is_empty() {
                blocks.push((String::new(), self.line_at(start)));
            }
            if let Some((block, _)) = blocks.last_mut() {
                block.push_str(comment.text());
                block.push('\n');
            }
            last_end = Some(usize::from(token.text_range().end()));
        }
        for (block, line) in &blocks {
            let refs = doc_refs_at(block, self.edition, *line);
            self.facts.refs.extend(refs);
        }
    }
}

/// The condition of an unknown `cfg`, for example a `cfg` in a `cfg_attr`.
pub const UNKNOWN_CFG: &str = "?";

/// The conditions of the `cfg` attributes of a node, outer and inner, with no
/// whitespace: `test` for `#[cfg(test)]`. `#[test]`, also with a path such as
/// `#[tokio::test]`, and the built-in `#[bench]` give `test`, because only a
/// test build compiles them. Another test attribute, for example
/// `#[test_case(1)]`, and a `cfg_attr` that can add a `cfg` or a test
/// attribute give [`UNKNOWN_CFG`].
pub fn cfg_conditions(owner: &ast::AnyHasAttrs) -> Vec<String> {
    owner
        .attrs()
        .filter_map(|attr| {
            let body = attr_body(&attr);
            if let Some(condition) = body.strip_prefix("cfg(") {
                return Some(condition.strip_suffix(')').unwrap_or(condition).to_owned());
            }
            let name = body.split('(').next().unwrap_or(&body);
            if name.rsplit("::").next() == Some("test") || crate::attrs::is_bench_attribute(name) {
                return Some("test".to_owned());
            }
            if crate::attrs::is_test_attribute(&body) {
                return Some(UNKNOWN_CFG.to_owned());
            }
            body.strip_prefix("cfg_attr(")
                .filter(|rest| {
                    rest.contains("cfg(")
                        || crate::attrs::split_list(rest.strip_suffix(')').unwrap_or(rest))
                            .iter()
                            .skip(1)
                            .any(|attr| crate::attrs::is_test_attribute(attr))
                })
                .map(|_| UNKNOWN_CFG.to_owned())
        })
        .collect()
}

/// True when a `cfg` attribute of the node can be off in a build with all
/// features and tests.
fn cfg_can_be_off(owner: &ast::AnyHasAttrs) -> bool {
    owner.attrs().any(|attr| {
        let body = attr_body(&attr);
        body.strip_prefix("cfg(")
            .and_then(|rest| rest.strip_suffix(')'))
            .is_some_and(|condition| crate::attrs::cfg_value_all_features(condition) != Some(true))
    })
}

/// The field name of a [`RefKind::RecordField`] that stands for each field of
/// the struct: struct update syntax reads them all.
pub const ALL_FIELDS: &str = "..";

/// The derives of the standard library. None of them declares a helper
/// attribute, except `#[default]`, which is a built-in attribute here.
const STD_DERIVES: [&str; 9] = [
    "Debug",
    "Clone",
    "Copy",
    "PartialEq",
    "Eq",
    "PartialOrd",
    "Ord",
    "Hash",
    "Default",
];

/// True when the body of a `derive` attribute names a derive outside the
/// standard library.
fn derive_has_helpers(body: &str) -> bool {
    let Some(list) = body
        .split_once('(')
        .and_then(|(_, rest)| rest.strip_suffix(')'))
    else {
        return true;
    };
    list.split(',')
        .map(|name| name.rsplit("::").next().unwrap_or(name))
        .filter(|name| !name.is_empty())
        .any(|name| !STD_DERIVES.contains(&name))
}

/// True when the attribute body allows or expects the `dead_code` lint.
fn allows_dead(body: &str) -> bool {
    let name = body.split(['(', '=']).next().unwrap_or(body);
    let last = name.rsplit("::").next().unwrap_or(name);
    (last == "allow" || last == "expect") && (body.contains("dead_code") || body.contains("unused"))
}

/// True when an inner attribute that is a direct child of `node` allows the
/// `dead_code` lint: `#![allow(dead_code)]` at the top of a file or a module.
fn inner_allows_dead(node: &SyntaxNode) -> bool {
    node.children()
        .filter_map(ast::Attr::cast)
        .filter(|attr| attr.excl_token().is_some())
        .any(|attr| allows_dead(&attr_body(&attr)))
}

/// True for an attribute that the compiler or a tool defines, not a macro.
fn is_builtin_attribute(path: &str) -> bool {
    const BUILTIN: [&str; 39] = [
        "cfg",
        "default",
        "cfg_attr",
        "derive",
        "doc",
        "allow",
        "warn",
        "deny",
        "forbid",
        "expect",
        "inline",
        "cold",
        "must_use",
        "deprecated",
        "path",
        "test",
        "bench",
        "ignore",
        "should_panic",
        "repr",
        "non_exhaustive",
        "macro_export",
        "macro_use",
        "automatically_derived",
        "track_caller",
        "no_mangle",
        "export_name",
        "link_section",
        "link",
        "link_name",
        "used",
        "unsafe",
        "target_feature",
        "proc_macro",
        "proc_macro_derive",
        "proc_macro_attribute",
        "global_allocator",
        "panic_handler",
        "diagnostic",
    ];
    let first = path.split("::").next().unwrap_or(path);
    BUILTIN.contains(&first) || matches!(first, "rustfmt" | "clippy" | "rustdoc")
}

/// The value of a token tree with one string literal and no other token,
/// for example `("inc.rs")`.
fn single_string(tree: &ast::TokenTree) -> Option<String> {
    let mut tokens = tree
        .syntax()
        .children_with_tokens()
        .filter_map(ra_ap_syntax::NodeOrToken::into_token)
        .filter(|token| {
            !token.kind().is_trivia()
                && !matches!(token.kind(), SyntaxKind::L_PAREN | SyntaxKind::R_PAREN)
        });
    let token = tokens.next()?;
    if tokens.next().is_some() || tree.syntax().children().next().is_some() {
        return None;
    }
    let literal = ast::String::cast(token)?;
    let value = literal.value().ok()?.into_owned();
    Some(value)
}

fn string_literals(node: &SyntaxNode) -> Vec<String> {
    node.descendants_with_tokens()
        .filter_map(ra_ap_syntax::NodeOrToken::into_token)
        .filter(|token| token.kind() == SyntaxKind::STRING)
        .filter_map(|token| {
            let text = token.text();
            Some(text.strip_prefix('"')?.strip_suffix('"')?.to_owned())
        })
        .collect()
}

/// The documentation text of one item, with the file line of each text line,
/// and the files that `#[doc = include_str!(..)]` reads.
#[derive(Default)]
struct DocText {
    text: String,
    lines: Vec<u32>,
    /// The path, the line and true when the path is relative to the folder
    /// of the package.
    includes: Vec<(String, u32, bool)>,
}

impl DocText {
    fn push(&mut self, text: &str, first: u32) {
        let count = u32::try_from(text.lines().count().max(1)).unwrap_or(u32::MAX);
        self.lines
            .extend((0..count).map(|offset| first.saturating_add(offset)));
        self.text.push_str(text);
        self.text.push('\n');
    }
}

/// Adds the text of a `doc = ".."` attribute, also inside `cfg_attr(..)`, and
/// the file of a `doc = include_str!(..)` attribute.
fn doc_meta(meta: ast::Meta, line: u32, docs: &mut DocText) {
    match meta {
        ast::Meta::KeyValueMeta(meta) => {
            let is_doc = meta
                .path()
                .is_some_and(|path| path.syntax().text() == "doc");
            let Some(expr) = meta.expr().filter(|_| is_doc) else {
                return;
            };
            if let ast::Expr::MacroExpr(call) = &expr {
                doc_include(call, line, docs);
                return;
            }
            for text in string_literals(expr.syntax()) {
                docs.push(&text.replace("\\n", "\n"), line);
            }
        }
        ast::Meta::CfgAttrMeta(meta) => {
            for inner in meta.metas() {
                doc_meta(inner, line, docs);
            }
        }
        ast::Meta::UnsafeMeta(_)
        | ast::Meta::TokenTreeMeta(_)
        | ast::Meta::CfgMeta(_)
        | ast::Meta::PathMeta(_) => {}
    }
}

/// Adds the file of a `doc = include_str!(..)` attribute. The literals of a
/// `concat!` form one path, and `env!("CARGO_MANIFEST_DIR")` makes the path
/// relative to the folder of the package.
fn doc_include(call: &ast::MacroExpr, line: u32, docs: &mut DocText) {
    let include = call.macro_call().and_then(|call| {
        let name = call.path()?.segment()?.name_ref()?;
        (name.text() == "include_str").then_some(call.token_tree()?)
    });
    let Some(tree) = include else {
        return;
    };
    let manifest = tree
        .syntax()
        .text()
        .to_string()
        .contains("CARGO_MANIFEST_DIR");
    let path: String = string_literals(tree.syntax())
        .into_iter()
        .filter(|part| part != "CARGO_MANIFEST_DIR")
        .collect();
    if !path.is_empty() {
        docs.includes.push((path, line, manifest));
    }
}

/// True when macro tokens can write a `pub` item: they hold the `pub`
/// keyword or a `$name:vis` fragment.
fn writes_pub(tree: &SyntaxNode) -> bool {
    let mut after_colon = false;
    for token in tree
        .descendants_with_tokens()
        .filter_map(ra_ap_syntax::NodeOrToken::into_token)
    {
        match token.kind() {
            SyntaxKind::PUB_KW => return true,
            SyntaxKind::COLON => after_colon = true,
            SyntaxKind::IDENT if after_colon && token.text() == "vis" => return true,
            SyntaxKind::WHITESPACE | SyntaxKind::COMMENT => {}
            _ => after_colon = false,
        }
    }
    false
}

fn vis_of(node: &SyntaxNode) -> Vis {
    let Some(vis) = ast::AnyHasVisibility::cast(node.clone()).and_then(|owner| owner.visibility())
    else {
        return Vis::Private;
    };
    match vis.kind() {
        VisibilityKind::Pub => Vis::Public,
        VisibilityKind::PubCrate => Vis::Crate,
        VisibilityKind::PubSuper => Vis::Super,
        VisibilityKind::PubSelf => Vis::In(vec!["self".to_owned()]),
        VisibilityKind::In(path) => Vis::In(path_segments(&path).0),
    }
}

/// The segments of a path, as text, and whether it starts with `::`. A
/// qualified segment such as `<T as Trait>` ends the path before it.
pub fn path_segments(path: &ast::Path) -> (Vec<String>, bool) {
    let mut segments = Vec::new();
    let global = path
        .first_segment()
        .is_some_and(|segment| segment.coloncolon_token().is_some());
    for segment in path.segments() {
        let text = match segment.kind() {
            Some(ast::PathSegmentKind::Name(name)) => name.text().to_string(),
            Some(ast::PathSegmentKind::SelfTypeKw) => "Self".to_owned(),
            Some(ast::PathSegmentKind::SelfKw) => "self".to_owned(),
            Some(ast::PathSegmentKind::SuperKw) => "super".to_owned(),
            Some(ast::PathSegmentKind::CrateKw) => "crate".to_owned(),
            Some(ast::PathSegmentKind::Type { .. }) | None => {
                segments.clear();
                continue;
            }
        };
        segments.push(
            text.strip_prefix("r#")
                .map_or_else(|| text.clone(), str::to_owned),
        );
    }
    (segments, global)
}

/// The path of a type, when the type is a plain path: `a::B<T>` gives `a::B`.
fn type_path(ty: &ast::Type) -> Option<Vec<String>> {
    match ty {
        ast::Type::PathType(path) => {
            let (segments, _) = path_segments(&path.path()?);
            (!segments.is_empty()).then_some(segments)
        }
        _ => None,
    }
}

/// The offset of the name that a leaf of a use tree binds: the name after
/// `as`, else the last segment of the path.
fn leaf_name_offset(leaf: &ast::UseTree) -> usize {
    let name = leaf
        .rename()
        .and_then(|rename| rename.name())
        .map(|name| name.syntax().text_range().start())
        .or_else(|| {
            leaf.path()
                .and_then(|path| path.segment())
                .map(|segment| segment.syntax().text_range().start())
        });
    usize::from(name.unwrap_or_else(|| leaf.syntax().text_range().start()))
}

/// One flat import: the path, the `::` flag, the binding and the leaf tree.
type FlatUse = (Vec<String>, bool, Binding, ast::UseTree);

fn flatten_use(
    tree: &ast::UseTree,
    prefix: &[String],
    prefix_global: bool,
    out: &mut Vec<FlatUse>,
) {
    let mut path = prefix.to_vec();
    let mut global = prefix_global;
    if let Some(own) = tree.path() {
        let (segments, own_global) = path_segments(&own);
        global |= prefix.is_empty() && own_global;
        path.extend(segments);
    } else if prefix.is_empty() && tree.coloncolon_token().is_some() {
        global = true;
    }
    if let Some(list) = tree.use_tree_list() {
        for child in list.use_trees() {
            flatten_use(&child, &path, global, out);
        }
        return;
    }
    if tree.star_token().is_some() {
        out.push((path, global, Binding::Glob, tree.clone()));
        return;
    }
    if path.last().is_some_and(|last| last == "self") {
        path.pop();
    }
    let binding = match tree.rename() {
        Some(rename) if rename.underscore_token().is_some() => Binding::Underscore,
        Some(rename) => match rename.name() {
            Some(name) => Binding::Name(plain_name(&name)),
            None => return,
        },
        None => match path.last() {
            Some(last) => Binding::Name(last.clone()),
            None => return,
        },
    };
    out.push((path, global, binding, tree.clone()));
}
