//! The voice profile: the user's own messages from the store, their stats, and exemplars.

use std::collections::{HashMap, HashSet};

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

#[derive(Debug, Clone, PartialEq)]
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

#[derive(Debug, Clone)]
struct Sample {
    item: ItemRef,
    channel: Channel,
    /// Recipient keys (lowercased addresses or handles).
    recipients: Vec<String>,
    recipient_names: Vec<String>,
    text: String,
    at: Option<OffsetDateTime>,
    sensitive: bool,
    message_id: Option<String>,
    features: Features,
    terms: HashMap<String, f32>,
}

#[derive(Debug, Clone)]
struct Record {
    origin: Origin,
    sample: Option<Sample>,
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
    /// Versions of every item seen, per source: the incremental cursor.
    seen: HashMap<SourceId, HashMap<ItemKey, Option<String>>>,
    records: HashMap<ItemRef, Record>,
    overall: StyleStats,
    channels: HashMap<Channel, StyleStats>,
    recipients: HashMap<String, StyleStats>,
    samples: u64,
    last_built: Option<OffsetDateTime>,
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
    /// Build the profile from every item in `store`. `me` is the configured identity; with
    /// [`VoiceConfig::infer_me`] it is completed from sent mail.
    pub fn build(store: &Store, me: MeIdentity, config: VoiceConfig) -> Result<Self> {
        let mut p = Self {
            store: store.clone(),
            config,
            me: me.clone(),
            configured: me,
            seen: HashMap::new(),
            records: HashMap::new(),
            overall: StyleStats::default(),
            channels: HashMap::new(),
            recipients: HashMap::new(),
            samples: 0,
            last_built: None,
        };
        p.refresh()?;
        Ok(p)
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
        let gone: Vec<SourceId> = self
            .seen
            .keys()
            .filter(|s| !live.contains(s))
            .cloned()
            .collect();
        for s in gone {
            if let Some(keys) = self.seen.remove(&s) {
                report.removed += keys.len() as u64;
                for k in keys.into_keys() {
                    self.records.remove(&ItemRef {
                        source: s.clone(),
                        key: k,
                    });
                }
            }
        }

        let my_names = self.me.name_variants();
        for source in &sources {
            let versions = self.store.item_versions(source)?;
            report.scanned += versions.len() as u64;
            let seen = self.seen.entry(source.clone()).or_default();
            let removed: Vec<ItemKey> = seen
                .keys()
                .filter(|k| !versions.contains_key(*k))
                .cloned()
                .collect();
            for k in removed {
                seen.remove(&k);
                report.removed += 1;
                self.records.remove(&ItemRef {
                    source: source.clone(),
                    key: k,
                });
            }
            for (key, version) in versions {
                match seen.get(&key) {
                    // Unversioned items are read once.
                    Some(old) if version.is_none() || *old == version => continue,
                    _ => {}
                }
                let item_ref = ItemRef {
                    source: source.clone(),
                    key: key.clone(),
                };
                seen.insert(key, version);
                report.fetched += 1;
                let Some(item) = self.store.get_item(&item_ref)? else {
                    self.records.remove(&item_ref);
                    continue;
                };
                match classify(&item, &self.config) {
                    Some(origin) => {
                        let sample = if is_own(&origin, &self.me) {
                            make_sample(&self.store, &item, &origin, &my_names)?
                        } else {
                            None
                        };
                        self.records.insert(item_ref, Record { origin, sample });
                    }
                    None => {
                        self.records.remove(&item_ref);
                    }
                }
            }
        }

        // Re-infer "me", then make sure every own record has a sample (and only those).
        let me = if self.config.infer_me {
            let inferred =
                MeIdentity::infer(self.records.values().filter_map(|r| match &r.origin {
                    Origin::Mail {
                        from: Some(f),
                        sent: true,
                        draft: false,
                    } => Some(f.as_str()),
                    _ => None,
                }));
            self.configured.merged(&inferred)
        } else {
            self.configured.clone()
        };
        let names_changed = me.names != self.me.names || me.emails != self.me.emails;
        self.me = me;
        let my_names = self.me.name_variants();
        let refs: Vec<ItemRef> = self.records.keys().cloned().collect();
        for r in refs {
            let Some(rec) = self.records.get(&r) else {
                continue;
            };
            let own = is_own(&rec.origin, &self.me);
            match (own, rec.sample.is_some()) {
                (true, false) => {
                    let origin = rec.origin.clone();
                    let sample = match self.store.get_item(&r)? {
                        Some(item) => make_sample(&self.store, &item, &origin, &my_names)?,
                        None => None,
                    };
                    if let Some(rec) = self.records.get_mut(&r) {
                        rec.sample = sample;
                    }
                }
                (false, true) => {
                    if let Some(rec) = self.records.get_mut(&r) {
                        rec.sample = None;
                    }
                }
                (true, true) if names_changed => {
                    if let Some(s) = self.records.get_mut(&r).and_then(|x| x.sample.as_mut()) {
                        s.features = Features::of(&s.text, &s.recipient_names, &my_names);
                    }
                }
                _ => {}
            }
        }

        self.recompute();
        report.samples = self.samples;
        let now = OffsetDateTime::now_utc();
        self.last_built = Some(now);
        self.store.set_cursor(
            &SourceId::new(VOICE_CURSOR_SOURCE),
            json!({
                "v": 1,
                "samples": self.samples,
                "items_seen": self.seen.values().map(|m| m.len()).sum::<usize>(),
                "last_built": now.format(&Rfc3339).ok(),
            }),
        )?;
        Ok(report)
    }

