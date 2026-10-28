//! Persistent pipeline state, kept in the store's cursor table through [`ItemFeed`]:
//!
//! - `knowledge`: [`State`] (extractor version, the user's entity and learned addresses, the
//!   sources seen, unresolved wikilinks);
//! - `knowledge/l/<bucket>/<source>`: the **ledger**, one entry per processed item (connector
//!   version, content fingerprint, LLM status), sharded into [`BUCKETS`] buckets per source so a
//!   flush rewrites only what changed;
//! - `knowledge/notes`: the notes index used to resolve wikilinks.
//!
//! The ledger is written only after an item's extraction is fully applied, so a crash or a
//! budget cut resumes where it stopped; re-processing an item is idempotent (its evidence is
//! retracted first).

use std::collections::{BTreeMap, HashMap, HashSet};

use nugget_core::Result;
use nugget_core::ids::{EntityId, ItemKey, ItemRef, SourceId};
use nugget_graph::normalize_name;
use serde::{Deserialize, Serialize};

use crate::feed::ItemFeed;

/// Bump to re-extract everything after extractor changes.
pub const EXTRACTOR_VERSION: u32 = 1;
pub(crate) const STATE_KEY: &str = "knowledge";
pub(crate) const NOTES_KEY: &str = "knowledge/notes";
pub(crate) const LINKS_KEY: &str = "knowledge/links";
pub(crate) const BUCKETS: usize = 64;
/// Unresolved wikilinks kept at most (oldest dropped).
pub(crate) const MAX_PENDING_LINKS: usize = 20_000;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct State {
    pub version: u32,
    pub me: Option<EntityId>,
    /// Addresses learned to be the user's (lower case).
    pub me_emails: Vec<String>,
    /// Sources with ledger entries.
    pub sources: Vec<SourceId>,
    /// Kept under its own key ([`LINKS_KEY`]) and written only when it changed.
    #[serde(skip)]
    pub pending_links: Vec<PendingLink>,
    #[serde(skip)]
    pub links_dirty: bool,
    pub processed_total: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PendingLink {
    pub item: ItemRef,
    pub from: EntityId,
    pub target: String,
    pub chunk_id: Option<i64>,
    pub quote: Option<String>,
    pub confidence: f32,
}

impl State {
    pub fn load(feed: &dyn ItemFeed) -> Result<Self> {
        let mut s: Self = match feed.get_state(STATE_KEY)? {
            Some(v) => serde_json::from_value(v).unwrap_or_default(),
            None => Self::default(),
        };
        if let Some(v) = feed.get_state(LINKS_KEY)? {
            s.pending_links = serde_json::from_value(v).unwrap_or_default();
        }
        Ok(s)
    }

    pub fn save(&mut self, feed: &dyn ItemFeed) -> Result<()> {
        if self.links_dirty {
            feed.set_state(LINKS_KEY, serde_json::to_value(&self.pending_links)?)?;
            self.links_dirty = false;
        }
        feed.set_state(STATE_KEY, serde_json::to_value(&*self)?)
    }
}

/// LLM status of a processed item.
pub(crate) const LLM_NONE: u32 = 0;
pub(crate) const LLM_DONE: u32 = 1;
/// `LLM_PENDING + n`: wanted, not done yet, `n` failed attempts.
pub(crate) const LLM_PENDING: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    /// Connector version token when processed.
    #[serde(rename = "v", default)]
    pub version: Option<String>,
    /// Content fingerprint (content hash and store `updated_at`).
    #[serde(rename = "f")]
    pub fingerprint: String,
    #[serde(rename = "l", default)]
    pub llm: u32,
    /// Consecutive processing failures.
    #[serde(rename = "e", default)]
    pub failures: u32,
}

/// Ledger of one source.
#[derive(Debug, Default)]
pub(crate) struct SourceLedger {
    buckets: Vec<BTreeMap<String, Entry>>,
    dirty: HashSet<usize>,
}

fn bucket_of(key: &str) -> usize {
    // FNV-1a: stable across runs and platforms.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (h % BUCKETS as u64) as usize
}

fn bucket_key(source: &SourceId, bucket: usize) -> String {
    format!("knowledge/l/{bucket:02}/{}", source.as_str())
}

