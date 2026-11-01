//! Writes one source file with a runtime switch at each enabled site.
//!
//! The module splices on exact text ranges and never prints the syntax tree,
//! so all other text stays the same. A site becomes one match expression:
//!
//! ```text
//! (match crate::__fallow_schema::active() { 17_u64 => (a != b), _ => (a == b) })
//! ```
//!
//! Only one mutant is active in a process. So a mutant arm holds the plain
//! original operands, and only the default arm holds the switches of nested
//! sites. The text then grows with the nesting depth, not exponentially.

use std::collections::BTreeSet;
use std::fmt::Write;
use std::ops::Range;

use crate::mutation::sites::{Mutant, Shape, Site};

/// A byte range of the generated text that belongs to one site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// The range in the generated text.
    pub range: Range<usize>,
    /// The index of the site in the file.
    pub site: usize,
    /// The mutant of an arm, or `None` for the whole switch.
    pub mutant: Option<u64>,
}

/// A generated source file and the owner of each byte range.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rendered {
    /// The generated text.
    pub text: String,
    /// The regions, in the order that the writer closed them.
    pub regions: Vec<Region>,
}

/// The part of the generated text that a compiler error points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// One mutant arm.
    Arm(u64),
    /// The switch of one site, outside each mutant arm.
    Site(usize),
}

impl Rendered {
    /// The smallest region that holds the whole span.
    pub fn owner_of(&self, span: Range<usize>) -> Option<Owner> {
        let region = self
            .regions
            .iter()
            .filter(|region| region.range.start <= span.start && span.end <= region.range.end)
            .min_by_key(|region| region.range.len())?;
        Some(region.mutant.map_or(Owner::Site(region.site), Owner::Arm))
    }
}

/// The crate-root additions of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrateRoot {
    /// The original offset after the last inner attribute.
    pub after_inner_attrs: usize,
}

/// The lints that a switch can start in a crate with `#![deny(warnings)]`.
///
/// Each switch has parentheses, because a `match` at the start of a statement
/// ends that statement. A mutant such as `x >= 0` on an unsigned `x` is a
/// comparison that the compiler can see through.
const LINT_LEVELS: &str = "#![allow(unused_parens, unused_comparisons)]";

/// The call that reads the active mutant. `crate::` works in each edition.
const ACTIVE_CALL: &str = "crate::__fallow_schema::active()";

/// The helper. The environment variable holds a hexadecimal mutant id.
///
/// The helper takes each name from an `extern crate` item in its own module:
/// that works in a `#![no_std]` crate and in a crate with
/// `#![no_implicit_prelude]`. A value that does not parse stops the process:
/// a silent fallback to 0 runs the baseline, and that reports the mutant as a
/// survivor.
const HELPER: &str = r#"
#[doc(hidden)]
#[allow(dead_code, missing_docs, unreachable_pub, unused_extern_crates, unused_qualifications)]
mod __fallow_schema {
    extern crate std as fallow_std;
    static ACTIVE: self::fallow_std::sync::OnceLock<u64> = self::fallow_std::sync::OnceLock::new();
    #[inline]
    pub(crate) fn active() -> u64 {
        *ACTIVE.get_or_init(|| match self::fallow_std::env::var("FALLOW_MUTANT") {
            self::fallow_std::result::Result::Ok(value) => {
                match u64::from_str_radix(&value, 16) {
                    self::fallow_std::result::Result::Ok(id) => id,
                    self::fallow_std::result::Result::Err(_) => invalid(),
                }
            }
            self::fallow_std::result::Result::Err(self::fallow_std::env::VarError::NotPresent) => 0,
            self::fallow_std::result::Result::Err(_) => invalid(),
        })
    }
    fn invalid() -> ! {
        self::fallow_std::eprintln!("cargo-fallow: FALLOW_MUTANT is not a hexadecimal mutant id");
        self::fallow_std::process::abort()
    }
}
"#;

/// What a mutant arm holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The mutated expression.
    Schema,
    /// A name that does not exist. The compiler reports each such name in the
    /// code that it compiles, so the report shows which mutants a build reads.
    /// A mutant under a `cfg` that is off gives no error, so a clean compile
    /// says nothing about that mutant.
    Probe,
}

/// The start of a name in a [`Mode::Probe`] arm. The mutant id follows.
pub const PROBE_PREFIX: &str = "__fallow_probe_";

