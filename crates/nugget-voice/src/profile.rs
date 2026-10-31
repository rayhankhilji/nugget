//! The voice profile: the user's own messages from the store, their stats, and exemplars.
//!
//! # What is held
//! Per item the profile keeps its connector version (the incremental cursor) and, for the
//! user's own messages, a compact sample: channel, recipients, date, sensitivity, Message-ID,
//! the per-message [`Features`] the stats are made of, the message's content terms as ids into
//! one shared vocabulary (one `u32` per occurrence, sorted), and two fingerprints of its text.
//! The text itself is not held: it stays in the encrypted store and is read again for the few
//! exemplars a draft gets. That keeps a 100k-message profile in tens of MB instead of about a
//! GB of texts and per-message term maps, and makes the snapshot quick to load.

use std::cmp::Ordering;
use std::collections::hash_map::Entry as MapEntry;
use std::collections::{HashMap, HashSet};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use nugget_core::Result;
use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_core::ipc::VoiceStatus;
use nugget_store::{Item, Store};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::feedback::{FEEDBACK_KIND, FEEDBACK_SOURCE};
use crate::me::{MeIdentity, parse_address, recipient_first_names, recipient_key};
use crate::stats::{aggregate, distance_with};
use crate::text::{self, Features};
use crate::{Channel, StyleStats, VoiceHints, VoiceQuery, VoiceSource};

/// Cursor key under which the profile records its last build (`Store::get_cursor`).
pub const VOICE_CURSOR_SOURCE: &str = "nugget-voice:profile";
/// Cursor key under which [`VoiceProfile::save`] keeps the profile snapshot. The snapshot holds
/// what the profile learned from the user's own messages (features, recipients, the words they
/// use), so it lives only inside the encrypted store.
pub const VOICE_SNAPSHOT_SOURCE: &str = "nugget-voice:snapshot";
/// Snapshot format version; snapshots of another version are ignored (rebuilt). Version 1 held
/// every sample's text; version 2 holds term ids and text fingerprints instead.
const SNAPSHOT_VERSION: u32 = 2;

/// Folder names (last path segment, lowercase) that hold sent mail.
const SENT_FOLDERS: &[&str] = &[
    "sent",
    "sent items",
    "sent mail",
    "sent messages",
    "sent-mail",
    "sentmail",
    "gesendet",
    "gesendete elemente",
    "gesendete objekte",
    "envoyés",
    "éléments envoyés",
    "messages envoyés",
    "enviados",
    "elementos enviados",
    "posta inviata",
    "inviati",
    "verzonden",
    "verzonden items",
    "skickat",
    "skickade objekt",
    "sendt",
    "wysłane",
];
const DRAFT_FOLDERS: &[&str] = &["drafts", "draft", "entwürfe", "brouillons", "borradores"];

/// Most store hits considered when ranking exemplars by topic.
const SEARCH_DEPTH: usize = 200;
/// Most query terms for the full-text leg (the store caps at 32).
const QUERY_TERMS: usize = 24;
/// Exemplars this similar (term Jaccard) to one already chosen are duplicates.
const NEAR_DUPLICATE: f32 = 0.8;
/// Chunks read to rebuild text when an item does not keep its text.
const FALLBACK_CHUNKS: u32 = 8;
/// Items read from the store per query while refreshing (bounds what a refresh holds at once).
const FETCH_BATCH: usize = 256;
/// The vocabulary is rebuilt from the live samples when it holds more than twice their
/// distinct terms plus this many (terms of deleted or changed messages).
const VOCAB_SLACK: usize = 4_096;

/// Tuning for [`VoiceProfile`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceConfig {
    /// Messages a channel needs before it gets its own stats (and is listed in the status).
    pub min_channel_samples: u32,
    /// Messages to one recipient before they get their own stats.
    pub min_recipient_samples: u32,
    /// Exemplars returned at most...
    pub max_exemplars: usize,
    /// ...and at least, when that many exist (other channels fill up to this number).
    pub min_exemplars: usize,
    /// Longest exemplar, in characters.
    pub exemplar_chars: usize,
    /// Treat Markdown notes as the user's writing (channel `document`).
    pub include_notes: bool,
    /// Infer extra addresses and names from sent mail.
    pub infer_me: bool,
    /// Additional sent-folder names (matched case-insensitively against the whole path or its
    /// last segment).
    pub sent_folders: Vec<String>,
    /// Recency half-life for exemplar ranking, in days.
    pub recency_half_life_days: f32,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            min_channel_samples: 5,
            min_recipient_samples: 3,
            max_exemplars: 6,
            min_exemplars: 3,
            exemplar_chars: 600,
            include_notes: true,
            infer_me: true,
            sent_folders: Vec::new(),
            recency_half_life_days: 180.0,
        }
    }
}

/// What one [`VoiceProfile::refresh`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshReport {
    /// Items listed across all sources.
    pub scanned: u64,
    /// Items read because they were new or their version changed.
    pub fetched: u64,
    /// Items gone from the store since the last refresh.
    pub removed: u64,
    /// The user's messages in the profile after the refresh.
    pub samples: u64,
}

impl RefreshReport {
    /// Whether the refresh changed the profile (items were read or dropped), i.e. whether a
    /// saved snapshot is now out of date and [`VoiceProfile::save`] should be called.
    pub fn changed(&self) -> bool {
        self.fetched > 0 || self.removed > 0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum Origin {
    Mail {
        from: Option<String>,
        sent: bool,
        draft: bool,
    },
    Chat,
    Note,
    Feedback(Channel),
}

/// One of the user's messages, without its text (see the module docs).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Sample {
    channel: Channel,
    /// Recipient keys (lowercased addresses or handles).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    recipients: Vec<String>,
    #[serde(with = "time::serde::rfc3339::option", default)]
    at: Option<OffsetDateTime>,
    sensitive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message_id: Option<String>,
    features: Features,
    terms: Terms,
    /// [`fingerprint`] of the text: checks that a text read back for an exemplar is still
    /// the one this sample describes.
    text_hash: u64,
    /// [`fingerprint`] of the canonical text ([`text::canonical`]): exact-duplicate exemplars.
    canon_hash: u64,
    /// [`fingerprint`] of [`text::signoff_name_key`], or 0: when the user's names change, only
    /// samples whose key is (or was) one of the names get a new sign-off.
    #[serde(default, skip_serializing_if = "is_zero")]
    name_key: u64,
    /// A newer sample has the same Message-ID (set by `recompute`).
    #[serde(skip)]
    shadowed: bool,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    origin: Origin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sample: Option<Box<Sample>>,
}

/// Every item seen: its version (the incremental cursor) and, when it may be the user's
/// writing, its record.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Entry {
    /// [`fingerprint`] of the connector version (`None` for unversioned items).
    #[serde(rename = "v", default)]
    version: Option<u64>,
    #[serde(rename = "r", default, skip_serializing_if = "Option::is_none")]
    record: Option<Record>,
}

/// Content terms of all samples, interned. Ids are dense (`0..len`).
#[derive(Debug, Default)]
struct Vocab {
    ids: HashMap<Box<str>, u32>,
}

impl Vocab {
    fn len(&self) -> usize {
        self.ids.len()
    }

    fn get(&self, term: &str) -> Option<u32> {
        self.ids.get(term).copied()
    }

    fn intern(&mut self, term: String) -> u32 {
        let next = self.ids.len() as u32;
        *self.ids.entry(term.into_boxed_str()).or_insert(next)
    }

    /// The content terms of `text`.
    fn terms_of(&mut self, text: &str) -> Terms {
        let mut counts: Vec<(String, f32)> = text::terms(text).into_iter().collect();
        // Intern in a fixed order, so ids do not depend on hash order.
        counts.sort_by(|a, b| a.0.cmp(&b.0));
        let mut ids = Vec::with_capacity(counts.iter().map(|(_, c)| *c as usize).sum());
        for (term, count) in counts {
            let id = self.intern(term);
            ids.extend(std::iter::repeat_n(id, count as usize));
        }
        ids.sort_unstable();
        Terms::pack(&ids)
    }

    /// The vocabulary saved as `terms` (term `i` has id `i`); `None` when it has duplicates.
    fn from_terms(terms: Vec<String>) -> Option<Self> {
        let n = terms.len();
        let ids: HashMap<Box<str>, u32> = terms
            .into_iter()
            .enumerate()
            .map(|(i, t)| (t.into_boxed_str(), i as u32))
            .collect();
        (ids.len() == n).then_some(Self { ids })
    }

    /// Terms by id.
    fn terms(&self) -> Vec<&str> {
        let mut out = vec![""; self.ids.len()];
        for (t, &id) in &self.ids {
            out[id as usize] = &**t;
        }
        out
    }
}

/// A message's content terms ([`text::terms`]): vocabulary ids, sorted, one per occurrence (a
/// term's count is its run length), packed as LEB128 deltas (one or two bytes per occurrence).
/// Serialized as base64 of those bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Terms(Box<[u8]>);

