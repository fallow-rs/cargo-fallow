//! The facts of one source file: items, imports and references.
//!
//! The walk reads the syntax tree once, in preorder. A stack of frames holds
//! the context of each node: the inline module, the item that owns the code,
//! the `impl` block, a function body and test code. The facts are plain data,
//! because the syntax tree cannot leave the sized stack of the parse.

use ra_ap_syntax::ast::{self, HasAttrs, HasName, HasVisibility, VisibilityKind};
use ra_ap_syntax::{AstNode, AstToken, SyntaxKind, SyntaxNode, WalkEvent};

use crate::attrs::{AttrClass, attr_body, attributes_of, path_attribute, plain_name};
use crate::graph::tokens::{TokenPath, paths_in_code, paths_in_tree, rust_code_blocks};
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
    /// `#[test]`, `#[bench]` and the test macros of the wider ecosystem.
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
    /// In the tokens of an attribute.
    Attr,
    /// In a code block of the documentation.
    Doc,
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
    /// True when a `cfg` attribute other than a test condition is on an
    /// enclosing node.
    pub under_cfg: bool,
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
}

/// Reads the facts of one file.
pub fn extract(source: &str, edition: Edition) -> Result<FileFacts, ParseOutcome> {
    with_parsed(source, edition, |file, text| {
        let mut walker = Walker::new(text, edition);
        walker.walk(file.syntax());
        walker.docs(file.syntax());
        walker.facts.allow_dead = inner_allows_dead(file.syntax());
        walker.facts
    })
}

/// The documentation code blocks of a Markdown file, as references.
pub fn doc_refs_of_markdown(markdown: &str, edition: Edition) -> Vec<RawRef> {
    rust_code_blocks(markdown)
        .iter()
        .flat_map(|code| paths_in_code(code, edition))
        .flat_map(doc_ref)
        .collect()
}

fn doc_ref(path: TokenPath) -> Vec<RawRef> {
    token_ref_kinds(path)
        .into_iter()
        .map(|kind| RawRef {
            inline: Vec::new(),
            owner: None,
            kind,
            origin: Origin::Doc,
            test_only: true,
            under_cfg: false,
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
    LocalItems,
}

struct Walker<'a> {
    facts: FileFacts,
    lines: Vec<usize>,
    text: &'a str,
    edition: Edition,
    frames: Vec<(SyntaxNode, Frame)>,
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
        if let Some(owner) = ast::AnyHasAttrs::cast(node.clone()) {
            match attributes_of(node) {
                AttrClass::Test => self.push(node, Frame::Test),
                AttrClass::Cfg => self.push(node, Frame::Cfg),
                AttrClass::Other => {}
            }
            self.attr_refs(&owner);
        }
        if Self::is_body(node) {
            self.push(node, Frame::Body);
        }
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
            under_cfg: self.under_cfg(),
        });
    }

    fn path(&mut self, node: &SyntaxNode) {
        let Some(path) = ast::Path::cast(node.clone()) else {
            return;
        };
        let skip = node.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                SyntaxKind::PATH
                    | SyntaxKind::USE_TREE
                    | SyntaxKind::VISIBILITY
                    | SyntaxKind::MACRO_CALL
                    | SyntaxKind::PATH_META
                    | SyntaxKind::TOKEN_TREE_META
                    | SyntaxKind::KEY_VALUE_META
            )
        });
        if skip {
            return;
        }
        let (segments, global) = self.segments_of(&path);
        if !segments.is_empty() {
            self.add_ref(RefKind::Path { segments, global }, Origin::Body);
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
            self.add_ref(RefKind::Method(name.text().to_string()), Origin::Body);
        }
    }

    fn field_access(&mut self, node: &SyntaxNode) {
        if let Some(name) = ast::FieldExpr::cast(node.clone()).and_then(|access| access.name_ref())
        {
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
        if let Some(path) = call.path()
            && let Some(name) = path.segment().and_then(|segment| segment.name_ref())
        {
            let (segments, global) = path_segments(&path);
            if segments.len() > 1 {
                self.add_ref(RefKind::Path { segments, global }, Origin::Macro);
            }
            self.add_ref(RefKind::Macro(name.text().to_string()), Origin::Macro);
        }
        if let Some(tree) = call.token_tree() {
            self.token_refs(tree.syntax(), Origin::Macro);
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
            self.token_refs(&tree, Origin::Macro);
        }
    }

    fn token_refs(&mut self, tree: &SyntaxNode, origin: Origin) {
        for mut path in paths_in_tree(tree) {
            if let (Some(first), Some(self_ty)) = (path.segments.first(), self.impl_self())
                && first == "Self"
            {
                let mut segments = self_ty;
                segments.extend(path.segments.into_iter().skip(1));
                path.segments = segments;
            }
            for kind in token_ref_kinds(path) {
                self.add_ref(kind, origin);
            }
        }
    }

    fn attr_refs(&mut self, owner: &ast::AnyHasAttrs) {
        for attr in owner.attrs() {
            match attr.meta() {
                Some(ast::Meta::TokenTreeMeta(meta)) => {
                    if let Some(tree) = meta.token_tree() {
                        self.token_refs(tree.syntax(), Origin::Attr);
                    }
                }
                Some(ast::Meta::KeyValueMeta(meta)) => self.key_value(&meta),
                _ => {}
            }
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
        self.token_refs(expr.syntax(), Origin::Attr);
    }

    /// The code blocks of the doc comments of the file. Each run of doc
    /// comments with only whitespace between them is one Markdown text.
    fn docs(&mut self, root: &SyntaxNode) {
        let mut blocks: Vec<String> = Vec::new();
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
                blocks.push(String::new());
            }
            if let Some(block) = blocks.last_mut() {
                block.push_str(comment.text());
                block.push('\n');
            }
            last_end = Some(usize::from(token.text_range().end()));
        }
        for block in &blocks {
            let refs = doc_refs_of_markdown(block, self.edition);
            self.facts.refs.extend(refs);
        }
    }
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
