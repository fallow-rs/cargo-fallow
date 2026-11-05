//! The credits of the module graph, with the name credits in groups.
//!
//! A name credit gives credit to each item with one name, for example each
//! field `a` for a field access `.a`. A generated file can have thousands of
//! fields with one name and thousands of accesses, so one credit for each pair
//! needs memory that grows with their product. A group holds the items of one
//! name once, and each site of the name refers to its group. The memory then
//! grows with the sites and the items.

use rustc_hash::FxHashMap;

use crate::graph::extract::ItemKind;
use crate::graph::resolve::Credit;
use crate::graph::{CrateId, ItemId};

/// The item kinds that a name credit can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NameFilter {
    /// Each kind except a field and a method: a bare name in tokens.
    NotMember,
    /// The last name of a path that did not resolve: no field, and no method
    /// when the path has one name.
    PathEnd {
        /// True when the path has one name.
        single: bool,
    },
    /// A method: a method call.
    Method,
    /// A field: a field access, or a field of an unknown struct.
    Field,
    /// A field or a method: a member name in tokens.
    Member,
    /// A `macro_rules!` macro: a macro call.
    Macro,
}

impl NameFilter {
    /// True when the filter keeps an item of this kind.
    pub fn keeps(self, kind: ItemKind) -> bool {
        match self {
            Self::NotMember => !matches!(kind, ItemKind::Field | ItemKind::Method),
            Self::PathEnd { single } => {
                kind != ItemKind::Field && !(single && kind == ItemKind::Method)
            }
            Self::Method => kind == ItemKind::Method,
            Self::Field => kind == ItemKind::Field,
            Self::Member => matches!(kind, ItemKind::Field | ItemKind::Method),
            Self::Macro => kind == ItemKind::Macro,
        }
    }
}

/// One site of a name credit: the code, and the group of the items that it
/// can name. The `item` field of the credit is not used.
#[derive(Debug, Clone, Copy)]
pub struct Site {
    /// The index into [`Credits::groups`].
    pub group: usize,
    /// The credit without its item.
    pub credit: Credit,
}

/// The key of a group: the filter, the crate of the code and true for the
/// crate of a documentation test. The name is the inner key.
type GroupKey = (NameFilter, CrateId, bool);

/// The credits of every import and every reference of the graph.
#[derive(Debug, Default)]
pub struct Credits {
    /// The credits of one item each.
    pub direct: Vec<Credit>,
    /// The items of each group, in ascending order.
    pub groups: Vec<Vec<ItemId>>,
    /// The sites of the name credits.
    pub sites: Vec<Site>,
    index: FxHashMap<GroupKey, FxHashMap<String, usize>>,
}

impl Credits {
    /// The group of a name for code in `krate`, made with `targets` on the
    /// first use.
    pub fn group(
        &mut self,
        key: GroupKey,
        name: &str,
        targets: impl FnOnce() -> Vec<ItemId>,
    ) -> usize {
        if let Some(&group) = self.index.get(&key).and_then(|names| names.get(name)) {
            return group;
        }
        let mut items = targets();
        items.sort_unstable();
        items.dedup();
        let group = self.groups.len();
        self.groups.push(items);
        self.index
            .entry(key)
            .or_default()
            .insert(name.to_owned(), group);
        group
    }

    /// The items of the group of a site.
    pub fn targets(&self, site: &Site) -> &[ItemId] {
        &self.groups[site.group]
    }

    /// Each credit of one item, with the sites of its groups as credits.
    pub fn for_item(&self, item: ItemId) -> Vec<Credit> {
        let mut credits: Vec<Credit> = self
            .direct
            .iter()
            .filter(|credit| credit.item == item)
            .copied()
            .collect();
        credits.extend(
            self.sites
                .iter()
                .filter(|site| self.groups[site.group].binary_search(&item).is_ok())
                .map(|site| Credit {
                    item,
                    ..site.credit
                }),
        );
        credits
    }
}