impl Terms {
    /// Pack sorted ids.
    fn pack(ids: &[u32]) -> Self {
        let mut bytes = Vec::with_capacity(ids.len() + ids.len() / 2);
        let mut prev = 0u32;
        for &id in ids {
            debug_assert!(id >= prev, "term ids must be sorted");
            let mut d = id.wrapping_sub(prev);
            prev = id;
            loop {
                let b = (d & 0x7f) as u8;
                d >>= 7;
                if d == 0 {
                    bytes.push(b);
                    break;
                }
                bytes.push(b | 0x80);
            }
        }
        Self(bytes.into_boxed_slice())
    }

    fn ids(&self) -> TermIds<'_> {
        TermIds {
            bytes: &self.0,
            prev: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether the bytes are well-formed (every id complete, at most five bytes, no overflow)
    /// and every id is below `vocab`.
    fn is_valid(&self, vocab: usize) -> bool {
        let (mut prev, mut cur, mut shift) = (0u64, 0u64, 0u32);
        for &b in self.0.iter() {
            if shift > 28 {
                return false;
            }
            cur |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                prev += cur;
                if prev >= vocab as u64 {
                    return false;
                }
                cur = 0;
                shift = 0;
            } else {
                shift += 7;
            }
        }
        shift == 0
    }

    /// Euclidean norm of the term counts (as `text::cosine` computes it on a term map: counts
    /// are integers, so the sum does not depend on order).
    fn norm(&self) -> f32 {
        let mut sum = 0.0f32;
        let mut ids = self.ids();
        let Some(mut cur) = ids.next() else {
            return 0.0;
        };
        let mut count = 1u32;
        for id in ids {
            if id == cur {
                count += 1;
            } else {
                sum += count as f32 * count as f32;
                cur = id;
                count = 1;
            }
        }
        sum += count as f32 * count as f32;
        sum.sqrt()
    }
}

/// Iterator over packed term ids.
struct TermIds<'a> {
    bytes: &'a [u8],
    prev: u32,
}

impl Iterator for TermIds<'_> {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        let mut cur = 0u32;
        let mut shift = 0u32;
        loop {
            let (&b, rest) = self.bytes.split_first()?;
            self.bytes = rest;
            cur |= u32::from(b & 0x7f).checked_shl(shift).unwrap_or(0);
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        self.prev = self.prev.wrapping_add(cur);
        Some(self.prev)
    }
}

impl Serialize for Terms {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD_NO_PAD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Terms {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = Terms;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a base64 term list")
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> std::result::Result<Terms, E> {
                // At most two bytes more than needed: shrinking keeps the allocation in place.
                let mut bytes = vec![0u8; base64::decoded_len_estimate(s.len())];
                let n = STANDARD_NO_PAD
                    .decode_slice(s, &mut bytes)
                    .map_err(|e| E::custom(format!("invalid term list: {e}")))?;
                bytes.truncate(n);
                Ok(Terms(bytes.into_boxed_slice()))
            }
        }
        d.deserialize_str(V)
    }
}

/// Cosine between a sample's terms and a thread's: `weights[id]` is the thread's count of term
/// `id` (0 when absent), `thread_norm` the norm over all thread terms (also the ones the
/// vocabulary does not know). The same value as `text::cosine` on the term maps: counts are
/// integers, so the sums do not depend on order.
fn cosine_ids(terms: &Terms, weights: &[f32], thread_norm: f32) -> f32 {
    if thread_norm == 0.0 {
        return 0.0;
    }
    let dot: f32 = terms
        .ids()
        .map(|id| weights.get(id as usize).copied().unwrap_or(0.0))
        .sum();
    if dot == 0.0 {
        return 0.0;
    }
    let norm = terms.norm();
    if norm == 0.0 {
        0.0
    } else {
        dot / (norm * thread_norm)
    }
}

/// Jaccard similarity of the distinct terms of two term lists.
fn jaccard_ids(a: &Terms, b: &Terms) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut a: Vec<u32> = a.ids().collect();
    let mut b: Vec<u32> = b.ids().collect();
    a.dedup();
    b.dedup();
    let (mut i, mut j) = (0, 0);
    let mut inter = 0usize;
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Less => i += 1,
            Ordering::Greater => j += 1,
            Ordering::Equal => {
                inter += 1;
                i += 1;
                j += 1;
            }
        }
    }
    let inter = inter as f32;
    inter / (a.len() as f32 + b.len() as f32 - inter)
}

/// A stable 64-bit fingerprint (FNV-1a) of a text.
fn fingerprint(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A sample with where it came from.
#[derive(Clone, Copy)]
struct Own<'a> {
    source: &'a SourceId,
    key: &'a ItemKey,
    origin: &'a Origin,
    sample: &'a Sample,
}

/// The user's voice: their own messages from the store, stats per channel and recipient, and
/// exemplar selection. Build with [`VoiceProfile::build`], keep it current with
/// [`VoiceProfile::refresh`] (incremental), share it as a [`VoiceSource`] (e.g. behind an
/// `RwLock`).
pub struct VoiceProfile {
    store: Store,
    config: VoiceConfig,
    configured: MeIdentity,
    me: MeIdentity,
    /// Every item seen, per source.
    items: HashMap<SourceId, HashMap<ItemKey, Entry>>,
    vocab: Vocab,
    overall: StyleStats,
    channels: HashMap<Channel, StyleStats>,
    recipients: HashMap<String, StyleStats>,
    samples: u64,
    last_built: Option<OffsetDateTime>,
    last_report: RefreshReport,
}

/// What [`VoiceProfile::save`] writes (borrowed).
#[derive(Serialize)]
struct SnapshotRef<'a> {
    v: u32,
    configured: &'a MeIdentity,
    config: &'a VoiceConfig,
    me: &'a MeIdentity,
    #[serde(with = "time::serde::rfc3339::option")]
    last_built: Option<OffsetDateTime>,
    /// The vocabulary: term `i` has id `i`.
    terms: Vec<&'a str>,
    sources: Vec<SourceRef<'a>>,
}

#[derive(Serialize)]
struct SourceRef<'a> {
    source: &'a SourceId,
    items: Vec<(&'a ItemKey, &'a Entry)>,
}

/// What [`VoiceProfile::load`] reads.
#[derive(Deserialize)]
struct Snapshot {
    configured: MeIdentity,
    config: VoiceConfig,
    me: MeIdentity,
    #[serde(with = "time::serde::rfc3339::option", default)]
    last_built: Option<OffsetDateTime>,
    terms: Vec<String>,
    sources: Vec<SourceSnapshot>,
}

#[derive(Deserialize)]
struct SourceSnapshot {
    source: SourceId,
    items: Vec<(ItemKey, Entry)>,
}

/// Just the version of a snapshot (other fields are skipped).
#[derive(Deserialize)]
struct SnapshotHeader {
    #[serde(default)]
    v: Option<u64>,
}

impl std::fmt::Debug for VoiceProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceProfile")
            .field("me", &self.me)
            .field("samples", &self.samples)
            .field("last_built", &self.last_built)
            .finish_non_exhaustive()
    }
}

impl VoiceProfile {
    fn empty(store: &Store, me: MeIdentity, config: VoiceConfig) -> Self {
        Self {
            store: store.clone(),
            config,
            me: me.clone(),
            configured: me,
            items: HashMap::new(),
            vocab: Vocab::default(),
            overall: StyleStats::default(),
            channels: HashMap::new(),
            recipients: HashMap::new(),
            samples: 0,
            last_built: None,
            last_report: RefreshReport::default(),
        }
    }

    /// Build the profile from every item in `store`. `me` is the configured identity; with
    /// [`VoiceConfig::infer_me`] it is completed from sent mail.
    pub fn build(store: &Store, me: MeIdentity, config: VoiceConfig) -> Result<Self> {
        let mut p = Self::empty(store, me, config);
        p.refresh()?;
        Ok(p)
    }

    /// Write a snapshot of the profile (identity, configuration, the incremental cursor and
    /// every record with its sample, i.e. features, recipients and term ids; not the texts) into
    /// the encrypted store, under the cursor key [`VOICE_SNAPSHOT_SOURCE`]. It is never written
    /// to a plain file.
    ///
    /// [`VoiceProfile::refresh`] does not save: call this after a refresh whose
    /// [`RefreshReport::changed`] is true (it takes `&self`, so a read lock is enough).
    pub fn save(&self) -> Result<()> {
        let mut sources: Vec<SourceRef<'_>> = self
            .items
            .iter()
            .map(|(source, entries)| {
                let mut items: Vec<(&ItemKey, &Entry)> = entries.iter().collect();
                items.sort_by(|a, b| a.0.cmp(b.0));
                SourceRef { source, items }
            })
            .collect();
        sources.sort_by(|a, b| a.source.cmp(b.source));
        let snap = SnapshotRef {
            v: SNAPSHOT_VERSION,
            configured: &self.configured,
            config: &self.config,
            me: &self.me,
            last_built: self.last_built,
            terms: self.vocab.terms(),
            sources,
        };
        self.store
            .set_cursor_from(&SourceId::new(VOICE_SNAPSHOT_SOURCE), &snap)
    }

