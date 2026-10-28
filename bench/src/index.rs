//! `FakeIndex`: an in-memory stand-in for `nugget-store` used by the M0 suites.
//!
//! It deliberately mirrors the store's surface (content-hash dedup on upsert, delete, BM25
//! text search, vector kNN and reciprocal-rank-fusion hybrid search) so suites written against
//! it port to the real store with little change. Embeddings are feature-hashed bags of words:
//! cheap, deterministic and good enough to exercise the vector path. They are not semantic.

use std::collections::HashMap;

use nugget_core::ids::ItemRef;

/// Result of an upsert, matching the store's `UpsertOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertOutcome {
    Inserted,
    Updated,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub item: ItemRef,
    pub score: f32,
}

#[derive(Debug)]
struct Entry {
    item: ItemRef,
    hash: u64,
    len: u32,
    terms: Vec<(u32, u32)>,
    vector: Vec<f32>,
}

/// BM25 parameters.
const K1: f32 = 1.2;
const B: f32 = 0.75;
/// Reciprocal-rank-fusion constant.
const RRF_K: f32 = 60.0;

#[derive(Debug)]
pub struct FakeIndex {
    dim: usize,
    by_item: HashMap<ItemRef, usize>,
    slots: Vec<Option<Entry>>,
    free: Vec<usize>,
    term_ids: HashMap<String, u32>,
    postings: Vec<HashMap<u32, u32>>,
    total_len: u64,
}

/// 64-bit FNV-1a. Used as the content hash and for feature hashing.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Lowercased alphanumeric tokens.
pub fn tokenize(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
}

impl FakeIndex {
    pub fn new(dim: usize) -> Self {
        assert!(dim > 0, "embedding dimension must be non-zero");
        Self {
            dim,
            by_item: HashMap::new(),
            slots: Vec::new(),
            free: Vec::new(),
            term_ids: HashMap::new(),
            postings: Vec::new(),
            total_len: 0,
        }
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Number of live items.
    pub fn len(&self) -> usize {
        self.by_item.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_item.is_empty()
    }

    pub fn contains(&self, item: &ItemRef) -> bool {
        self.by_item.contains_key(item)
    }

    /// Feature-hashed, L2-normalised bag-of-words embedding.
    pub fn embed(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; self.dim];
        for tok in tokenize(text) {
            let h = fnv1a(tok.as_bytes());
            let idx = (h % self.dim as u64) as usize;
            let sign = if h >> 63 == 0 { 1.0 } else { -1.0 };
            v[idx] += sign;
        }
        normalize(&mut v);
        v
    }

    /// Insert or replace an item's text. Unchanged content (by hash) is a no-op.
    pub fn upsert(&mut self, item: &ItemRef, text: &str) -> UpsertOutcome {
        let hash = fnv1a(text.as_bytes());
        let existing = self.by_item.get(item).copied();
        if let Some(slot) = existing
            && self.slots[slot].as_ref().is_some_and(|e| e.hash == hash)
        {
            return UpsertOutcome::Unchanged;
        }

        let mut tf: HashMap<u32, u32> = HashMap::new();
        let mut len = 0u32;
        for tok in tokenize(text) {
            len += 1;
            let next = self.term_ids.len() as u32;
            let id = *self.term_ids.entry(tok).or_insert(next);
            if id as usize == self.postings.len() {
                self.postings.push(HashMap::new());
            }
            *tf.entry(id).or_insert(0) += 1;
        }
        let vector = self.embed(text);

        let (slot, outcome) = match existing {
            Some(slot) => {
                self.remove_postings(slot);
                (slot, UpsertOutcome::Updated)
            }
            None => {
                let slot = match self.free.pop() {
                    Some(s) => s,
                    None => {
                        self.slots.push(None);
                        self.slots.len() - 1
                    }
                };
                self.by_item.insert(item.clone(), slot);
                (slot, UpsertOutcome::Inserted)
            }
        };

        let slot_id = slot as u32;
        let terms: Vec<(u32, u32)> = tf.into_iter().collect();
        for &(term, count) in &terms {
            self.postings[term as usize].insert(slot_id, count);
        }
        self.total_len += u64::from(len);
        self.slots[slot] = Some(Entry {
            item: item.clone(),
            hash,
            len,
            terms,
            vector,
        });
        outcome
    }

