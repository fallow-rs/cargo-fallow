use super::extract::{Binding, ItemKind, Origin, RefKind, Vis, extract};
use crate::parse::Edition;

fn facts(source: &str) -> super::extract::FileFacts {
    let Ok(facts) = extract(source, Edition::Edition2021) else {
        panic!("the source parses");
    };
    facts
}

fn path_refs(facts: &super::extract::FileFacts) -> Vec<String> {
    facts
        .refs
        .iter()
        .filter_map(|reference| match &reference.kind {
            RefKind::Path { segments, .. } => Some(segments.join("::")),
            _ => None,
        })
        .collect()
}

#[test]
fn items_have_kind_visibility_parent_and_position() {
    let found = facts(
        "pub struct S {\n    pub(crate) a: u8,\n    b: u8,\n}\nimpl S {\n    pub fn new() -> Self { todo!() }\n}\n\
         pub(super) enum E { One(u8) }\nmod m { pub fn f() {} }\n",
    );
    let summary: Vec<(String, ItemKind, Vis, Option<usize>, u32)> = found
        .items
        .iter()
        .map(|item| {
            (
                item.name.clone(),
                item.kind,
                item.vis.clone(),
                item.parent,
                item.line,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("S".to_owned(), ItemKind::Struct, Vis::Public, None, 1),
            ("a".to_owned(), ItemKind::Field, Vis::Crate, Some(0), 2),
            ("b".to_owned(), ItemKind::Field, Vis::Private, Some(0), 3),
            (String::new(), ItemKind::Impl, Vis::Private, None, 5),
            ("new".to_owned(), ItemKind::Method, Vis::Public, Some(3), 6),
            ("E".to_owned(), ItemKind::Enum, Vis::Super, None, 8),
            ("One".to_owned(), ItemKind::Variant, Vis::Public, Some(5), 8),
            ("0".to_owned(), ItemKind::Field, Vis::Private, Some(6), 8),
            ("m".to_owned(), ItemKind::Module, Vis::Private, None, 9),
            ("f".to_owned(), ItemKind::Function, Vis::Public, None, 9),
        ]
    );
    assert_eq!(found.items[9].inline, ["m"]);
}

#[test]
fn use_trees_become_flat_imports() {
    let found =
        facts("use a::{b::{self, c as d}, e::*, f as _};\npub use ::g::h;\nextern crate k as l;\n");
    let imports: Vec<(String, bool, Binding)> = found
        .uses
        .iter()
        .map(|import| {
            (
                import.path.join("::"),
                import.global,
                import.binding.clone(),
            )
        })
        .collect();
    assert_eq!(
        imports,
        [
            ("a::b".to_owned(), false, Binding::Name("b".to_owned())),
            ("a::b::c".to_owned(), false, Binding::Name("d".to_owned())),
            ("a::e".to_owned(), false, Binding::Glob),
            ("a::f".to_owned(), false, Binding::Underscore),
            ("g::h".to_owned(), true, Binding::Name("h".to_owned())),
            ("k".to_owned(), true, Binding::Name("l".to_owned())),
        ]
    );
    let reexport = found.uses[4].item.map(|index| found.items[index].kind);
    assert_eq!(reexport, Some(ItemKind::Use));
}

#[test]
fn self_in_an_impl_becomes_the_self_type() {
    let found = facts("impl a::T { fn f() { Self::g(); } }\n");
    assert!(path_refs(&found).contains(&"a::T::g".to_owned()));
}

#[test]
fn references_carry_their_owner_and_origin() {
    let found = facts(
        "fn f(x: Input) -> Output { helper(); x.call(); x.field; make!(inner::path, name); Output }\n",
    );
    let owner = found
        .refs
        .iter()
        .map(|reference| reference.owner)
        .collect::<Vec<_>>();
    assert!(owner.iter().all(|owner| *owner == Some(0)));
    let kinds: Vec<(&RefKind, Origin)> = found
        .refs
        .iter()
        .map(|reference| (&reference.kind, reference.origin))
        .collect();
    let path = |text: &str| RefKind::Path {
        segments: text.split("::").map(str::to_owned).collect(),
        global: false,
    };
    assert!(kinds.contains(&(&path("Input"), Origin::Signature)));
    assert!(kinds.contains(&(&path("Output"), Origin::Signature)));
    assert!(kinds.contains(&(&path("helper"), Origin::Body)));
    assert!(kinds.contains(&(&RefKind::Method("call".to_owned()), Origin::Body)));
    assert!(kinds.contains(&(&RefKind::Field("field".to_owned()), Origin::Body)));
    assert!(kinds.contains(&(&RefKind::Macro("make".to_owned()), Origin::Macro)));
    assert!(kinds.contains(&(&path("inner::path"), Origin::Macro)));
    assert!(kinds.contains(&(&path("name"), Origin::Macro)));
}

#[test]
fn doc_code_and_test_code_are_marked() {
    let found = facts(
        "/// ```\n/// use krate::Thing;\n/// ```\npub fn f() {}\n#[cfg(test)]\nmod tests { pub fn t() {} }\n\
         #[test]\nfn check() {}\n",
    );
    let doc: Vec<String> = found
        .refs
        .iter()
        .filter(|reference| reference.origin == Origin::Doc)
        .filter_map(|reference| match &reference.kind {
            RefKind::Path { segments, .. } => Some(segments.join("::")),
            _ => None,
        })
        .collect();
    assert_eq!(doc, ["krate::Thing"]);
    let tests: Vec<(&str, bool, bool)> = found
        .items
        .iter()
        .map(|item| (item.name.as_str(), item.test_only, item.flags.test_fn))
        .collect();
    assert_eq!(
        tests,
        [
            ("f", false, false),
            ("tests", true, false),
            ("t", true, false),
            ("check", true, true)
        ]
    );
}

#[test]
fn local_items_in_a_body_are_not_items() {
    let found = facts("fn outer() { struct Local; fn inner() {} use x::y; }\n");
    assert_eq!(found.items.len(), 1);
    assert_eq!(found.uses.len(), 1);
}
