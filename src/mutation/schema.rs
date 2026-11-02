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

/// The call that reads the active mutant. `crate::` works in each edition.
const ACTIVE_CALL: &str = "crate::__fallow_schema::active()";

/// The file descriptor that holds the record of the active mutant.
#[cfg(unix)]
pub const MUTANT_FD: u32 = 9;

/// The first bytes of the record of the active mutant.
#[cfg(unix)]
pub const RECORD_PREFIX: &str = "fallow-mutant:";

/// The record of a mutant: the prefix and 16 hexadecimal digits.
#[cfg(unix)]
pub fn mutant_record(mutant: u64) -> String {
    format!("{RECORD_PREFIX}{mutant:016x}")
}

/// The helper. The runner opens a file that holds the mutant id on a fixed file
/// descriptor, and each child process inherits the descriptor.
///
/// A value from the environment is not enough: a test can clear the environment,
/// so the mutant is silently off, and `env::var` allocates, so a test that counts
/// allocations fails, and a site in a global allocator waits for itself. This read
/// allocates nothing and needs no unsafe code. A descriptor that is not a file of
/// the right size is no mutant, so a read cannot block on another pipe.
///
/// The helper takes each name from an `extern crate` item in its own module:
/// that works in a `#![no_std]` crate and in a crate with
/// `#![no_implicit_prelude]`. A record that does not parse stops the process:
/// a silent fallback to 0 runs the baseline, and that reports the mutant as a
/// survivor.
const HELPER: &str = r#"
#[doc(hidden)]
#[allow(dead_code, missing_docs, unreachable_pub, unused_extern_crates, unused_qualifications)]
mod __fallow_schema {
    extern crate std as fallow_std;
    use self::fallow_std::sync::atomic::{AtomicU64, Ordering};
    const UNREAD: u64 = u64::MAX;
    static ACTIVE: AtomicU64 = AtomicU64::new(UNREAD);
    #[inline]
    pub(crate) fn active() -> u64 {
        let known = ACTIVE.load(Ordering::Relaxed);
        if known != UNREAD {
            return known;
        }
        let read = read_active();
        ACTIVE.store(read, Ordering::Relaxed);
        read
    }
    #[cfg(unix)]
    fn read_active() -> u64 {
        const PREFIX: &[u8] = b"fallow-mutant:";
        const SIZE: usize = 30;
        const PATH: &str = "/dev/fd/9";
        match self::fallow_std::fs::metadata(PATH) {
            self::fallow_std::result::Result::Ok(data) => {
                if !data.is_file() || data.len() != SIZE as u64 {
                    return 0;
                }
            }
            self::fallow_std::result::Result::Err(_) => return 0,
        }
        let file = match self::fallow_std::fs::File::open(PATH) {
            self::fallow_std::result::Result::Ok(file) => file,
            self::fallow_std::result::Result::Err(_) => invalid(),
        };
        let mut bytes = [0_u8; SIZE];
        let read = self::fallow_std::os::unix::fs::FileExt::read_exact_at(&file, &mut bytes, 0);
        if read.is_err() {
            invalid();
        }
        if bytes[..PREFIX.len()] != *PREFIX {
            return 0;
        }
        match self::fallow_std::str::from_utf8(&bytes[PREFIX.len()..]) {
            self::fallow_std::result::Result::Ok(text) => match u64::from_str_radix(text, 16) {
                self::fallow_std::result::Result::Ok(id) => id,
                self::fallow_std::result::Result::Err(_) => invalid(),
            },
            self::fallow_std::result::Result::Err(_) => invalid(),
        }
    }
    #[cfg(not(unix))]
    fn read_active() -> u64 {
        match self::fallow_std::env::var("FALLOW_MUTANT") {
            self::fallow_std::result::Result::Ok(value) => {
                match u64::from_str_radix(&value, 16) {
                    self::fallow_std::result::Result::Ok(id) => id,
                    self::fallow_std::result::Result::Err(_) => invalid(),
                }
            }
            self::fallow_std::result::Result::Err(self::fallow_std::env::VarError::NotPresent) => 0,
            self::fallow_std::result::Result::Err(_) => invalid(),
        }
    }
    fn invalid() -> ! {
        self::fallow_std::eprintln!("cargo-fallow: the record of the active mutant is not valid");
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
/// stays plain text. Pass `is_root` for a crate root file when any mutant of the
/// crate is enabled. The helper goes at the end of the file, so the first line of
/// the file stays the first line, also when that line is a shebang.
pub fn render(
    original: &str,
    sites: &[Site],
    enabled: &BTreeSet<u64>,
    is_root: bool,
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
    writer.range(0..original.len(), &tree);
    if is_root {
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
        if let Shape::ArmGuard { pattern_end, guard } = site.shape {
            self.arm_guard(node, pattern_end, guard);
            return;
        }
        let start = self.out.text.len();
        // A function body must stay a block.
        let block = site.shape == Shape::Block;
        if block {
            self.out.text.push_str("{ ");
        }
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
        if block {
            self.out.text.push_str(" }");
        }
        self.out.regions.push(Region {
            range: start..self.out.text.len(),
            site: node.site,
            mutant: None,
        });
    }

    /// Writes a match arm with a guard that is false when one of its mutants is
    /// active: `pat if ID != M && (guard) => body`. The arm then takes no value,
    /// and that is the same as a deleted arm.
    fn arm_guard(&mut self, node: &Node, pattern_end: usize, guard: Option<(usize, usize)>) {
        let site = &self.sites[node.site];
        let start = self.out.text.len();
        self.push(site.range.start..pattern_end);
        self.out.text.push_str(" if ");
        let mut first = true;
        for mutant in site.mutants.iter().filter(|m| self.enabled.contains(&m.id)) {
            if !first {
                self.out.text.push_str(" && ");
            }
            first = false;
            let at = self.out.text.len();
            // A write to a `String` cannot fail.
            let _ = match self.mode {
                Mode::Probe => write!(self.out.text, "{PROBE_PREFIX}{:x}", mutant.id),
                Mode::Schema => write!(self.out.text, "{ACTIVE_CALL} != {:#x}_u64", mutant.id),
            };
            self.out.regions.push(Region {
                range: at..self.out.text.len(),
                site: node.site,
                mutant: Some(mutant.id),
            });
        }
        let rest_start = match guard {
            Some((guard_start, guard_end)) => {
                let inside: Vec<&Node> = node
                    .children
                    .iter()
                    .filter(|child| self.sites[child.site].range.start < guard_end)
                    .collect();
                self.out.text.push_str(" && (");
                self.range_of_nodes(guard_start..guard_end, &inside);
                self.out.text.push(')');
                guard_end
            }
            None => pattern_end,
        };
        let after: Vec<&Node> = node
            .children
            .iter()
            .filter(|child| self.sites[child.site].range.start >= rest_start)
            .collect();
        self.range_of_nodes(rest_start..site.range.end, &after);
        self.out.regions.push(Region {
            range: start..self.out.text.len(),
            site: node.site,
            mutant: None,
        });
    }

    /// The same as [`Self::range`] for a list of references.
    fn range_of_nodes(&mut self, range: Range<usize>, nodes: &[&Node]) {
        let mut cursor = range.start;
        for node in nodes {
            let site_range = self.sites[node.site].range.clone();
            self.push(cursor..site_range.start);
            self.switch(node);
            cursor = site_range.end;
        }
        self.push(cursor..range.end);
    }

    fn arm(&mut self, index: usize, mutant: &Mutant) {
        let site = &self.sites[index];
        let start = self.out.text.len();
        // A write to a `String` cannot fail.
        let _ = write!(self.out.text, "{:#x}_u64 => (", mutant.id);
        match self.mode {
            Mode::Probe => {
                let _ = write!(self.out.text, "{PROBE_PREFIX}{:x}", mutant.id);
            }
            Mode::Schema => self
                .out
                .text
                .push_str(&mutated_text(self.original, site, mutant)),
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

/// The text of a site with the mutant applied. A nested site stays plain.
pub fn mutated_text(original: &str, site: &Site, mutant: &Mutant) -> String {
    let text = mutant.text.as_deref().unwrap_or_default();
    match site.shape {
        Shape::Splice { start, end } => format!(
            "{}{text}{}",
            &original[site.range.start..start],
            &original[end..site.range.end]
        ),
        Shape::Block => text.to_owned(),
        // The writer of an arm writes no mutated text.
        Shape::ArmGuard { .. } => String::new(),
        Shape::Condition => format!("!({})", &original[site.range.clone()]),
        Shape::Binary { lhs_end, rhs_start } => {
            let lhs = &original[site.range.start..lhs_end];
            let rhs = &original[rhs_start..site.range.end];
            // `x as usize < y` is a syntax error: the parser reads `<` as the
            // start of generic arguments.
            if mutant.replacement == "<" {
                format!("({lhs}) < {rhs}")
            } else {
                format!("{lhs} {} {rhs}", mutant.replacement)
            }
        }
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
            render(source, &found.sites, &enabled, false, Mode::Schema),
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

    /// Renders the sites of the rebuild operators in `source`, each one enabled.
    fn render_rewrites(source: &str) -> (String, Vec<Site>) {
        let Ok(found) =
            crate::mutation::rewrites::find_rewrites(source, Edition::Edition2021, "src/lib.rs")
        else {
            panic!("the source parses");
        };
        let sites = crate::mutation::rewrites::schema_sites(&found.rewrites);
        let enabled = all_ids(&sites);
        let out = render(source, &sites, &enabled, false, Mode::Schema);
        (readable(&out.text, &sites), sites)
    }

    #[test]
    fn an_arithmetic_operator_becomes_a_switch_of_its_expression() {
        let (text, _) = render_rewrites("fn f(a: u8) -> u8 { a + 1 }");
        assert!(
            text.contains("_ => ({ (match ID { M2 => (a - 1), _ => (a + 1) }) })"),
            "{text}"
        );
    }

    #[test]
    fn a_return_value_switch_keeps_the_body_a_block() {
        let (text, _) = render_rewrites("fn f(a: u8) -> Option<u8> { Some(a) }");
        assert_eq!(
            text,
            "fn f(a: u8) -> Option<u8> { (match ID { M0 => ({ None }), M1 => ({ Some(0) }), \
             M2 => ({ Some(1) }), _ => ({ Some(a) }) }) }"
        );
    }

    #[test]
    fn a_deleted_arm_gets_a_guard_and_keeps_its_own_guard() {
        let (text, _) =
            render_rewrites("fn f(a: u8) -> u8 { match a { 1 => 2, x if x > 3 => 4, _ => 5 } }");
        assert!(text.contains("1 if ID != M"), "{text}");
        assert!(text.contains(" => 2,"), "{text}");
        // The guard of the second arm keeps its own switch of the match guard.
        assert!(text.contains("x if ID != M"), "{text}");
        assert!(text.contains(" && ((match ID { M"), "{text}");
        assert!(text.contains("_ => (x > 3) })) => 4"), "{text}");
    }

    #[test]
    fn a_unary_operator_switch_holds_the_operand() {
        let (text, _) = render_rewrites("fn f(a: i8) -> i8 { -a }");
        assert!(
            text.contains("(match ID { M3 => (a), _ => (-a) })"),
            "{text}"
        );
    }

    #[test]
    fn a_rebuild_mutant_in_a_constant_context_has_no_schema_site() {
        let (_, sites) = render_rewrites("const fn f(a: u8) -> u8 { a + 1 }");
        assert!(sites.is_empty());
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
            false,
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
        let out = render(source, &found.sites, &BTreeSet::new(), false, Mode::Schema);
        assert_eq!(out.text, source);
        assert!(out.regions.is_empty());
    }

    #[test]
    fn disabled_outer_site_keeps_the_inner_switch() {
        let source = "fn f(a: bool) -> bool { a == a && a }";
        let found = parse(source);
        let inner: BTreeSet<u64> = [found.sites[1].mutants[0].id].into();
        let out = render(source, &found.sites, &inner, false, Mode::Schema);
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
    fn crate_root_keeps_its_first_lines_and_gets_the_helper_at_the_end() {
        let source = "#!/usr/bin/env tool\n#![forbid(unused_parens)]\nfn f() -> bool { 1 == 2 }";
        let found = parse(source);
        let out = render(
            source,
            &found.sites,
            &all_ids(&found.sites),
            true,
            Mode::Schema,
        );
        assert!(
            out.text
                .starts_with("#!/usr/bin/env tool\n#![forbid(unused_parens)]\nfn f()"),
            "{}",
            out.text
        );
        assert!(out.text.contains("mod __fallow_schema"));
        assert!(out.text.contains("extern crate std as fallow_std;"));
    }

    #[cfg(unix)]
    #[test]
    fn the_record_has_the_size_that_the_helper_reads() {
        let record = mutant_record(0xfa);
        assert_eq!(record, "fallow-mutant:00000000000000fa");
        assert!(HELPER.contains(&format!("const SIZE: usize = {};", record.len())));
        assert!(HELPER.contains(&format!("b\"{RECORD_PREFIX}\"")));
        assert!(HELPER.contains(&format!("\"/dev/fd/{MUTANT_FD}\"")));
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
