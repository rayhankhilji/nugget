//! User splits, remembered as constraints on entity resolution.
//!
//! When the user splits entity A into A and B ([`crate::Knowledge::record_split`]), re-extracting
//! the sources must not undo it. Each split is kept (under its own state key, `knowledge/splits`,
//! written only by `record_split`, so a running pipeline never overwrites one) with:
//! - the pair `(A, B)`: resolution never merges them, whatever ids they end up under (merges are
//!   followed; a pair the user merged back together, or with a side gone, no longer applies);
//! - B's names and identifiers at split time;
//! - the items whose evidence went to B only (the items whose mentions moved).
//!
//! An occurrence that resolves to A goes to B instead when it comes from one of B's items,
//! carries one of the identifiers that went to B, or is called by one of B's names that A no
//! longer has; when both A and B match an
//! occurrence by name, the same rule picks one of them.

use nugget_core::Result;
use nugget_core::ids::{EntityId, ItemRef};
use nugget_graph::{GraphStore, Identifier, normalize_name};
use serde::{Deserialize, Serialize};

use crate::feed::ItemFeed;

pub(crate) const SPLITS_KEY: &str = "knowledge/splits";
/// Splits remembered at most (oldest dropped).
pub(crate) const MAX_SPLITS: usize = 10_000;
/// Items remembered per split.
pub(crate) const MAX_SPLIT_ITEMS: usize = 5_000;
/// Names (and identifiers) remembered per split.
const MAX_SPLIT_NAMES: usize = 256;

/// One user split: `to` was split off `from`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Split {
    pub from: EntityId,
    pub to: EntityId,
    /// `to`'s name and aliases at split time, normalised.
    #[serde(default)]
    pub names: Vec<String>,
    /// `to`'s identifiers at split time.
    #[serde(default)]
    pub identifiers: Vec<Identifier>,
    /// Items whose evidence went to `to` only.
    #[serde(default)]
    pub items: Vec<ItemRef>,
}

pub(crate) fn load(feed: &dyn ItemFeed) -> Result<Vec<Split>> {
    Ok(match feed.get_state(SPLITS_KEY)? {
        Some(v) => serde_json::from_value(v).unwrap_or_default(),
        None => Vec::new(),
    })
}

pub(crate) fn save(feed: &dyn ItemFeed, splits: &[Split]) -> Result<()> {
    feed.set_state(SPLITS_KEY, serde_json::to_value(splits)?)
}

/// What a split looks like right after `GraphStore::split` moved `to` off `from`.
pub(crate) fn describe(
    graph: &dyn GraphStore,
    from: &EntityId,
    to: &EntityId,
) -> Result<Option<Split>> {
    let (Some(a), Some(b)) = (graph.get_entity(from)?, graph.get_entity(to)?) else {
        return Ok(None);
    };
    if a.id == b.id {
        return Ok(None);
    }
    let mut names: Vec<String> = std::iter::once(&b.name)
        .chain(&b.aliases)
        .map(|n| normalize_name(n))
        .filter(|n| !n.is_empty())
        .collect();
    names.sort();
    names.dedup();
    names.truncate(MAX_SPLIT_NAMES);
    let mut identifiers = b.identifiers.clone();
    identifiers.truncate(MAX_SPLIT_NAMES);
    // Items supporting a claim still on A are shared: they decide nothing.
    let mut staying: Vec<ItemRef> = Vec::new();
    for c in graph.claims_for(&a.id, true)? {
        let on_b = c.subject == b.id || c.object == nugget_graph::ClaimObject::Entity(b.id.clone());
        if !on_b {
            staying.extend(c.evidence.into_iter().map(|e| e.item));
        }
    }
    let mut items: Vec<ItemRef> = Vec::new();
    let moved = graph
        .claims_for(&b.id, true)?
        .into_iter()
        .flat_map(|c| c.evidence.into_iter().map(|e| e.item))
        .chain(
            graph
                .mentions_for(&b.id, MAX_SPLIT_ITEMS)?
                .into_iter()
                .map(|m| m.item),
        );
    for item in moved {
        if items.len() >= MAX_SPLIT_ITEMS {
            break;
        }
        if !staying.contains(&item) && !items.contains(&item) {
            items.push(item);
        }
    }
    Ok(Some(Split {
        from: a.id,
        to: b.id,
        names,
        identifiers,
        items,
    }))
}

/// A split as it applies now: both sides followed through merges.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Live {
    pub from: EntityId,
    pub to: EntityId,
    /// Index into the split list.
    pub idx: usize,
}

/// The splits that still apply: both sides alive and still apart.
pub(crate) fn live(graph: &dyn GraphStore, splits: &[Split]) -> Result<Vec<Live>> {
    let mut out = Vec::new();
    for (idx, s) in splits.iter().enumerate() {
        if let (Some(a), Some(b)) = (graph.get_entity(&s.from)?, graph.get_entity(&s.to)?)
            && a.id != b.id
        {
            out.push(Live {
                from: a.id,
                to: b.id,
                idx,
            });
        }
    }
    Ok(out)
}

/// Whether a split keeps `a` and `b` apart.
pub(crate) fn apart(live: &[Live], a: &EntityId, b: &EntityId) -> bool {
    live.iter()
        .any(|l| (&l.from == a && &l.to == b) || (&l.from == b && &l.to == a))
}