    /// Load the profile saved by [`VoiceProfile::save`], as it was then (call
    /// [`VoiceProfile::refresh`] to catch up with the store). `None` when there is no snapshot,
    /// or it is unreadable or stale: another snapshot version, or saved for a different
    /// configured identity `me` or `config`.
    pub fn load(store: &Store, me: MeIdentity, config: VoiceConfig) -> Result<Option<Self>> {
        let Some(raw) = store.get_cursor_json(&SourceId::new(VOICE_SNAPSHOT_SOURCE))? else {
            return Ok(None);
        };
        match serde_json::from_str::<SnapshotHeader>(&raw) {
            Ok(h) if h.v == Some(u64::from(SNAPSHOT_VERSION)) => {}
            Ok(_) => {
                tracing::info!("voice: profile snapshot has another version; rebuilding");
                return Ok(None);
            }
            Err(e) => {
                tracing::warn!(error = %e, "voice: unreadable profile snapshot; rebuilding");
                return Ok(None);
            }
        }
        let snap: Snapshot = match serde_json::from_str(&raw) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "voice: unreadable profile snapshot; rebuilding");
                return Ok(None);
            }
        };
        drop(raw);
        if snap.configured != me || snap.config != config {
            tracing::info!("voice: identity or settings changed since the snapshot; rebuilding");
            return Ok(None);
        }
        let Some(vocab) = Vocab::from_terms(snap.terms) else {
            tracing::warn!("voice: profile snapshot has a broken vocabulary; rebuilding");
            return Ok(None);
        };
        let mut items: HashMap<SourceId, HashMap<ItemKey, Entry>> =
            HashMap::with_capacity(snap.sources.len());
        for s in snap.sources {
            let mut entries = HashMap::with_capacity(s.items.len());
            for (key, entry) in s.items {
                let sample = entry.record.as_ref().and_then(|r| r.sample.as_ref());
                if sample.is_some_and(|s| !s.terms.is_valid(vocab.len())) {
                    tracing::warn!("voice: profile snapshot has unknown term ids; rebuilding");
                    return Ok(None);
                }
                entries.insert(key, entry);
            }
            items.insert(s.source, entries);
        }
        let mut p = Self {
            items,
            vocab,
            me: snap.me,
            last_built: snap.last_built,
            ..Self::empty(store, snap.configured, snap.config)
        };
        p.recompute();
        Ok(Some(p))
    }

    /// The saved profile brought up to date with [`VoiceProfile::refresh`] (only new or changed
    /// items are read), else a full [`VoiceProfile::build`]. Saves the snapshot when anything
    /// changed. [`VoiceProfile::last_refresh`] tells what was read.
    pub fn load_or_build(store: &Store, me: MeIdentity, config: VoiceConfig) -> Result<Self> {
        if let Some(mut p) = Self::load(store, me.clone(), config.clone())? {
            let report = p.refresh()?;
            if report.changed() {
                p.save()?;
            }
            return Ok(p);
        }
        let p = Self::build(store, me, config)?;
        p.save()?;
        Ok(p)
    }

    /// What the last [`VoiceProfile::refresh`] (or build) did; all zero after a bare
    /// [`VoiceProfile::load`].
    pub fn last_refresh(&self) -> RefreshReport {
        self.last_report
    }

    /// Bring the profile up to date: read only items that are new or whose connector version
    /// changed, drop deleted ones, re-infer "me", and recompute the stats.
    pub fn refresh(&mut self) -> Result<RefreshReport> {
        let mut report = RefreshReport::default();
        let sources: Vec<SourceId> = self
            .store
            .source_counts()?
            .into_iter()
            .map(|c| c.source)
            .collect();
        let live: HashSet<&SourceId> = sources.iter().collect();
        self.items.retain(|s, entries| {
            let keep = live.contains(s);
            if !keep {
                report.removed += entries.len() as u64;
            }
            keep
        });

        let my_names = self.me.name_variants();
        for source in &sources {
            let versions = self.store.item_versions(source)?;
            report.scanned += versions.len() as u64;
            let entries = self.items.entry(source.clone()).or_default();
            let before = entries.len();
            entries.retain(|k, _| versions.contains_key(k));
            report.removed += (before - entries.len()) as u64;
            let mut fetch: Vec<ItemRef> = Vec::new();
            for (key, version) in versions {
                let version = version.as_deref().map(fingerprint);
                match entries.entry(key) {
                    MapEntry::Occupied(mut e) => {
                        // Unversioned items are read once.
                        if version.is_none() || e.get().version == version {
                            continue;
                        }
                        e.get_mut().version = version;
                        fetch.push(ItemRef {
                            source: source.clone(),
                            key: e.key().clone(),
                        });
                    }
                    MapEntry::Vacant(e) => {
                        fetch.push(ItemRef {
                            source: source.clone(),
                            key: e.key().clone(),
                        });
                        e.insert(Entry {
                            version,
                            record: None,
                        });
                    }
                }
            }
            report.fetched += fetch.len() as u64;
            for batch in fetch.chunks(FETCH_BATCH) {
                let items = self.store.get_items(batch)?;
                for (r, item) in batch.iter().zip(items) {
                    let record = match item {
                        Some(item) => match classify(&item, &self.config) {
                            Some(origin) => {
                                let sample = if is_own(&origin, &self.me) {
                                    make_sample(
                                        &self.store,
                                        &item,
                                        &origin,
                                        &my_names,
                                        &mut self.vocab,
                                    )?
                                } else {
                                    None
                                };
                                Some(Record {
                                    origin,
                                    sample: sample.map(Box::new),
                                })
                            }
                            None => None,
                        },
                        None => None,
                    };
                    if let Some(e) = self.entry_mut(r) {
                        e.record = record;
                    }
                }
            }
        }

        // Re-infer "me", then make sure every own record has a sample (and only those).
        let me =
            if self.config.infer_me {
                let inferred =
                    MeIdentity::infer(self.items.values().flat_map(HashMap::values).filter_map(
                        |e| match e.record.as_ref().map(|r| &r.origin) {
                            Some(Origin::Mail {
                                from: Some(f),
                                sent: true,
                                draft: false,
                            }) => Some(f.as_str()),
                            _ => None,
                        },
                    ));
                self.configured.merged(&inferred)
            } else {
                self.configured.clone()
            };
        let names_changed = me.names != self.me.names || me.emails != self.me.emails;
        let old_names: HashSet<u64> = my_names.iter().map(|n| fingerprint(n)).collect();
        self.me = me;
        let my_names = self.me.name_variants();
        let new_names: HashSet<u64> = my_names.iter().map(|n| fingerprint(n)).collect();
        // Samples to (re)make from the store: records that became the user's, and, when the
        // user's names changed, those whose sign-off may now read differently (their last line
        // is one of the old or the new names but not both).
        let resign = |s: &Sample| {
            names_changed
                && s.name_key != 0
                && old_names.contains(&s.name_key) != new_names.contains(&s.name_key)
        };
        let mut remake: Vec<ItemRef> = Vec::new();
        for (source, entries) in &mut self.items {
            for (key, e) in entries.iter_mut() {
                let Some(rec) = e.record.as_mut() else {
                    continue;
                };
                let own = is_own(&rec.origin, &self.me);
                match (own, rec.sample.as_deref()) {
                    (true, None) => {}
                    (false, Some(_)) => {
                        rec.sample = None;
                        continue;
                    }
                    (true, Some(s)) if resign(s) => {}
                    _ => continue,
                }
                if own {
                    remake.push(ItemRef {
                        source: source.clone(),
                        key: key.clone(),
                    });
                }
            }
        }
        for batch in remake.chunks(FETCH_BATCH) {
            let items = self.store.get_items(batch)?;
            for (r, item) in batch.iter().zip(items) {
                let Some(origin) = self
                    .entry_mut(r)
                    .and_then(|e| e.record.as_ref())
                    .map(|rec| rec.origin.clone())
                else {
                    continue;
                };
                let sample = match item {
                    Some(item) => {
                        make_sample(&self.store, &item, &origin, &my_names, &mut self.vocab)?
                    }
                    None => None,
                };
                if let Some(rec) = self.entry_mut(r).and_then(|e| e.record.as_mut()) {
                    rec.sample = sample.map(Box::new);
                }
            }
        }

        if report.changed() || !remake.is_empty() {
            self.compact_vocab();
        }
        self.recompute();
        report.samples = self.samples;
        self.last_report = report;
        let now = OffsetDateTime::now_utc();
        self.last_built = Some(now);
        self.store.set_cursor(
            &SourceId::new(VOICE_CURSOR_SOURCE),
            json!({
                "v": 1,
                "samples": self.samples,
                "items_seen": self.items.values().map(HashMap::len).sum::<usize>(),
                "last_built": now.format(&Rfc3339).ok(),
            }),
        )?;
        Ok(report)
    }

    fn entry_mut(&mut self, r: &ItemRef) -> Option<&mut Entry> {
        self.items.get_mut(&r.source)?.get_mut(&r.key)
    }

    /// Every sample, in no particular order.
    fn all_samples(&self) -> impl Iterator<Item = Own<'_>> + '_ {
        self.items.iter().flat_map(|(source, entries)| {
            entries.iter().filter_map(move |(key, e)| {
                let r = e.record.as_ref()?;
                Some(Own {
                    source,
                    key,
                    origin: &r.origin,
                    sample: r.sample.as_deref()?,
                })
            })
        })
    }

    fn samples_mut(&mut self) -> impl Iterator<Item = &mut Sample> + '_ {
        self.items
            .values_mut()
            .flat_map(HashMap::values_mut)
            .filter_map(|e| e.record.as_mut()?.sample.as_deref_mut())
    }

    /// Own samples newest first, split into one per message (the newest of those sharing a
    /// `Message-ID` across sources) and the duplicates.
    fn own_samples(&self) -> (Vec<Own<'_>>, Vec<Own<'_>>) {
        let mut all: Vec<Own<'_>> = self.all_samples().collect();
        all.sort_by(|a, b| {
            b.sample
                .at
                .cmp(&a.sample.at)
                .then_with(|| a.source.cmp(b.source))
                .then_with(|| a.key.cmp(b.key))
        });
        let mut ids = HashSet::new();
        let mut dups = Vec::new();
        all.retain(|o| match &o.sample.message_id {
            Some(id) if !ids.insert(id.as_str()) => {
                dups.push(*o);
                false
            }
            _ => true,
        });
        (all, dups)
    }

    /// Rebuild the vocabulary from the live samples when it mostly holds terms of messages
    /// that are gone or changed. Ids keep their order, so term lists stay sorted.
    fn compact_vocab(&mut self) {
        let n = self.vocab.len();
        let mut used = vec![false; n];
        for o in self.all_samples() {
            for id in o.sample.terms.ids() {
                if let Some(u) = used.get_mut(id as usize) {
                    *u = true;
                }
            }
        }
        let live = used.iter().filter(|u| **u).count();
        if n <= 2 * live + VOCAB_SLACK {
            return;
        }
        let mut remap = vec![u32::MAX; n];
        let mut next = 0u32;
        for (old, u) in used.iter().enumerate() {
            if *u {
                remap[old] = next;
                next += 1;
            }
        }
        self.vocab.ids.retain(|_, id| {
            let new = remap[*id as usize];
            *id = new;
            new != u32::MAX
        });
        self.vocab.ids.shrink_to_fit();
        for s in self.samples_mut() {
            let ids: Vec<u32> = s.terms.ids().map(|id| remap[id as usize]).collect();
            s.terms = Terms::pack(&ids);
        }
    }

    fn recompute(&mut self) {
        let (overall, channels, recipients, n, dups) = {
            let (samples, dups) = self.own_samples();
            let messages: Vec<&Features> = samples
                .iter()
                .filter(|o| o.sample.channel != Channel::Document)
                .map(|o| &o.sample.features)
                .collect();
            let overall = if messages.is_empty() {
                aggregate(samples.iter().map(|o| &o.sample.features))
            } else {
                aggregate(messages)
            };
            let mut by_channel: HashMap<Channel, Vec<&Features>> = HashMap::new();
            let mut by_recipient: HashMap<String, Vec<&Features>> = HashMap::new();
            for o in &samples {
                let s = o.sample;
                by_channel.entry(s.channel).or_default().push(&s.features);
                for r in &s.recipients {
                    by_recipient.entry(r.clone()).or_default().push(&s.features);
                }
            }
            let channels: HashMap<Channel, StyleStats> = by_channel
                .into_iter()
                .filter(|(_, f)| f.len() as u32 >= self.config.min_channel_samples)
                .map(|(c, f)| (c, aggregate(f)))
                .collect();
            let recipients: HashMap<String, StyleStats> = by_recipient
                .into_iter()
                .filter(|(_, f)| f.len() as u32 >= self.config.min_recipient_samples)
                .map(|(r, f)| (r, aggregate(f)))
                .collect();
            let dups: Vec<ItemRef> = dups
                .iter()
                .map(|o| ItemRef {
                    source: o.source.clone(),
                    key: o.key.clone(),
                })
                .collect();
            (overall, channels, recipients, samples.len() as u64, dups)
        };
        for s in self.samples_mut() {
            s.shadowed = false;
        }
        for r in &dups {
            if let Some(s) = self
                .entry_mut(r)
                .and_then(|e| e.record.as_mut())
                .and_then(|rec| rec.sample.as_deref_mut())
            {
                s.shadowed = true;
            }
        }
        self.overall = overall;
        self.channels = channels;
        self.recipients = recipients;
        self.samples = n;
    }

    /// Rough heap bytes the profile holds (items, samples, vocabulary, stats), from lengths
    /// and capacities; allocator overhead is not counted.
    pub fn approx_heap_bytes(&self) -> usize {
        fn string(s: &str) -> usize {
            s.len()
        }
        fn opt(s: &Option<String>) -> usize {
            s.as_deref().map_or(0, string)
        }
        let mut bytes = 0usize;
        for (source, entries) in &self.items {
            bytes += string(source.as_str())
                + entries.capacity()
                    * (std::mem::size_of::<ItemKey>() + std::mem::size_of::<Entry>() + 1);
            for (key, e) in entries {
                bytes += string(key.as_str());
                let Some(rec) = &e.record else { continue };
                if let Origin::Mail { from, .. } = &rec.origin {
                    bytes += opt(from);
                }
                let Some(s) = &rec.sample else { continue };
                bytes += std::mem::size_of::<Sample>()
                    + s.recipients.iter().map(|r| 24 + r.len()).sum::<usize>()
                    + opt(&s.message_id)
                    + opt(&s.features.greeting)
                    + opt(&s.features.signoff)
                    + s.terms.0.len();
            }
        }
        bytes += self.vocab.ids.capacity() * (std::mem::size_of::<(Box<str>, u32)>() + 1)
            + self.vocab.ids.keys().map(|t| t.len()).sum::<usize>();
        bytes += self.recipients.capacity() * std::mem::size_of::<(String, StyleStats)>();
        bytes
    }

    /// The effective identity (configured plus inferred).
    pub fn me(&self) -> &MeIdentity {
        &self.me
    }

    pub fn config(&self) -> &VoiceConfig {
        &self.config
    }

    /// Stats over all of the user's messages (notes only when there are no messages).
    pub fn overall_stats(&self) -> &StyleStats {
        &self.overall
    }

    /// Stats for a channel with at least [`VoiceConfig::min_channel_samples`] messages.
    pub fn channel_stats(&self, channel: Channel) -> Option<&StyleStats> {
        self.channels.get(&channel)
    }

    /// Stats for a recipient (address or handle, any case) with at least
    /// [`VoiceConfig::min_recipient_samples`] messages.
    pub fn recipient_stats(&self, recipient: &str) -> Option<&StyleStats> {
        recipient_key(recipient).and_then(|k| self.recipients.get(&k))
    }

    /// The most specific stats for a draft: the first recipient with their own stats, else the
    /// channel's, else overall.
    pub fn stats_for(&self, channel: Channel, recipients: &[String]) -> &StyleStats {
        recipients
            .iter()
            .find_map(|r| self.recipient_stats(r))
            .or_else(|| self.channel_stats(channel))
            .unwrap_or(&self.overall)
    }

    /// Hints for a draft that may go to a cloud model (sensitive exemplars excluded). Use
    /// [`VoiceSource::hints`] with a [`VoiceQuery`] for full control.
    pub fn hints(&self, channel: Channel, recipients: &[String], thread_text: &str) -> VoiceHints {
        VoiceSource::hints(
            self,
            &VoiceQuery::new(channel)
                .with_recipients(recipients.iter().cloned())
                .with_thread(thread_text),
        )
    }

    pub fn status(&self) -> VoiceStatus {
        let mut channels: Vec<String> = self
            .channels
            .keys()
            .map(|c| c.as_str().to_owned())
            .collect();
        channels.sort();
        VoiceStatus {
            samples: self.samples,
            channels,
            last_built: self.last_built,
        }
    }

    /// `1 − style distance` of `text` to the overall style, in `[0, 1]`.
    pub fn similarity(&self, text: &str) -> f32 {
        1.0 - distance_with(&self.overall, text, &[], &self.me.name_variants())
    }

    /// `1 − style distance` of `text` to the most specific stats for `channel` and
    /// `recipients` (see [`VoiceProfile::stats_for`]).
    pub fn similarity_for(&self, channel: Channel, recipients: &[String], text: &str) -> f32 {
        let stats = self.stats_for(channel, recipients);
        1.0 - distance_with(
            stats,
            text,
            &recipient_first_names(recipients),
            &self.me.name_variants(),
        )
    }

    /// Whether a sample may be an exemplar for `query`.
    fn is_candidate(s: &Sample, query: &VoiceQuery) -> bool {
        !s.shadowed
            && !(query.exclude_sensitive && s.sensitive)
            // Notes are exemplars only for documents.
            && (s.channel == Channel::Document) == (query.channel == Channel::Document)
    }

    /// The text of a sample, read again from the store. `None` (with a debug log) when the
    /// item is gone or no longer has the text the profile learned from (it changed since the
    /// last refresh, which will pick it up).
    fn read_text(&self, o: Own<'_>) -> Option<String> {
        let r = ItemRef {
            source: o.source.clone(),
            key: o.key.clone(),
        };
        let item = match self.store.get_item(&r) {
            Ok(Some(item)) => item,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!(error = %e, "voice: cannot read an exemplar");
                return None;
            }
        };
        match extract(&self.store, &item, o.origin) {
            Ok(Some(x)) if fingerprint(&x.text) == o.sample.text_hash => Some(x.text),
            Ok(_) => {
                tracing::debug!("voice: exemplar changed since the last refresh; skipped");
                None
            }
            Err(e) => {
                tracing::warn!(error = %e, "voice: cannot read an exemplar");
                None
            }
        }
    }

    /// Exemplars for `query`: see [`VoiceSource::hints`].
    pub fn exemplars(&self, query: &VoiceQuery) -> Vec<String> {
        let want: HashSet<String> = query
            .recipients
            .iter()
            .filter_map(|r| recipient_key(r))
            .collect();
        let candidates: Vec<Own<'_>> = self
            .all_samples()
            .filter(|o| Self::is_candidate(o.sample, query))
            .collect();
        if candidates.is_empty() {
            return Vec::new();
        }

        // Topic: rank among the user's own items in a hybrid store search, plus a local term
        // cosine against the thread.
        let thread_terms = text::terms(&query.thread_text);
        let thread_norm: f32 = thread_terms.values().map(|v| v * v).sum::<f32>().sqrt();
        // Thread counts by term id (all zero when the vocabulary knows none of its terms).
        let mut weights: Vec<f32> = Vec::new();
        for (t, w) in &thread_terms {
            if let Some(id) = self.vocab.get(t) {
                if weights.is_empty() {
                    weights = vec![0.0; self.vocab.len()];
                }
                weights[id as usize] = *w;
            }
        }
        let mut store_rank: HashMap<(&SourceId, &ItemKey), f32> = HashMap::new();
        let q = text::query_terms(&query.thread_text, QUERY_TERMS).join(" ");
        if !q.is_empty() || query.thread_embedding.is_some() {
            match self.store.search_hybrid_with_depth(
                &q,
                query.thread_embedding.as_deref(),
                SEARCH_DEPTH,
                SEARCH_DEPTH,
            ) {
                Ok(hits) => {
                    let mut pos = 0usize;
                    for h in hits {
                        let Some((source, entries)) = self.items.get_key_value(&h.item.source)
                        else {
                            continue;
                        };
                        let Some((key, e)) = entries.get_key_value(&h.item.key) else {
                            continue;
                        };
                        let Some(s) = e.record.as_ref().and_then(|r| r.sample.as_deref()) else {
                            continue;
                        };
                        if !Self::is_candidate(s, query) {
                            continue;
                        }
                        if let MapEntry::Vacant(v) = store_rank.entry((source, key)) {
                            v.insert(1.0 / (1.0 + pos as f32 * 0.25));
                            pos += 1;
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, "voice: exemplar search failed"),
            }
        }
        let newest = candidates.iter().filter_map(|o| o.sample.at).max();
        let half_life = self.config.recency_half_life_days.max(1.0);
        let mut scored: Vec<(u8, f32, Own<'_>)> = candidates
            .into_iter()
            .map(|o| {
                let s = o.sample;
                let tier = if s.recipients.iter().any(|r| want.contains(r)) {
                    3
                } else if s.channel == query.channel {
                    2
                } else {
                    1
                };
                let topic = 0.5 * store_rank.get(&(o.source, o.key)).copied().unwrap_or(0.0)
                    + 0.5 * cosine_ids(&s.terms, &weights, thread_norm);
                let recency = match (newest, s.at) {
                    (Some(n), Some(at)) => {
                        let days = (n - at).as_seconds_f32() / 86_400.0;
                        0.5f32.powf(days.max(0.0) / half_life)
                    }
                    _ => 0.5,
                };
                (tier, 0.65 * topic + 0.35 * recency, o)
            })
            .collect();
        // Ties: newest first, then by source and key (the order own samples are listed in).
        scored.sort_unstable_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal))
                .then_with(|| b.2.sample.at.cmp(&a.2.sample.at))
                .then_with(|| a.2.source.cmp(b.2.source))
                .then_with(|| a.2.key.cmp(b.2.key))
        });

        let max = self.config.max_exemplars.max(1);
        let min = self.config.min_exemplars.min(max);
        let mut chosen: Vec<(&Sample, String)> = Vec::new();
        let mut seen_text: HashSet<u64> = HashSet::new();
        for (tier, _, o) in scored {
            if chosen.len() >= max || (tier == 1 && chosen.len() >= min) {
                break;
            }
            if !seen_text.insert(o.sample.canon_hash) {
                continue;
            }
            if chosen
                .iter()
                .any(|(c, _)| jaccard_ids(&c.terms, &o.sample.terms) >= NEAR_DUPLICATE)
            {
                continue;
            }
            if let Some(text) = self.read_text(o) {
                chosen.push((o.sample, text));
            }
        }
        chosen
            .into_iter()
            .map(|(_, t)| text::truncate(&t, self.config.exemplar_chars))
            .collect()
    }
}

