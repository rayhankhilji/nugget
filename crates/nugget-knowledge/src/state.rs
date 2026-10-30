//! Persistent pipeline state, kept in the store's cursor table through [`ItemFeed`]:
//!
//! - `knowledge`: [`State`] (extractor and time-model versions, the user's entity and learned
//!   addresses, the sources seen, the item change-log cursor and retry hints, unresolved
//!   wikilinks);
//! - `knowledge/lb/<bucket>/<source>`: the **ledger**, one entry per processed item (connector
//!   version, content fingerprint, LLM status), sharded into [`BUCKETS`] buckets per source so a
//!   flush rewrites only what changed (a flush of `flush_every` items dirties about that many
//!   buckets, so the buckets must be many and small for a flush to cost much less than the
//!   whole ledger);
//! - `knowledge/nb/<bucket>`: the notes index used to resolve wikilinks, sharded the same way.
//!
//! Earlier versions kept the ledger in [`LEGACY_BUCKETS`] buckets (`knowledge/l/<bucket>/...`)
//! and the notes under one key (`knowledge/notes`); both are read, merged and moved to the
//! current layout by the next flush.
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

/// Bump to re-extract everything after extractor changes. A run that finds an older version
/// re-extracts every item (each item's old evidence is retracted first), so the new resolver
/// folds duplicates the old one left and entities nothing supports any more are pruned.
///
/// - 1: M2.
/// - 2: W7 resolution and Markdown fixes (trusted note identifiers, name-only twins folded,
///   organisations linked across name and mail domain, verbatim frontmatter quotes).
///
/// Deliberately not bumped for AA3's LLM extraction (compact schema, local limits, kind
/// adoption): new and changed items use the new path, but re-extracting a whole vault costs one
/// local-model request per item (26–33 s each on a 2-core CPU), too much to start silently.
pub const EXTRACTOR_VERSION: u32 = 2;
/// Version of the claims' time model. A run that finds an older one in [`State::temporal`]
/// first migrates the graph, without the LLM and without re-extracting anything
/// (`Session::backfill_validity`): claims get `valid_from` from their evidence dates, and
/// single-valued predicates are settled again (one current value, dated history). Kept apart
/// from [`EXTRACTOR_VERSION`], whose bump would re-extract every item with the LLM.
///
/// - 1: EE1 validity intervals, past-tense and start cues, order-independent supersession.
pub(crate) const TEMPORAL_VERSION: u32 = 1;
pub(crate) const STATE_KEY: &str = "knowledge";
/// The pre-sharding notes index (read for migration, cleared once moved).
pub(crate) const NOTES_KEY: &str = "knowledge/notes";
pub(crate) const LINKS_KEY: &str = "knowledge/links";
pub(crate) const BUCKETS: usize = 1024;
/// Bucket count of the previous ledger layout.
pub(crate) const LEGACY_BUCKETS: usize = 64;
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
    /// Position in the feed's item change log up to which every change was processed. `None`:
    /// the next run diffs every source against the ledger (first run, after a reset, after an
    /// extractor upgrade, or with a feed that keeps no change log).
    pub item_cursor: Option<u64>,
    /// Some ledger entry may be due for a retry after a processing failure.
    pub retry_failed: bool,
    /// Some ledger entry may be waiting for the LLM extractor.
    pub retry_llm: bool,
    /// [`TEMPORAL_VERSION`] the graph was last migrated to (0: written before validity
    /// intervals, or a new vault).
    pub temporal: u32,
    /// The migration's progress: the last entity (in id order) it finished, while unfinished.
    pub temporal_cursor: Option<EntityId>,
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
#[derive(Debug)]
pub(crate) struct SourceLedger {
    buckets: Vec<BTreeMap<String, Entry>>,
    dirty: HashSet<usize>,
    /// Entries were read from the legacy layout, whose keys the next flush clears.
    legacy: bool,
}

impl Default for SourceLedger {
    fn default() -> Self {
        Self {
            buckets: vec![BTreeMap::new(); BUCKETS],
            dirty: HashSet::new(),
            legacy: false,
        }
    }
}

fn hash_bucket(key: &str, buckets: usize) -> usize {
    // FNV-1a: stable across runs and platforms.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (h % buckets as u64) as usize
}

fn bucket_of(key: &str) -> usize {
    hash_bucket(key, BUCKETS)
}

fn bucket_key(source: &SourceId, bucket: usize) -> String {
    format!("knowledge/lb/{bucket:04}/{}", source.as_str())
}