impl SourceLedger {
    pub fn load(feed: &dyn ItemFeed, source: &SourceId) -> Result<Self> {
        let mut buckets = Vec::with_capacity(BUCKETS);
        for b in 0..BUCKETS {
            let map = match feed.get_state(&bucket_key(source, b))? {
                Some(v) => serde_json::from_value(v).unwrap_or_default(),
                None => BTreeMap::new(),
            };
            buckets.push(map);
        }
        Ok(Self {
            buckets,
            dirty: HashSet::new(),
        })
    }

    pub fn get(&self, key: &ItemKey) -> Option<&Entry> {
        self.buckets[bucket_of(key.as_str())].get(key.as_str())
    }

    pub fn set(&mut self, key: &ItemKey, e: Entry) {
        let b = bucket_of(key.as_str());
        self.buckets[b].insert(key.as_str().to_owned(), e);
        self.dirty.insert(b);
    }

    pub fn remove(&mut self, key: &ItemKey) {
        let b = bucket_of(key.as_str());
        if self.buckets[b].remove(key.as_str()).is_some() {
            self.dirty.insert(b);
        }
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.buckets.iter().flat_map(|b| b.keys())
    }

    pub fn is_empty(&self) -> bool {
        self.buckets.iter().all(BTreeMap::is_empty)
    }

    pub fn flush(&mut self, feed: &dyn ItemFeed, source: &SourceId) -> Result<()> {
        let mut dirty: Vec<usize> = self.dirty.drain().collect();
        dirty.sort_unstable();
        for b in dirty {
            let key = bucket_key(source, b);
            if self.buckets[b].is_empty() {
                feed.clear_state(&key)?;
            } else {
                feed.set_state(&key, serde_json::to_value(&self.buckets[b])?)?;
            }
        }
        Ok(())
    }

    /// Forget every entry (and persist that on the next flush).
    pub fn clear(&mut self) {
        for (i, b) in self.buckets.iter_mut().enumerate() {
            if !b.is_empty() {
                b.clear();
                self.dirty.insert(i);
            }
        }
    }
}

/// Notes by the names other notes link to them with.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct NoteEntry {
    pub names: Vec<String>,
    pub subjects: Vec<EntityId>,
}

#[derive(Debug, Default)]
pub(crate) struct Notes {
    /// `source \u{1f} key` → entry.
    entries: BTreeMap<String, NoteEntry>,
    by_name: Option<HashMap<String, Vec<String>>>,
    pub dirty: bool,
}

fn note_id(item: &ItemRef) -> String {
    format!("{}\u{1f}{}", item.source.as_str(), item.key.as_str())
}

impl Notes {
    pub fn load(feed: &dyn ItemFeed) -> Result<Self> {
        let entries = match feed.get_state(NOTES_KEY)? {
            Some(v) => serde_json::from_value(v).unwrap_or_default(),
            None => BTreeMap::new(),
        };
        Ok(Self {
            entries,
            by_name: None,
            dirty: false,
        })
    }

    pub fn save(&mut self, feed: &dyn ItemFeed) -> Result<()> {
        if self.dirty {
            feed.set_state(NOTES_KEY, serde_json::to_value(&self.entries)?)?;
            self.dirty = false;
        }
        Ok(())
    }

    pub fn remove(&mut self, item: &ItemRef) {
        if self.entries.remove(&note_id(item)).is_some() {
            self.dirty = true;
            self.by_name = None;
        }
    }

    pub fn set(&mut self, item: &ItemRef, names: &[String], subjects: Vec<EntityId>) {
        let mut ns: Vec<String> = names
            .iter()
            .map(|n| normalize_name(n))
            .filter(|n| !n.is_empty())
            .collect();
        ns.sort();
        ns.dedup();
        if subjects.is_empty() || ns.is_empty() {
            self.remove(item);
            return;
        }
        self.entries.insert(
            note_id(item),
            NoteEntry {
                names: ns,
                subjects,
            },
        );
        self.dirty = true;
        self.by_name = None;
    }

    /// Subjects of the one note called `name`; empty if none or ambiguous.
    pub fn lookup(&mut self, name: &str) -> Vec<EntityId> {
        let index = self.by_name.get_or_insert_with(|| {
            let mut m: HashMap<String, Vec<String>> = HashMap::new();
            for (id, e) in &self.entries {
                for n in &e.names {
                    m.entry(n.clone()).or_default().push(id.clone());
                }
            }
            m
        });
        match index.get(&normalize_name(name)).map(Vec::as_slice) {
            Some([one]) => self.entries[one].subjects.clone(),
            _ => Vec::new(),
        }
    }
}