/// Writes the file with the mutants in `enabled`. A site with no enabled mutant
/// stays plain text. Pass `root` for a crate root file when any mutant of the
/// crate is enabled.
pub fn render(
    original: &str,
    sites: &[Site],
    enabled: &BTreeSet<u64>,
    root: Option<CrateRoot>,
    mode: Mode,
) -> Rendered {
    let tree = build_tree(sites, enabled);
    let mut writer = Writer {
        mode,
        original,
        sites,
        enabled,
        out: Rendered::default(),
    };
    let start = root.map_or(0, |root| {
        writer.push(0..root.after_inner_attrs);
        writer.out.text.push_str(LINT_LEVELS);
        root.after_inner_attrs
    });
    writer.range(start..original.len(), &tree);
    if root.is_some() {
        writer.out.text.push_str(HELPER);
    }
    writer.out
}

/// An enabled site with the enabled sites inside it.
struct Node {
    site: usize,
    children: Vec<Node>,
}

/// Builds the nesting of the enabled sites. `sites` has the outer sites first.
fn build_tree(sites: &[Site], enabled: &BTreeSet<u64>) -> Vec<Node> {
    let mut roots = Vec::new();
    let mut open: Vec<Node> = Vec::new();
    let is_enabled = |site: &Site| site.mutants.iter().any(|m| enabled.contains(&m.id));
    for (index, site) in sites.iter().enumerate().filter(|(_, s)| is_enabled(s)) {
        while open.last().is_some_and(|top| {
            sites[top.site].range.end < site.range.end
                || sites[top.site].range.end <= site.range.start
        }) {
            close_top(&mut open, &mut roots);
        }
        open.push(Node {
            site: index,
            children: Vec::new(),
        });
    }
    while !open.is_empty() {
        close_top(&mut open, &mut roots);
    }
    roots
}

fn close_top(open: &mut Vec<Node>, roots: &mut Vec<Node>) {
    let Some(node) = open.pop() else {
        return;
    };
    open.last_mut()
        .map_or(&mut *roots, |parent| &mut parent.children)
        .push(node);
}

struct Writer<'a> {
    mode: Mode,
    original: &'a str,
    sites: &'a [Site],
    enabled: &'a BTreeSet<u64>,
    out: Rendered,
}