    /// Remove an item from every index. Returns whether it existed.
    pub fn delete(&mut self, item: &ItemRef) -> bool {
        let Some(slot) = self.by_item.remove(item) else {
            return false;
        };
        self.remove_postings(slot);
        self.slots[slot] = None;
        self.free.push(slot);
        true
    }

    fn remove_postings(&mut self, slot: usize) {
        if let Some(entry) = &self.slots[slot] {
            for &(term, _) in &entry.terms {
                self.postings[term as usize].remove(&(slot as u32));
            }
            self.total_len -= u64::from(entry.len);
        }
    }

    /// BM25-ranked full-text search.
    pub fn search_text(&self, query: &str, limit: usize) -> Vec<Hit> {
        let n = self.len() as f32;
        if n == 0.0 || limit == 0 {
            return Vec::new();
        }
        let avg_len = self.total_len as f32 / n;
        let mut scores: HashMap<u32, f32> = HashMap::new();
        let mut seen = Vec::new();
        for tok in tokenize(query) {
            let Some(&term) = self.term_ids.get(&tok) else {
                continue;
            };
            if seen.contains(&term) {
                continue;
            }
            seen.push(term);
            let posting = &self.postings[term as usize];
            let df = posting.len() as f32;
            if df == 0.0 {
                continue;
            }
            let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
            for (&slot, &tf) in posting {
                let Some(entry) = &self.slots[slot as usize] else {
                    continue;
                };
                let tf = tf as f32;
                let norm = K1 * (1.0 - B + B * entry.len as f32 / avg_len);
                *scores.entry(slot).or_insert(0.0) += idf * tf * (K1 + 1.0) / (tf + norm);
            }
        }
        self.top(scores.into_iter().map(|(s, v)| (s as usize, v)), limit)
    }

    /// Brute-force cosine kNN. `query` should be normalised (as [`FakeIndex::embed`] returns).
    pub fn search_vector(&self, query: &[f32], limit: usize) -> Vec<Hit> {
        if query.len() != self.dim || limit == 0 {
            return Vec::new();
        }
        let scored = self.slots.iter().enumerate().filter_map(|(slot, e)| {
            e.as_ref().map(|e| {
                (
                    slot,
                    e.vector.iter().zip(query).map(|(a, b)| a * b).sum::<f32>(),
                )
            })
        });
        self.top(scored, limit)
    }

    /// Reciprocal-rank fusion of text and vector results.
    pub fn search_hybrid(&self, query: &str, limit: usize) -> Vec<Hit> {
        let depth = limit.saturating_mul(4).max(limit);
        let text = self.search_text(query, depth);
        let vector = self.search_vector(&self.embed(query), depth);
        let mut fused: HashMap<usize, f32> = HashMap::new();
        for list in [&text, &vector] {
            for (rank, hit) in list.iter().enumerate() {
                if let Some(&slot) = self.by_item.get(&hit.item) {
                    *fused.entry(slot).or_insert(0.0) += 1.0 / (RRF_K + rank as f32 + 1.0);
                }
            }
        }
        self.top(fused.into_iter(), limit)
    }

    fn top(&self, scored: impl Iterator<Item = (usize, f32)>, limit: usize) -> Vec<Hit> {
        let mut v: Vec<(usize, f32)> = scored.collect();
        // Sort by score descending, then slot ascending for deterministic ties.
        let cmp = |a: &(usize, f32), b: &(usize, f32)| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0));
        if v.len() > limit {
            v.select_nth_unstable_by(limit - 1, cmp);
            v.truncate(limit);
        }
        v.sort_by(cmp);
        v.into_iter()
            .filter_map(|(slot, score)| {
                self.slots[slot].as_ref().map(|e| Hit {
                    item: e.item.clone(),
                    score,
                })
            })
            .collect()
    }

    /// Rough estimate of heap bytes held by the index (postings, vectors, term dictionary,
    /// item map). An estimate for trend tracking, not an allocator measurement.
    pub fn approx_heap_bytes(&self) -> usize {
        // Rough hashbrown cost per entry: key + value + control byte, at ~87% load.
        fn map_bytes(entries: usize, kv: usize) -> usize {
            (entries * (kv + 1)) * 8 / 7
        }
        let postings: usize = self
            .postings
            .iter()
            .map(|p| map_bytes(p.capacity(), 8) + std::mem::size_of::<HashMap<u32, u32>>())
            .sum();
        let entries: usize = self
            .slots
            .iter()
            .flatten()
            .map(|e| {
                std::mem::size_of::<Entry>()
                    + e.terms.capacity() * 8
                    + e.vector.capacity() * 4
                    + e.item.source.0.capacity()
                    + e.item.key.0.capacity()
            })
            .sum();
        let terms: usize = self
            .term_ids
            .keys()
            .map(|k| k.capacity() + std::mem::size_of::<String>() + 4)
            .sum();
        let items = map_bytes(self.by_item.capacity(), std::mem::size_of::<ItemRef>() + 8);
        postings
            + entries
            + terms
            + items
            + self.slots.capacity() * std::mem::size_of::<Option<Entry>>()
    }
}

fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::ids::{ItemKey, SourceId};

    fn item(k: &str) -> ItemRef {
        ItemRef {
            source: SourceId::new("bench"),
            key: ItemKey::new(k),
        }
    }

    #[test]
    fn upsert_outcomes_and_dedup() {
        let mut ix = FakeIndex::new(64);
        assert_eq!(ix.upsert(&item("a"), "alpha beta"), UpsertOutcome::Inserted);
        assert_eq!(
            ix.upsert(&item("a"), "alpha beta"),
            UpsertOutcome::Unchanged
        );
        assert_eq!(ix.upsert(&item("a"), "alpha gamma"), UpsertOutcome::Updated);
        assert_eq!(ix.len(), 1);
        // Old content no longer matches; new content does.
        assert!(ix.search_text("beta", 10).is_empty());
        assert_eq!(ix.search_text("gamma", 10)[0].item, item("a"));
    }

    #[test]
    fn delete_removes_from_all_indexes() {
        let mut ix = FakeIndex::new(64);
        ix.upsert(&item("a"), "alpha beta");
        ix.upsert(&item("b"), "gamma delta");
        assert!(ix.delete(&item("a")));
        assert!(!ix.delete(&item("a")));
        assert!(ix.search_text("alpha", 10).is_empty());
        let q = ix.embed("alpha beta");
        assert!(ix.search_vector(&q, 10).iter().all(|h| h.item != item("a")));
        assert!(
            ix.search_hybrid("alpha beta", 10)
                .iter()
                .all(|h| h.item != item("a"))
        );
        // Slot reuse keeps working.
        assert_eq!(ix.upsert(&item("c"), "epsilon"), UpsertOutcome::Inserted);
        assert_eq!(ix.len(), 2);
        assert_eq!(ix.search_text("epsilon", 1)[0].item, item("c"));
    }

    #[test]
    fn bm25_prefers_rarer_and_more_frequent_terms() {
        let mut ix = FakeIndex::new(64);
        ix.upsert(&item("common"), "the the the cat");
        ix.upsert(&item("rare"), "the zebra zebra");
        ix.upsert(&item("other"), "the dog");
        let hits = ix.search_text("zebra the", 3);
        assert_eq!(hits[0].item, item("rare"));
        assert!(hits.windows(2).all(|w| w[0].score >= w[1].score));
    }

    #[test]
    fn vector_and_hybrid_find_exact_text() {
        let mut ix = FakeIndex::new(256);
        for (k, t) in [
            ("a", "red green blue"),
            ("b", "north south east"),
            ("c", "one two three"),
        ] {
            ix.upsert(&item(k), t);
        }
        let q = ix.embed("north south east");
        let hits = ix.search_vector(&q, 1);
        assert_eq!(hits[0].item, item("b"));
        assert!((hits[0].score - 1.0).abs() < 1e-5);
        assert_eq!(ix.search_hybrid("one two", 2)[0].item, item("c"));
        assert!(
            ix.search_vector(&[1.0; 3], 1).is_empty(),
            "dimension mismatch yields nothing"
        );
    }

    #[test]
    fn heap_estimate_grows() {
        let mut ix = FakeIndex::new(32);
        let before = ix.approx_heap_bytes();
        for i in 0..100 {
            ix.upsert(&item(&i.to_string()), &format!("word{i} shared text"));
        }
        assert!(ix.approx_heap_bytes() > before);
    }
}