fn legacy_bucket_key(source: &SourceId, bucket: usize) -> String {
    format!("knowledge/l/{bucket:02}/{}", source.as_str())
}

/// Buckets read from `key(b)` for `b < n`, decoded (an undecodable bucket reads as empty).
fn load_buckets<T: serde::de::DeserializeOwned>(
    feed: &dyn ItemFeed,
    n: usize,
    key: impl Fn(usize) -> String,
) -> Result<Vec<Option<BTreeMap<String, T>>>> {
    (0..n)
        .map(|b| {
            Ok(feed
                .get_state(&key(b))?
                .map(|v| serde_json::from_value(v).unwrap_or_default()))
        })
        .collect()
}

impl SourceLedger {
    pub fn load(feed: &dyn ItemFeed, source: &SourceId) -> Result<Self> {
        let mut me = Self::default();
        for (b, map) in load_buckets(feed, BUCKETS, |b| bucket_key(source, b))?
            .into_iter()
            .enumerate()
        {
            me.buckets[b] = map.unwrap_or_default();
        }
        // Entries of the legacy layout not in the current one move over (current wins: it was
        // written later), and their old keys go at the next flush.
        for map in load_buckets::<Entry>(feed, LEGACY_BUCKETS, |b| legacy_bucket_key(source, b))?
            .into_iter()
            .flatten()
        {
            me.legacy = true;
            for (k, e) in map {
                let b = bucket_of(&k);
                if let std::collections::btree_map::Entry::Vacant(v) = me.buckets[b].entry(k) {
                    v.insert(e);
                    me.dirty.insert(b);
                }
            }
        }
        Ok(me)
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
        if self.legacy {
            for b in 0..LEGACY_BUCKETS {
                feed.clear_state(&legacy_bucket_key(source, b))?;
            }
            self.legacy = false;
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

/// Notes index, sharded into [`BUCKETS`] buckets by note id like the ledger.
#[derive(Debug)]
pub(crate) struct Notes {
    /// Per bucket: `source \u{1f} key` → entry.
    buckets: Vec<BTreeMap<String, NoteEntry>>,
    /// Name → note ids, built on the first lookup and kept up to date by `set` and `remove`.
    by_name: Option<HashMap<String, Vec<String>>>,
    dirty: HashSet<usize>,
    /// Entries were read from the legacy single key, which the next save clears.
    legacy: bool,
}

fn note_id(item: &ItemRef) -> String {
    format!("{}\u{1f}{}", item.source.as_str(), item.key.as_str())
}

fn notes_key(bucket: usize) -> String {
    format!("knowledge/nb/{bucket:04}")
}

impl Notes {
    pub fn load(feed: &dyn ItemFeed) -> Result<Self> {
        let mut buckets: Vec<BTreeMap<String, NoteEntry>> = load_buckets(feed, BUCKETS, notes_key)?
            .into_iter()
            .map(Option::unwrap_or_default)
            .collect();
        let mut dirty = HashSet::new();
        let legacy = match feed.get_state(NOTES_KEY)? {
            Some(v) => {
                let old: BTreeMap<String, NoteEntry> =
                    serde_json::from_value(v).unwrap_or_default();
                for (id, e) in old {
                    let b = bucket_of(&id);
                    if let std::collections::btree_map::Entry::Vacant(v) = buckets[b].entry(id) {
                        v.insert(e);
                        dirty.insert(b);
                    }
                }
                true
            }
            None => false,
        };
        Ok(Self {
            buckets,
            by_name: None,
            dirty,
            legacy,
        })
    }

    pub fn save(&mut self, feed: &dyn ItemFeed) -> Result<()> {
        let mut dirty: Vec<usize> = self.dirty.drain().collect();
        dirty.sort_unstable();
        for b in dirty {
            let key = notes_key(b);
            if self.buckets[b].is_empty() {
                feed.clear_state(&key)?;
            } else {
                feed.set_state(&key, serde_json::to_value(&self.buckets[b])?)?;
            }
        }
        if self.legacy {
            feed.clear_state(NOTES_KEY)?;
            self.legacy = false;
        }
        Ok(())
    }

    pub fn remove(&mut self, item: &ItemRef) {
        let id = note_id(item);
        let b = bucket_of(&id);
        if let Some(old) = self.buckets[b].remove(&id) {
            self.dirty.insert(b);
            if let Some(index) = self.by_name.as_mut() {
                for n in &old.names {
                    if let Some(ids) = index.get_mut(n) {
                        ids.retain(|i| i != &id);
                        if ids.is_empty() {
                            index.remove(n);
                        }
                    }
                }
            }
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
        // Drops the old entry and its names from the index.
        self.remove(item);
        if subjects.is_empty() || ns.is_empty() {
            return;
        }
        let id = note_id(item);
        if let Some(index) = self.by_name.as_mut() {
            for n in &ns {
                index.entry(n.clone()).or_default().push(id.clone());
            }
        }
        let b = bucket_of(&id);
        self.buckets[b].insert(
            id,
            NoteEntry {
                names: ns,
                subjects,
            },
        );
        self.dirty.insert(b);
    }

    /// Subjects of the one note called `name`; empty if none or ambiguous.
    pub fn lookup(&mut self, name: &str) -> Vec<EntityId> {
        let index = self.by_name.get_or_insert_with(|| {
            let mut m: HashMap<String, Vec<String>> = HashMap::new();
            for (id, e) in self.buckets.iter().flatten() {
                for n in &e.names {
                    m.entry(n.clone()).or_default().push(id.clone());
                }
            }
            m
        });
        match index.get(&normalize_name(name)).map(Vec::as_slice) {
            Some([one]) => self.buckets[bucket_of(one)][one].subjects.clone(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_store::Store;
    use serde_json::json;

    fn item(key: &str) -> ItemRef {
        ItemRef {
            source: SourceId::new("notes"),
            key: ItemKey::new(key),
        }
    }

    fn entry(f: &str) -> Entry {
        Entry {
            version: None,
            fingerprint: f.into(),
            llm: LLM_NONE,
            failures: 0,
        }
    }

    #[test]
    fn a_legacy_ledger_is_read_and_moved_to_the_current_layout() {
        let feed = Store::open_in_memory().unwrap();
        let source = SourceId::new("notes");
        let old = legacy_bucket_key(&source, hash_bucket("a", LEGACY_BUCKETS));
        feed.set_state(&old, json!({"a": {"f": "fp-a"}})).unwrap();
        let mut l = SourceLedger::load(&feed, &source).unwrap();
        assert_eq!(l.get(&ItemKey::new("a")), Some(&entry("fp-a")));
        l.set(&ItemKey::new("b"), entry("fp-b"));
        l.flush(&feed, &source).unwrap();
        assert_eq!(feed.get_state(&old).unwrap(), None, "legacy bucket cleared");
        let l = SourceLedger::load(&feed, &source).unwrap();
        assert_eq!(l.get(&ItemKey::new("a")), Some(&entry("fp-a")));
        assert_eq!(l.get(&ItemKey::new("b")), Some(&entry("fp-b")));
        assert_eq!(l.keys().count(), 2);
    }

    #[test]
    fn a_legacy_notes_index_is_read_and_moved_to_buckets() {
        let feed = Store::open_in_memory().unwrap();
        let id = EntityId::new("ent_1");
        feed.set_state(
            NOTES_KEY,
            json!({note_id(&item("a.md")): {"names": ["alpha"], "subjects": [id]}}),
        )
        .unwrap();
        let mut notes = Notes::load(&feed).unwrap();
        assert_eq!(notes.lookup("Alpha"), vec![id.clone()]);
        notes.save(&feed).unwrap();
        assert_eq!(feed.get_state(NOTES_KEY).unwrap(), None);
        assert_eq!(Notes::load(&feed).unwrap().lookup("alpha"), vec![id]);
    }

    #[test]
    fn the_name_index_follows_set_and_remove() {
        let feed = Store::open_in_memory().unwrap();
        let mut notes = Notes::load(&feed).unwrap();
        let (x, y) = (EntityId::new("ent_x"), EntityId::new("ent_y"));
        notes.set(&item("a.md"), &["Alpha".into()], vec![x.clone()]);
        assert_eq!(notes.lookup("alpha"), vec![x.clone()]);
        notes.set(&item("b.md"), &["Alpha".into()], vec![y.clone()]);
        assert!(notes.lookup("alpha").is_empty(), "ambiguous");
        notes.remove(&item("a.md"));
        assert_eq!(notes.lookup("alpha"), vec![y.clone()]);
        notes.set(&item("b.md"), &["Beta".into()], vec![y.clone()]);
        assert!(notes.lookup("alpha").is_empty());
        assert_eq!(notes.lookup("beta"), vec![y.clone()]);
        notes.save(&feed).unwrap();
        let mut again = Notes::load(&feed).unwrap();
        assert_eq!(again.lookup("beta"), vec![y]);
        assert!(again.lookup("alpha").is_empty());
    }
}