impl Writer<'_> {
    fn push(&mut self, range: Range<usize>) {
        self.out.text.push_str(&self.original[range]);
    }

    /// Writes the original text of `range` with each node in it as a switch.
    fn range(&mut self, range: Range<usize>, nodes: &[Node]) {
        let mut cursor = range.start;
        for node in nodes {
            let site_range = self.sites[node.site].range.clone();
            self.push(cursor..site_range.start);
            self.switch(node);
            cursor = site_range.end;
        }
        self.push(cursor..range.end);
    }

    fn switch(&mut self, node: &Node) {
        let sites = self.sites;
        let site = &sites[node.site];
        let start = self.out.text.len();
        self.out.text.push_str("(match ");
        self.out.text.push_str(ACTIVE_CALL);
        self.out.text.push_str(" { ");
        for mutant in site.mutants.iter().filter(|m| self.enabled.contains(&m.id)) {
            self.arm(node.site, mutant);
        }
        // An arm that starts with a block ends after that block, so `unsafe { x }
        // == 1` needs parentheses. Each arm gets them, so no rule can miss one.
        self.out.text.push_str("_ => (");
        self.range(site.range.clone(), &node.children);
        self.out.text.push_str(") })");
        self.out.regions.push(Region {
            range: start..self.out.text.len(),
            site: node.site,
            mutant: None,
        });
    }

    fn arm(&mut self, index: usize, mutant: &Mutant) {
        let site = &self.sites[index];
        let start = self.out.text.len();
        // A write to a `String` cannot fail.
        let _ = write!(self.out.text, "{:#x}_u64 => (", mutant.id);
        match (self.mode, site.shape) {
            (Mode::Probe, _) => {
                let _ = write!(self.out.text, "{PROBE_PREFIX}{:x}", mutant.id);
            }
            (Mode::Schema, Shape::Condition) => {
                self.out.text.push_str("!(");
                self.push(site.range.clone());
                self.out.text.push(')');
            }
            (Mode::Schema, Shape::Binary { lhs_end, rhs_start }) => {
                // `x as usize < y` is a syntax error: the parser reads `<` as
                // the start of generic arguments.
                let wrap = mutant.replacement == "<";
                self.out.text.push_str(if wrap { "(" } else { "" });
                self.push(site.range.start..lhs_end);
                self.out.text.push_str(if wrap { ")" } else { "" });
                self.out.text.push(' ');
                self.out.text.push_str(mutant.replacement);
                self.out.text.push(' ');
                self.push(rhs_start..site.range.end);
            }
        }
        self.out.text.push(')');
        self.out.regions.push(Region {
            range: start..self.out.text.len(),
            site: index,
            mutant: Some(mutant.id),
        });
        self.out.text.push_str(", ");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mutation::sites::{FileSites, find_sites};
    use crate::parse::Edition;

    fn parse(source: &str) -> FileSites {
        let Ok(found) = find_sites(source, Edition::Edition2021, "src/lib.rs") else {
            panic!("the source parses");
        };
        found
    }

    fn all_ids(sites: &[Site]) -> BTreeSet<u64> {
        sites
            .iter()
            .flat_map(|site| site.mutants.iter().map(|m| m.id))
            .collect()
    }

    fn render_all(source: &str) -> (Rendered, Vec<Site>) {
        let found = parse(source);
        let enabled = all_ids(&found.sites);
        (
            render(source, &found.sites, &enabled, None, Mode::Schema),
            found.sites,
        )
    }

    /// Replaces each id in `text` with its position in `sites`, so the
    /// expected text stays readable.
    fn readable(text: &str, sites: &[Site]) -> String {
        let mut text = text.to_owned();
        let ids = sites.iter().flat_map(|site| site.mutants.iter());
        for (index, mutant) in ids.enumerate() {
            text = text.replace(&format!("0x{:x}_u64", mutant.id), &format!("M{index}"));
        }
        text.replace("crate::__fallow_schema::active()", "ID")
    }

    #[test]
    fn binary_site_becomes_a_switch() {
        let (out, sites) = render_all("fn f() -> bool { 1 == 2 }");
        assert_eq!(
            readable(&out.text, &sites),
            "fn f() -> bool { (match ID { M0 => (1 != 2), _ => (1 == 2) }) }"
        );
    }

    #[test]
    fn one_arm_for_each_replacement() {
        let (out, sites) = render_all("fn f() -> bool { 1 < 2 }");
        assert_eq!(
            readable(&out.text, &sites),
            "fn f() -> bool { (match ID { M0 => (1 <= 2), M1 => (1 >= 2), _ => (1 < 2) }) }"
        );
    }

    #[test]
    fn condition_is_negated_with_parentheses() {
        let (out, sites) = render_all("fn f(a: bool) { if a {} }");
        assert_eq!(
            readable(&out.text, &sites),
            "fn f(a: bool) { if (match ID { M0 => (!(a)), _ => (a) }) {} }"
        );
    }

    #[test]
    fn mutant_arms_hold_the_plain_operands() {
        let (out, sites) = render_all("fn f(a: bool) -> bool { a == a && a != a }");
        assert_eq!(
            readable(&out.text, &sites),
            "fn f(a: bool) -> bool { (match ID { M0 => (a == a || a != a), _ => (\
             (match ID { M1 => (a != a), _ => (a == a) }) && \
             (match ID { M2 => (a == a), _ => (a != a) })) }) }"
        );
    }

    #[test]
    fn deep_chain_grows_with_depth_and_not_exponentially() {
        let chain = vec!["a"; 64].join(" && ");
        let source = format!("fn f(a: bool) -> bool {{ {chain} }}");
        let (out, _) = render_all(&source);
        assert!(out.text.len() < source.len() * 100, "{}", out.text.len());
    }

    #[test]
    fn replacement_lt_gets_parentheses_around_a_cast() {
        let (out, sites) = render_all("fn f(a: u8, b: usize) -> bool { a as usize >= b }");
        assert!(
            readable(&out.text, &sites).contains("M1 => ((a as usize) < b),"),
            "{}",
            out.text
        );
    }

    #[test]
    fn replacement_has_spaces_so_tokens_do_not_join() {
        let (out, sites) = render_all("fn f(a: i8) -> bool { a>=-1 }");
        assert!(
            readable(&out.text, &sites).contains("M1 => ((a) < -1),"),
            "{}",
            out.text
        );
    }

    #[test]
    fn block_like_operand_stays_an_operand() {
        let (out, sites) = render_all("fn f(a: u8) { let _ = unsafe { a } == 1; }");
        assert_eq!(
            readable(&out.text, &sites),
            "fn f(a: u8) { let _ = (match ID { M0 => (unsafe { a } != 1), _ => (unsafe { a } == 1) }); }"
        );
    }

    #[test]
    fn probe_arm_holds_a_name_with_the_id() {
        let source = "fn f() -> bool { 1 == 2 }";
        let found = parse(source);
        let id = found.sites[0].mutants[0].id;
        let out = render(
            source,
            &found.sites,
            &all_ids(&found.sites),
            None,
            Mode::Probe,
        );
        assert!(
            out.text.contains(&format!("{PROBE_PREFIX}{id:x}")),
            "{}",
            out.text
        );
        assert!(out.text.contains("_ => (1 == 2)"), "{}", out.text);
    }

    #[test]
    fn disabled_site_stays_plain_text() {
        let source = "fn f() -> bool { 1 == 2 }";
        let found = parse(source);
        let out = render(source, &found.sites, &BTreeSet::new(), None, Mode::Schema);
        assert_eq!(out.text, source);
        assert!(out.regions.is_empty());
    }

    #[test]
    fn disabled_outer_site_keeps_the_inner_switch() {
        let source = "fn f(a: bool) -> bool { a == a && a }";
        let found = parse(source);
        let inner: BTreeSet<u64> = [found.sites[1].mutants[0].id].into();
        let out = render(source, &found.sites, &inner, None, Mode::Schema);
        assert_eq!(
            readable(&out.text, &found.sites),
            "fn f(a: bool) -> bool { (match ID { M1 => (a != a), _ => (a == a) }) && a }"
        );
    }

    #[test]
    fn single_line_site_keeps_the_line_count() {
        let source = "fn f() -> bool {\n    1 == 2\n}\n";
        let (out, _) = render_all(source);
        assert_eq!(out.text.lines().count(), source.lines().count());
    }

    #[test]
    fn crate_root_gets_the_lint_levels_on_the_same_line_and_the_helper() {
        let source = "//! Docs.\n#![deny(warnings)]\nfn f() -> bool { 1 == 2 }";
        let found = parse(source);
        let root = CrateRoot {
            after_inner_attrs: found.after_inner_attrs,
        };
        let out = render(
            source,
            &found.sites,
            &all_ids(&found.sites),
            Some(root),
            Mode::Schema,
        );
        assert!(
            out.text.starts_with(
                "//! Docs.\n#![deny(warnings)]#![allow(unused_parens, unused_comparisons)]\nfn f()"
            ),
            "{}",
            out.text
        );
        assert!(out.text.contains("mod __fallow_schema"));
        assert!(out.text.contains("extern crate std as fallow_std;"));
    }

    #[test]
    fn owner_of_finds_the_innermost_part() {
        let source = "fn f(a: bool) -> bool { a == a && a }";
        let (out, sites) = render_all(source);
        let (outer, inner) = (&sites[0], &sites[1]);
        let find = |needle: &str| {
            let Some(start) = out.text.find(needle) else {
                panic!("{needle} is not in {}", out.text);
            };
            start..start + needle.len()
        };
        assert_eq!(
            out.owner_of(find("a == a || a")),
            Some(Owner::Arm(outer.mutants[0].id))
        );
        assert_eq!(
            out.owner_of(find("a != a")),
            Some(Owner::Arm(inner.mutants[0].id))
        );
        assert_eq!(out.owner_of(find("&& a)")), Some(Owner::Site(0)));
        let inner_call = {
            let needle = "crate::__fallow_schema::active()";
            let Some(first) = out.text.find(needle) else {
                panic!("no switch");
            };
            let start = first + needle.len();
            let Some(second) = out.text[start..].find(needle) else {
                panic!("no inner switch");
            };
            start + second..start + second + needle.len()
        };
        assert_eq!(out.owner_of(inner_call), Some(Owner::Site(1)));
        assert_eq!(out.owner_of(find("fn f")), None);
        assert_eq!(out.owner_of(0..out.text.len()), None);
    }

    #[test]
    fn crlf_text_stays_in_place_around_the_splice() {
        let source = "fn f() -> bool {\r\n    1 == 2\r\n}\r\n";
        let (out, sites) = render_all(source);
        assert_eq!(
            readable(&out.text, &sites),
            "fn f() -> bool {\r\n    (match ID { M0 => (1 != 2), _ => (1 == 2) })\r\n}\r\n"
        );
    }
}