impl VoiceSource for VoiceProfile {
    /// Stats from [`VoiceProfile::stats_for`] and up to [`VoiceConfig::max_exemplars`]
    /// exemplars (each at most [`VoiceConfig::exemplar_chars`]), ordered by: messages to one of
    /// the recipients, then messages in the same channel, then (only to reach
    /// [`VoiceConfig::min_exemplars`]) other channels; within each group by topic similarity to
    /// the thread (hybrid search over the user's own items plus term cosine) blended with
    /// recency. Exact and near duplicates are skipped. Sensitive items are left out when
    /// [`VoiceQuery::exclude_sensitive`] is set. Exemplar texts are read from the store.
    fn hints(&self, query: &VoiceQuery) -> VoiceHints {
        VoiceHints {
            channel: Some(query.channel),
            stats: self.stats_for(query.channel, &query.recipients).clone(),
            exemplars: self.exemplars(query),
        }
    }

    fn status(&self) -> VoiceStatus {
        VoiceProfile::status(self)
    }
}

fn str_at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter().try_fold(v, |v, k| v.get(k))?.as_str()
}

fn strings_at(v: &Value, path: &[&str]) -> Vec<String> {
    path.iter()
        .try_fold(v, |v, k| v.get(k))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn bool_at(v: &Value, path: &[&str]) -> Option<bool> {
    path.iter().try_fold(v, |v, k| v.get(k))?.as_bool()
}

fn folder_matches(folder: &str, names: &[&str], extra: &[String]) -> bool {
    let lower = folder.trim().to_lowercase();
    let last = lower
        .rsplit(['/', '.', '\\'])
        .next()
        .unwrap_or(&lower)
        .trim();
    names.contains(&last)
        || names.contains(&lower.as_str())
        || extra.iter().any(|e| {
            let e = e.trim().to_lowercase();
            e == lower || e == last
        })
}

/// What kind of the user's writing an item could be; `None` for everything else.
fn classify(item: &Item, config: &VoiceConfig) -> Option<Origin> {
    let m = &item.metadata;
    if bool_at(m, &["junk"]) == Some(true) {
        return None;
    }
    if item.item.source.as_str() == FEEDBACK_SOURCE && str_at(m, &["kind"]) == Some(FEEDBACK_KIND) {
        let channel = str_at(m, &["channel"])
            .and_then(Channel::parse)
            .unwrap_or(Channel::Other);
        return Some(Origin::Feedback(channel));
    }
    let mime = str_at(m, &["mime"]).or(item.mime.as_deref()).unwrap_or("");
    let is_mail = mime.eq_ignore_ascii_case("message/rfc822")
        || (m.pointer("/extracted/from").is_some() && m.pointer("/extracted/to").is_some());
    if is_mail {
        let labels = strings_at(m, &["source", "labels"]);
        let tags = strings_at(m, &["tags"]);
        let flags = strings_at(m, &["source", "flags"]);
        let folder = str_at(m, &["source", "folder"]).unwrap_or("");
        let has = |list: &[String], v: &str| list.iter().any(|x| x.eq_ignore_ascii_case(v));
        let sent = has(&labels, "SENT")
            || has(&tags, "sent")
            || has(&tags, "sent mail")
            || (!folder.is_empty() && folder_matches(folder, SENT_FOLDERS, &config.sent_folders));
        let draft = has(&labels, "DRAFT")
            || has(&tags, "draft")
            || has(&tags, "drafts")
            || has(&flags, "\\Draft")
            || (!folder.is_empty() && folder_matches(folder, DRAFT_FOLDERS, &[]));
        let from = str_at(m, &["extracted", "from"])
            .or_else(|| str_at(m, &["source", "from"]))
            .map(str::to_owned);
        return Some(Origin::Mail { from, sent, draft });
    }
    let from_me = bool_at(m, &["source", "from_me"]).or_else(|| bool_at(m, &["from_me"]));
    if let Some(from_me) = from_me {
        return from_me.then_some(Origin::Chat);
    }
    let markdown =
        mime.eq_ignore_ascii_case("text/markdown") || mime.eq_ignore_ascii_case("text/x-markdown");
    (config.include_notes && markdown).then_some(Origin::Note)
}

fn is_own(origin: &Origin, me: &MeIdentity) -> bool {
    match origin {
        Origin::Mail { draft: true, .. } => false,
        Origin::Mail { sent: true, .. } => true,
        Origin::Mail { from, .. } => from
            .as_deref()
            .and_then(|f| parse_address(f).1)
            .is_some_and(|e| me.has_email(&e)),
        Origin::Chat | Origin::Note | Origin::Feedback(_) => true,
    }
}

fn is_sensitive(m: &Value) -> bool {
    bool_at(m, &["sensitive"]) == Some(true)
        || bool_at(m, &["triage", "sensitive"]) == Some(true)
        || m.get("pii")
            .and_then(Value::as_object)
            .is_some_and(|o| !o.is_empty())
}

fn item_text(store: &Store, item: &Item) -> Result<String> {
    let text = match &item.text {
        Some(t) => t.clone(),
        None => {
            let ords: Vec<u32> = (0..FALLBACK_CHUNKS).collect();
            let chunks = store.get_chunks_by_ord(&item.item, &ords)?;
            chunks
                .into_iter()
                .map(|c| c.text)
                .collect::<Vec<_>>()
                .join("\n\n")
        }
    };
    Ok(without_text_header(&item.metadata, text))
}

/// The message without the `From:` / `Date:` lines extraction puts before a mail's body
/// (`metadata.extracted.text_header`): they are not the user's writing.
fn without_text_header(meta: &Value, text: String) -> String {
    match meta
        .get("extracted")
        .and_then(|e| e.get("text_header"))
        .and_then(Value::as_str)
        .filter(|h| !h.is_empty())
    {
        Some(h) if text.starts_with(h) => text[h.len()..].to_owned(),
        _ => text,
    }
}

/// The parts of one of the user's messages a sample is made from.
struct Extracted {
    channel: Channel,
    recipients: Vec<String>,
    message_id: Option<String>,
    at: Option<OffsetDateTime>,
    /// The message text without quotes and signatures; never blank.
    text: String,
}

/// The user's message in `item` (of kind `origin`); `None` when its text is blank.
fn extract(store: &Store, item: &Item, origin: &Origin) -> Result<Option<Extracted>> {
    let m = &item.metadata;
    let raw = item_text(store, item)?;
    let (channel, recipients, message_id, at, text) = match origin {
        Origin::Mail { .. } => {
            let mut to = strings_at(m, &["extracted", "to"]);
            to.extend(strings_at(m, &["extracted", "cc"]));
            let at = str_at(m, &["extracted", "date"])
                .and_then(|d| OffsetDateTime::parse(d, &Rfc3339).ok());
            let text = text::clean_body(&raw, str_at(m, &["extracted", "signature"]));
            (
                Channel::Email,
                to,
                str_at(m, &["extracted", "message_id"]).map(str::to_owned),
                at,
                text,
            )
        }
        Origin::Chat => {
            let mut to = strings_at(m, &["source", "recipients"]);
            to.extend(strings_at(m, &["recipients"]));
            (Channel::Chat, to, None, None, text::clean_body(&raw, None))
        }
        Origin::Feedback(c) => (
            *c,
            strings_at(m, &["recipients"]),
            None,
            None,
            raw.trim().to_owned(),
        ),
        Origin::Note => (
            Channel::Document,
            Vec::new(),
            None,
            None,
            raw.trim().to_owned(),
        ),
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(Extracted {
        channel,
        recipients,
        message_id,
        at: at.or(item.source_modified_at).or(Some(item.updated_at)),
        text,
    }))
}

fn make_sample(
    store: &Store,
    item: &Item,
    origin: &Origin,
    my_names: &[String],
    vocab: &mut Vocab,
) -> Result<Option<Sample>> {
    let Some(x) = extract(store, item, origin)? else {
        return Ok(None);
    };
    let recipient_names = recipient_first_names(&x.recipients);
    let mut recipients: Vec<String> = Vec::new();
    for r in &x.recipients {
        if let Some(k) = recipient_key(r)
            && !recipients.contains(&k)
        {
            recipients.push(k);
        }
    }
    Ok(Some(Sample {
        channel: x.channel,
        recipients,
        at: x.at,
        sensitive: is_sensitive(&item.metadata),
        message_id: x.message_id,
        features: Features::of(&x.text, &recipient_names, my_names),
        terms: vocab.terms_of(&x.text),
        text_hash: fingerprint(&x.text),
        canon_hash: fingerprint(&text::canonical(&x.text)),
        name_key: text::signoff_name_key(&x.text).map_or(0, |k| fingerprint(&k)),
        shadowed: false,
    }))
}

#[cfg(test)]
mod tests {
    //! The compact profile against a reference of the earlier implementation, which held every
    //! sample's text and term map and recomputed everything from them.

    use nugget_store::{NewChunk, NewItem, content_hash};

    use super::*;

    #[test]
    fn the_mail_text_header_is_not_the_users_writing() {
        let meta = serde_json::json!({"extracted": {"text_header": "From: Me <me@x.example>\nDate: 2026-10-20\n\n"}});
        let text = "From: Me <me@x.example>\nDate: 2026-10-20\n\nHi Priya,\nThanks!".to_owned();
        assert_eq!(without_text_header(&meta, text), "Hi Priya,\nThanks!");
        // Older items (no header) and a header that doesn't match are left alone.
        let plain = "Hi Priya,\nThanks!".to_owned();
        assert_eq!(
            without_text_header(&serde_json::json!({}), plain.clone()),
            plain
        );
        assert_eq!(without_text_header(&meta, plain.clone()), plain);
    }

    /// A sample as the earlier implementation held it.
    struct RefSample {
        item: ItemRef,
        channel: Channel,
        recipients: Vec<String>,
        text: String,
        at: Option<OffsetDateTime>,
        sensitive: bool,
        message_id: Option<String>,
        features: Features,
        terms: HashMap<String, f32>,
    }

    /// The earlier eager profile: every item read, every own sample with its text.
    struct Reference {
        samples: Vec<RefSample>,
        overall: StyleStats,
        channels: HashMap<Channel, StyleStats>,
        recipients: HashMap<String, StyleStats>,
        config: VoiceConfig,
        me: MeIdentity,
    }

    fn old_jaccard(a: &HashMap<String, f32>, b: &HashMap<String, f32>) -> f32 {
        if a.is_empty() || b.is_empty() {
            return 0.0;
        }
        let inter = a.keys().filter(|k| b.contains_key(*k)).count() as f32;
        inter / (a.len() as f32 + b.len() as f32 - inter)
    }

    impl Reference {
        fn build(store: &Store, configured: &MeIdentity, config: &VoiceConfig) -> Self {
            let mut records: Vec<(Item, Origin)> = Vec::new();
            for c in store.source_counts().unwrap() {
                for key in store.item_versions(&c.source).unwrap().into_keys() {
                    let r = ItemRef {
                        source: c.source.clone(),
                        key,
                    };
                    let item = store.get_item(&r).unwrap().unwrap();
                    if let Some(origin) = classify(&item, config) {
                        records.push((item, origin));
                    }
                }
            }
            let me = if config.infer_me {
                configured.merged(&MeIdentity::infer(records.iter().filter_map(
                    |(_, o)| match o {
                        Origin::Mail {
                            from: Some(f),
                            sent: true,
                            draft: false,
                        } => Some(f.as_str()),
                        _ => None,
                    },
                )))
            } else {
                configured.clone()
            };
            let my_names = me.name_variants();
            let mut samples: Vec<RefSample> = Vec::new();
            for (item, origin) in &records {
                if !is_own(origin, &me) {
                    continue;
                }
                let Some(x) = extract(store, item, origin).unwrap() else {
                    continue;
                };
                let names = recipient_first_names(&x.recipients);
                let mut recipients: Vec<String> = Vec::new();
                for r in &x.recipients {
                    if let Some(k) = recipient_key(r)
                        && !recipients.contains(&k)
                    {
                        recipients.push(k);
                    }
                }
                samples.push(RefSample {
                    item: item.item.clone(),
                    channel: x.channel,
                    recipients,
                    at: x.at,
                    sensitive: is_sensitive(&item.metadata),
                    message_id: x.message_id,
                    features: Features::of(&x.text, &names, &my_names),
                    terms: text::terms(&x.text),
                    text: x.text,
                });
            }
            // Own samples newest first, one per Message-ID.
            samples.sort_by(|a, b| {
                b.at.cmp(&a.at)
                    .then_with(|| a.item.source.cmp(&b.item.source))
                    .then_with(|| a.item.key.cmp(&b.item.key))
            });
            let mut ids = HashSet::new();
            samples.retain(|s| match &s.message_id {
                Some(id) => ids.insert(id.clone()),
                None => true,
            });
            let messages: Vec<&Features> = samples
                .iter()
                .filter(|s| s.channel != Channel::Document)
                .map(|s| &s.features)
                .collect();
            let overall = if messages.is_empty() {
                aggregate(samples.iter().map(|s| &s.features))
            } else {
                aggregate(messages)
            };
            let mut by_channel: HashMap<Channel, Vec<&Features>> = HashMap::new();
            let mut by_recipient: HashMap<String, Vec<&Features>> = HashMap::new();
            for s in &samples {
                by_channel.entry(s.channel).or_default().push(&s.features);
                for r in &s.recipients {
                    by_recipient.entry(r.clone()).or_default().push(&s.features);
                }
            }
            let channels = by_channel
                .into_iter()
                .filter(|(_, f)| f.len() as u32 >= config.min_channel_samples)
                .map(|(c, f)| (c, aggregate(f)))
                .collect();
            let recipients = by_recipient
                .into_iter()
                .filter(|(_, f)| f.len() as u32 >= config.min_recipient_samples)
                .map(|(r, f)| (r, aggregate(f)))
                .collect();
            Self {
                samples,
                overall,
                channels,
                recipients,
                config: config.clone(),
                me,
            }
        }

        fn exemplars(&self, store: &Store, query: &VoiceQuery) -> Vec<String> {
            let want: HashSet<String> = query
                .recipients
                .iter()
                .filter_map(|r| recipient_key(r))
                .collect();
            let candidates: Vec<&RefSample> = self
                .samples
                .iter()
                .filter(|s| !(query.exclude_sensitive && s.sensitive))
                .filter(|s| {
                    (s.channel == Channel::Document) == (query.channel == Channel::Document)
                })
                .filter(|s| !s.text.trim().is_empty())
                .collect();
            if candidates.is_empty() {
                return Vec::new();
            }
            let thread_terms = text::terms(&query.thread_text);
            let mut store_rank: HashMap<&ItemRef, f32> = HashMap::new();
            let q = text::query_terms(&query.thread_text, QUERY_TERMS).join(" ");
            if !q.is_empty() || query.thread_embedding.is_some() {
                let own: HashSet<&ItemRef> = candidates.iter().map(|s| &s.item).collect();
                let hits = store
                    .search_hybrid_with_depth(
                        &q,
                        query.thread_embedding.as_deref(),
                        SEARCH_DEPTH,
                        SEARCH_DEPTH,
                    )
                    .unwrap();
                let mut pos = 0usize;
                for h in hits {
                    if let Some(r) = own.get(&h.item)
                        && !store_rank.contains_key(r)
                    {
                        store_rank.insert(r, 1.0 / (1.0 + pos as f32 * 0.25));
                        pos += 1;
                    }
                }
            }
            let newest = candidates.iter().filter_map(|s| s.at).max();
            let half_life = self.config.recency_half_life_days.max(1.0);
            let mut scored: Vec<(u8, f32, &RefSample)> = candidates
                .into_iter()
                .map(|s| {
                    let tier = if s.recipients.iter().any(|r| want.contains(r)) {
                        3
                    } else if s.channel == query.channel {
                        2
                    } else {
                        1
                    };
                    let topic = 0.5 * store_rank.get(&s.item).copied().unwrap_or(0.0)
                        + 0.5 * text::cosine(&s.terms, &thread_terms);
                    let recency = match (newest, s.at) {
                        (Some(n), Some(at)) => {
                            let days = (n - at).as_seconds_f32() / 86_400.0;
                            0.5f32.powf(days.max(0.0) / half_life)
                        }
                        _ => 0.5,
                    };
                    (tier, 0.65 * topic + 0.35 * recency, s)
                })
                .collect();
            scored.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then_with(|| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal))
                    .then_with(|| b.2.at.cmp(&a.2.at))
            });
            let max = self.config.max_exemplars.max(1);
            let min = self.config.min_exemplars.min(max);
            let mut chosen: Vec<&RefSample> = Vec::new();
            let mut seen_text: HashSet<String> = HashSet::new();
            for (tier, _, s) in scored {
                if chosen.len() >= max || (tier == 1 && chosen.len() >= min) {
                    break;
                }
                if !seen_text.insert(text::canonical(&s.text)) {
                    continue;
                }
                if chosen
                    .iter()
                    .any(|c| old_jaccard(&c.terms, &s.terms) >= NEAR_DUPLICATE)
                {
                    continue;
                }
                chosen.push(s);
            }
            chosen
                .into_iter()
                .map(|s| text::truncate(&s.text, self.config.exemplar_chars))
                .collect()
        }
    }

    const WORDS: &[&str] = &[
        "offsite",
        "venue",
        "lake",
        "budget",
        "contract",
        "invoice",
        "launch",
        "roadmap",
        "design",
        "review",
        "friday",
        "monday",
        "paddles",
        "kayak",
        "slides",
        "metrics",
        "hiring",
        "candidate",
        "interview",
        "dinner",
        "flight",
        "hotel",
        "agenda",
        "notes",
        "draft",
        "proposal",
        "pricing",
        "renewal",
        "support",
        "ticket",
        "deploy",
        "release",
    ];

    /// xorshift64*, deterministic.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn words(&mut self, n: usize) -> String {
            (0..n)
                .map(|_| WORDS[self.below(WORDS.len())])
                .collect::<Vec<_>>()
                .join(" ")
        }
    }

    const PEOPLE: &[&str] = &[
        "Sam Lee <sam@acme.example>",
        "Priya Shah <priya@studio.example>",
        "Dr. Ngozi Okafor <n.okafor@clinic.example>",
        "Jo Park <jo@park.example>",
    ];

    fn put(store: &Store, source: &str, key: &str, text: &str, meta: Value, version: &str) {
        let item = NewItem::new(source, key, content_hash(format!("{text}{meta}")))
            .with_text(text)
            .with_mime(meta["mime"].as_str().unwrap_or("text/plain"))
            .with_version(version)
            .with_metadata(meta);
        store
            .upsert_item_with_chunks(&item, &[NewChunk::text(text)])
            .unwrap();
    }

    /// Item `i` of a varied corpus: sent and received mail (duplicates across two sources by
    /// Message-ID, a draft, sensitive messages), chats and notes, with repeated and near
    /// repeated texts so deduplication is exercised.
    fn put_item(store: &Store, rng: &mut Rng, i: usize, version: &str) {
        let day = 1 + rng.below(28);
        let date = format!("2026-{:02}-{day:02}T{:02}:00:00Z", 1 + i % 9, i % 24);
        let to = PEOPLE[rng.below(PEOPLE.len())];
        let body = match i % 7 {
            // An exact repeat up to case and punctuation.
            0 => "Sounds good, see you Friday!".to_owned(),
            1 => "sounds good -- see you friday".to_owned(),
            _ => {
                let greeting = ["Hi Sam,", "hey", "Dear Dr. Okafor,", ""][rng.below(4)];
                let signoff = ["\n\nCheers,\nAlex", "\n\nThanks!", "\n\n- A", ""][rng.below(4)];
                let n = 4 + rng.below(30);
                format!("{greeting}\n{}.{signoff}", rng.words(n))
            }
        };
        match i % 5 {
            0..=2 => {
                let sent = !i.is_multiple_of(11);
                let from = if sent {
                    "Alex Morgan <alex@morgan.example>"
                } else {
                    to
                };
                let mut labels = vec![if sent { "SENT" } else { "INBOX" }];
                if i.is_multiple_of(23) {
                    labels.push("DRAFT");
                }
                let meta = json!({
                    "source": {"labels": labels, "from": from},
                    "mime": "message/rfc822",
                    "extracted": {"from": from, "to": [to], "date": date,
                                  "message_id": format!("<m{}@x>", i / 2)},
                    "sensitive": i.is_multiple_of(13),
                });
                let source = if i.is_multiple_of(2) {
                    "gmail:a"
                } else {
                    "imap:b"
                };
                put(store, source, &format!("m{i}"), &body, meta, version);
            }
            3 => {
                let meta = json!({
                    "source": {"from_me": true, "recipients": [to]},
                    "mime": "text/plain",
                });
                put(store, "slack:w", &format!("c{i}"), &body, meta, version);
            }
            _ => {
                let meta = json!({"mime": "text/markdown", "source": {"path": format!("n{i}.md")}});
                put(store, "files:notes", &format!("n{i}"), &body, meta, version);
            }
        }
    }

    fn queries() -> Vec<VoiceQuery> {
        let mut rng = Rng(99);
        let mut out = Vec::new();
        for channel in [
            Channel::Email,
            Channel::Chat,
            Channel::Document,
            Channel::Other,
        ] {
            for allow in [false, true] {
                for (i, to) in PEOPLE.iter().chain(&["nobody@x.example"]).enumerate() {
                    let thread = match i {
                        0 => String::new(),
                        _ => {
                            let n = 3 + rng.below(40);
                            rng.words(n)
                        }
                    };
                    out.push(
                        VoiceQuery::new(channel)
                            .with_recipients([*to])
                            .with_thread(thread)
                            .allow_sensitive(allow),
                    );
                }
            }
        }
        out
    }

    fn assert_matches_reference(p: &VoiceProfile, store: &Store, what: &str) {
        let r = Reference::build(store, &p.configured, &p.config);
        assert_eq!(p.me, r.me, "{what}: identity");
        assert_eq!(p.samples, r.samples.len() as u64, "{what}: samples");
        // Debug strings compare floats bit for bit (as far as they print).
        assert_eq!(
            format!("{:?}", p.overall),
            format!("{:?}", r.overall),
            "{what}"
        );
        for c in [
            Channel::Email,
            Channel::Chat,
            Channel::Document,
            Channel::Other,
        ] {
            assert_eq!(
                format!("{:?}", p.channels.get(&c)),
                format!("{:?}", r.channels.get(&c)),
                "{what}: {c:?}"
            );
        }
        let mut keys: Vec<&String> = r.recipients.keys().collect();
        keys.sort();
        assert_eq!(p.recipients.len(), keys.len(), "{what}: recipients");
        for k in keys {
            assert_eq!(
                format!("{:?}", p.recipients.get(k)),
                format!("{:?}", r.recipients.get(k)),
                "{what}: {k}"
            );
        }
        let mut nonempty = 0;
        for q in queries() {
            let got = p.exemplars(&q);
            assert_eq!(got, r.exemplars(store, &q), "{what}: {q:?}");
            nonempty += usize::from(!got.is_empty());
        }
        assert!(nonempty > 10, "{what}: the fixture yields exemplars");
    }

    fn me() -> MeIdentity {
        MeIdentity::new(["alex@morgan.example"], ["Alex Morgan"])
    }

    #[test]
    fn compact_profile_equals_the_eager_reference() {
        let store = Store::open_in_memory().unwrap();
        let mut rng = Rng(7);
        for i in 0..400 {
            put_item(&store, &mut rng, i, "1");
        }
        for config in [
            VoiceConfig::default(),
            VoiceConfig {
                max_exemplars: 3,
                exemplar_chars: 40,
                infer_me: false,
                ..Default::default()
            },
        ] {
            let built = VoiceProfile::build(&store, me(), config.clone()).unwrap();
            assert_matches_reference(&built, &store, "build");
            // Inferred identity only: sign-offs are re-normalised once names are known.
            let inferred = VoiceProfile::build(&store, MeIdentity::default(), config).unwrap();
            assert_matches_reference(&inferred, &store, "build, inferred me");
        }
    }

    #[test]
    fn incremental_and_loaded_profiles_equal_the_eager_reference() {
        let store = Store::open_in_memory().unwrap();
        let mut rng = Rng(11);
        for i in 0..200 {
            put_item(&store, &mut rng, i, "1");
        }
        let mut p =
            VoiceProfile::build(&store, MeIdentity::default(), VoiceConfig::default()).unwrap();
        p.save().unwrap();
        // More items, changed items and deleted items, picked up incrementally.
        for i in 200..400 {
            put_item(&store, &mut rng, i, "1");
        }
        for i in (0..200).step_by(3) {
            put_item(&store, &mut rng, i, "2");
        }
        for i in (1..200).step_by(17) {
            for (source, key) in [
                ("gmail:a", format!("m{i}")),
                ("imap:b", format!("m{i}")),
                ("slack:w", format!("c{i}")),
                ("files:notes", format!("n{i}")),
            ] {
                store
                    .delete_item(&ItemRef {
                        source: SourceId::new(source),
                        key: ItemKey::new(key),
                    })
                    .unwrap();
            }
        }
        let report = p.refresh().unwrap();
        assert!(report.fetched > 200 && report.removed > 0, "{report:?}");
        assert_matches_reference(&p, &store, "refreshed");

        // A snapshot taken before the changes, loaded and caught up.
        let mut loaded = VoiceProfile::load(&store, MeIdentity::default(), VoiceConfig::default())
            .unwrap()
            .unwrap();
        loaded.refresh().unwrap();
        assert_matches_reference(&loaded, &store, "loaded and refreshed");
        loaded.save().unwrap();
        let again = VoiceProfile::load(&store, MeIdentity::default(), VoiceConfig::default())
            .unwrap()
            .unwrap();
        assert_matches_reference(&again, &store, "loaded");
    }

    #[test]
    fn exemplars_changed_since_the_last_refresh_are_skipped() {
        let store = Store::open_in_memory().unwrap();
        let note = |key: &str, text: &str, v: &str| {
            put(
                &store,
                "files:notes",
                key,
                text,
                json!({"mime": "text/markdown"}),
                v,
            );
        };
        note("a", "Kayak trip planning for the lake offsite.", "1");
        note("b", "Budget review notes for the launch.", "1");
        let p = VoiceProfile::build(&store, me(), VoiceConfig::default()).unwrap();
        let q = VoiceQuery::new(Channel::Document);
        assert_eq!(p.exemplars(&q).len(), 2);
        note("a", "Something else entirely.", "2");
        assert_eq!(
            p.exemplars(&q),
            vec!["Budget review notes for the launch.".to_owned()]
        );
    }

    #[test]
    fn terms_roundtrip_and_reject_garbage() {
        for ids in [vec![], vec![0], vec![0, 0, 5, 127, 128, 16_384, u32::MAX]] {
            let t = Terms::pack(&ids);
            assert_eq!(t.ids().collect::<Vec<_>>(), ids);
            let json = serde_json::to_string(&t).unwrap();
            let back: Terms = serde_json::from_str(&json).unwrap();
            assert_eq!(back, t);
            assert!(back.is_valid(usize::MAX));
        }
        assert!(Terms::pack(&[3, 9]).is_valid(10));
        assert!(!Terms::pack(&[3, 10]).is_valid(10));
        assert!(serde_json::from_str::<Terms>("\"!!\"").is_err());
        // A varint that never ends, or one longer than five bytes.
        assert!(!Terms(Box::new([0x80])).is_valid(usize::MAX));
        assert!(!Terms(Box::new([0xff; 6])).is_valid(usize::MAX));
    }

    #[test]
    fn term_ids_give_the_same_cosine_and_jaccard_as_term_maps() {
        let mut vocab = Vocab::default();
        let mut rng = Rng(3);
        let texts: Vec<String> = (0..40).map(|i| rng.words(1 + i * 3)).collect();
        let thread = rng.words(25) + " unknownword";
        let tt = text::terms(&thread);
        let tn = tt.values().map(|v| v * v).sum::<f32>().sqrt();
        let ids: Vec<Terms> = texts.iter().map(|t| vocab.terms_of(t)).collect();
        let mut tids = vec![0.0; vocab.len()];
        for (t, w) in &tt {
            if let Some(id) = vocab.get(t) {
                tids[id as usize] = *w;
            }
        }
        for (i, t) in texts.iter().enumerate() {
            let a = text::terms(t);
            assert_eq!(cosine_ids(&ids[i], &tids, tn), text::cosine(&a, &tt));
            for (j, u) in texts.iter().enumerate() {
                assert_eq!(
                    jaccard_ids(&ids[i], &ids[j]),
                    old_jaccard(&a, &text::terms(u))
                );
            }
        }
    }

    #[test]
    fn holds_no_texts_and_bounds_the_vocabulary() {
        let store = Store::open_in_memory().unwrap();
        let mut rng = Rng(5);
        // Notes with many distinct words each (so the vocabulary matters).
        let n = 300;
        for i in 0..n {
            let words: Vec<String> = (0..150)
                .map(|w| format!("w{}x{}", i, w % 50 + rng.below(3)))
                .collect();
            let text = format!("{} {}", words.join(" "), rng.words(20));
            put(
                &store,
                "files:notes",
                &format!("n{i}"),
                &text,
                json!({"mime": "text/markdown"}),
                "1",
            );
        }
        let mut p = VoiceProfile::build(&store, me(), VoiceConfig::default()).unwrap();
        assert_eq!(p.samples, n as u64);
        let held = p.approx_heap_bytes();
        // Per sample: fixed fields, at most 2 bytes per term occurrence (170 here; no text),
        // and the vocabulary (≤ 52 new words per note).
        let budget = n * (std::mem::size_of::<Sample>() + 170 * 2 + 52 * 60 + 256);
        assert!(held < budget, "held {held} B, budget {budget} B");
        let tokens: usize = p.all_samples().map(|o| o.sample.terms.ids().count()).sum();
        assert_eq!(tokens, n * 170);
        assert!(p.vocab.len() <= n * 53 + WORDS.len());

        // Replace every note: the old terms are dropped from the vocabulary.
        for i in 0..n {
            put(
                &store,
                "files:notes",
                &format!("n{i}"),
                &rng.words(30),
                json!({"mime": "text/markdown"}),
                "2",
            );
        }
        p.refresh().unwrap();
        assert!(p.vocab.len() <= WORDS.len(), "{}", p.vocab.len());
        assert_matches_reference_lite(&p, &store);
    }

    /// Exemplars after the vocabulary was compacted still match the reference.
    fn assert_matches_reference_lite(p: &VoiceProfile, store: &Store) {
        let r = Reference::build(store, &p.configured, &p.config);
        for q in queries() {
            assert_eq!(p.exemplars(&q), r.exemplars(store, &q), "{q:?}");
        }
    }
}