    /// Own samples, one per message (duplicates by `Message-ID` across sources removed),
    /// newest first.
    fn own_samples(&self) -> Vec<&Sample> {
        let mut all: Vec<&Sample> = self
            .records
            .values()
            .filter_map(|r| r.sample.as_ref())
            .collect();
        all.sort_by(|a, b| {
            b.at.cmp(&a.at)
                .then_with(|| a.item.source.cmp(&b.item.source))
                .then_with(|| a.item.key.cmp(&b.item.key))
        });
        let mut ids = HashSet::new();
        all.retain(|s| match &s.message_id {
            Some(id) => ids.insert(id.as_str()),
            None => true,
        });
        all
    }

    fn recompute(&mut self) {
        let samples = self.own_samples();
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
            .filter(|(_, f)| f.len() as u32 >= self.config.min_channel_samples)
            .map(|(c, f)| (c, aggregate(f)))
            .collect();
        let recipients = by_recipient
            .into_iter()
            .filter(|(_, f)| f.len() as u32 >= self.config.min_recipient_samples)
            .map(|(r, f)| (r, aggregate(f)))
            .collect();
        let n = samples.len() as u64;
        self.overall = overall;
        self.channels = channels;
        self.recipients = recipients;
        self.samples = n;
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

    /// Exemplars for `query`: see [`VoiceSource::hints`].
    pub fn exemplars(&self, query: &VoiceQuery) -> Vec<String> {
        let want: HashSet<String> = query
            .recipients
            .iter()
            .filter_map(|r| recipient_key(r))
            .collect();
        let candidates: Vec<&Sample> = self
            .own_samples()
            .into_iter()
            .filter(|s| !(query.exclude_sensitive && s.sensitive))
            // Notes are exemplars only for documents.
            .filter(|s| (s.channel == Channel::Document) == (query.channel == Channel::Document))
            .filter(|s| !s.text.trim().is_empty())
            .collect();
        if candidates.is_empty() {
            return Vec::new();
        }

        // Topic: rank among the user's own items in a hybrid store search, plus a local term
        // cosine against the thread.
        let thread_terms = text::terms(&query.thread_text);
        let mut store_rank: HashMap<&ItemRef, f32> = HashMap::new();
        let q = text::query_terms(&query.thread_text, QUERY_TERMS).join(" ");
        if !q.is_empty() || query.thread_embedding.is_some() {
            let own: HashSet<&ItemRef> = candidates.iter().map(|s| &s.item).collect();
            match self.store.search_hybrid_with_depth(
                &q,
                query.thread_embedding.as_deref(),
                SEARCH_DEPTH,
                SEARCH_DEPTH,
            ) {
                Ok(hits) => {
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
                Err(e) => tracing::warn!(error = %e, "voice: exemplar search failed"),
            }
        }
        let newest = candidates.iter().filter_map(|s| s.at).max();
        let half_life = self.config.recency_half_life_days.max(1.0);
        let mut scored: Vec<(u8, f32, &Sample)> = candidates
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
                .then_with(|| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
                .then_with(|| b.2.at.cmp(&a.2.at))
        });

        let max = self.config.max_exemplars.max(1);
        let min = self.config.min_exemplars.min(max);
        let mut chosen: Vec<&Sample> = Vec::new();
        let mut seen_text: HashSet<String> = HashSet::new();
        for (tier, _, s) in scored {
            if chosen.len() >= max || (tier == 1 && chosen.len() >= min) {
                break;
            }
            let norm = text::canonical(&s.text);
            if !seen_text.insert(norm) {
                continue;
            }
            if chosen
                .iter()
                .any(|c| jaccard(&c.terms, &s.terms) >= NEAR_DUPLICATE)
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

impl VoiceSource for VoiceProfile {
    /// Stats from [`VoiceProfile::stats_for`] and up to [`VoiceConfig::max_exemplars`]
    /// exemplars (each at most [`VoiceConfig::exemplar_chars`]), ordered by: messages to one of
    /// the recipients, then messages in the same channel, then (only to reach
    /// [`VoiceConfig::min_exemplars`]) other channels; within each group by topic similarity to
    /// the thread (hybrid search over the user's own items plus term cosine) blended with
    /// recency. Exact and near duplicates are skipped. Sensitive items are left out when
    /// [`VoiceQuery::exclude_sensitive`] is set.
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

fn jaccard(a: &HashMap<String, f32>, b: &HashMap<String, f32>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.keys().filter(|k| b.contains_key(*k)).count() as f32;
    inter / (a.len() as f32 + b.len() as f32 - inter)
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
    if let Some(t) = &item.text {
        return Ok(t.clone());
    }
    let ords: Vec<u32> = (0..FALLBACK_CHUNKS).collect();
    let chunks = store.get_chunks_by_ord(&item.item, &ords)?;
    Ok(chunks
        .into_iter()
        .map(|c| c.text)
        .collect::<Vec<_>>()
        .join("\n\n"))
}

fn make_sample(
    store: &Store,
    item: &Item,
    origin: &Origin,
    my_names: &[String],
) -> Result<Option<Sample>> {
    let m = &item.metadata;
    let raw = item_text(store, item)?;
    let (channel, recipients_raw, message_id, at, text) = match origin {
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
    let at = at.or(item.source_modified_at).or(Some(item.updated_at));
    let recipient_names = recipient_first_names(&recipients_raw);
    let mut recipients: Vec<String> = Vec::new();
    for r in &recipients_raw {
        if let Some(k) = recipient_key(r)
            && !recipients.contains(&k)
        {
            recipients.push(k);
        }
    }
    let features = Features::of(&text, &recipient_names, my_names);
    let terms = text::terms(&text);
    Ok(Some(Sample {
        item: item.item.clone(),
        channel,
        recipients,
        recipient_names,
        text,
        at,
        sensitive: is_sensitive(m),
        message_id,
        features,
        terms,
    }))
}
