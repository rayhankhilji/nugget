//! One run of the pipeline: plan the work queue, process items, apply claims.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Instant;

use nugget_core::ids::{EntityId, ItemKey, ItemRef, SourceId};
use nugget_core::{Error, Result};
use nugget_graph::{
    Claim, ClaimId, ClaimObject, ClaimPatch, ClaimStatus, EntityDetail, EntityKind, EntityPatch,
    EntityQuery, Evidence, GraphBatch, GraphOp, Identifier, Mention, NewClaim, NewEntity,
    normalize_name,
};
use nugget_store::{Item, ItemChange};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::feed::ItemFeed;
use crate::knowledge::{Inner, RunStats};
use crate::resolve::Resolver;
use crate::spec::{ERef, Extraction, OSpec};
use crate::state::{
    EXTRACTOR_VERSION, Entry, LLM_DONE, LLM_NONE, LLM_PENDING, MAX_PENDING_LINKS, Notes,
    PendingLink, SourceLedger, State,
};
use crate::temporal::{self, Cue};
use crate::text::{MAX_QUOTE_CHARS, normalize_ws, truncate_chars};

/// Processing failures after which an item is left alone until it changes.
pub(crate) const MAX_FAILURES: u32 = 3;
/// Single-valued predicates (see [`crate::KnowledgeConfig::single_valued_predicates`]): one value
/// at a time is current, earlier ones are superseded history (see `Session::settle`).
///
/// - `works_at`, `lives_in`, `reports_to`: a person's employer, home and manager change over
///   time; only the latest is current.
/// - `has_title`, and `has_role` (the gold vocabulary's name for it, kept in case an extractor or
///   configuration writes it; the LLM's `role`, `position`, `job_title`, `works_as`, ... are
///   mapped to `has_title`): one job title at a time.
/// - `born_on`, `due_on`: one date; a different one is a correction.
/// - `located_at` (a calendar event's location) and `held_at` (the LLM's name for it): an event
///   takes place at one venue; a moved event supersedes the old venue.
pub const SINGLE_VALUED: &[&str] = &[
    "works_at",
    "lives_in",
    "has_title",
    "has_role",
    "reports_to",
    "born_on",
    "due_on",
    "located_at",
    "held_at",
];
/// Predicates whose validity is the time of the event they relate to (a calendar event's start,
/// which the extractor gives explicitly), not when an item mentioned it.
const EVENT_DATED: &[&str] = &["attended", "organized", "invited_to"];
/// Single-valued predicates whose start an extractor may know (an event's venue from its
/// calendar entry): a `valid_from` that is not one of the claim's evidence dates is kept.
const EXPLICIT_START: &[&str] = &["located_at", "held_at"];
/// Distinct quotes per claim (the newest by item date, then text) read for temporal cues. Dates
/// are read from every evidence item; only the quote work is capped, and the choice depends on
/// the evidence, not on the order it arrived in.
const CUE_SCAN: usize = 256;
/// Cached item dates at most (the cache is cleared beyond).
#[cfg(not(test))]
const DATE_CACHE: usize = 10_000;
#[cfg(test)]
const DATE_CACHE: usize = 4;
/// Entities migrated per write batch by the backfill.
const BACKFILL_PAGE: usize = 64;
/// How far in the future an item's date may be: later dates (a forged mail `Date`) are clamped,
/// so an item cannot pin a value as current for years.
const FUTURE_SLACK: time::Duration = time::Duration::days(1);

/// Item dates (clamped, see [`FUTURE_SLACK`]) for one computation.
type Dates = HashMap<ItemRef, OffsetDateTime>;
/// Upper bound of aggregated confidence.
const MAX_AGGREGATE: f32 = 0.99;
/// Name of the user's entity when no name is configured or learned.
pub(crate) const DEFAULT_ME_NAME: &str = "Me";

/// An item the plan says to look at.
#[derive(Debug, Clone)]
pub(crate) struct Work {
    pub item: ItemRef,
    pub version: Option<String>,
    pub prev: Option<Entry>,
    pub llm_retry: bool,
}

pub(crate) struct Plan {
    pub state: State,
    pub notes: Notes,
    pub ledgers: HashMap<SourceId, SourceLedger>,
    pub deletes: Vec<ItemRef>,
    pub upserts: Vec<Work>,
    /// Change-log position to store once every planned item is done (`None`: the feed keeps no
    /// change log).
    pub next_cursor: Option<u64>,
}

/// Whether an item needs processing given its ledger entry and current connector version, and
/// whether that is only an LLM retry.
fn decide(
    prev: Option<&Entry>,
    version: &Option<String>,
    llm_ready: bool,
    max_llm_attempts: u32,
) -> (bool, bool) {
    match prev {
        None => (true, false),
        Some(e) if e.failures > 0 && e.failures < MAX_FAILURES => (true, false),
        Some(_) if version.is_none() => (true, false),
        Some(e) if *version != e.version => (true, false),
        Some(e) => {
            let retry = llm_ready && llm_retry_due(e, max_llm_attempts);
            (retry, retry)
        }
    }
}

fn llm_retry_due(e: &Entry, max_llm_attempts: u32) -> bool {
    e.llm >= LLM_PENDING && e.llm - LLM_PENDING < max_llm_attempts
}

fn failure_retry_due(e: &Entry) -> bool {
    e.failures > 0 && e.failures < MAX_FAILURES
}

/// Build the work queue (blocking: run off the async runtime). With a change log and a stored
/// cursor, only items changed or deleted since the cursor are looked at, plus ledger entries due
/// for a retry when the retry hints say there may be some; otherwise every source is diffed
/// against the ledger.
pub(crate) fn plan(feed: &dyn ItemFeed, llm_ready: bool, max_llm_attempts: u32) -> Result<Plan> {
    let mut state = State::load(feed)?;
    let notes = Notes::load(feed)?;
    let reset = state.version != EXTRACTOR_VERSION;
    if reset {
        state.item_cursor = None;
        state.version = EXTRACTOR_VERSION;
    }
    if let Some(cursor) = state.item_cursor
        && let Some((changes, next)) = feed.items_changed_since(cursor, 0)?
    {
        return plan_incremental(
            feed,
            state,
            notes,
            changes,
            next,
            llm_ready,
            max_llm_attempts,
        );
    }
    plan_full(feed, state, notes, reset, llm_ready, max_llm_attempts)
}

fn plan_full(
    feed: &dyn ItemFeed,
    mut state: State,
    notes: Notes,
    reset: bool,
    llm_ready: bool,
    max_llm_attempts: u32,
) -> Result<Plan> {
    // Taken before the scan: changes made during it are seen (again) by the next run.
    let head = feed.item_changes_head()?;
    let mut sources: BTreeSet<SourceId> = feed.sources()?.into_iter().collect();
    sources.extend(state.sources.iter().cloned());
    let mut ledgers = HashMap::new();
    let mut deletes = Vec::new();
    let mut upserts = Vec::new();
    let (mut retry_failed, mut retry_llm) = (false, false);
    for source in sources {
        let versions = feed.item_versions(&source)?;
        let mut ledger = SourceLedger::load(feed, &source)?;
        // Items processed before but gone from the store lose their evidence, upgrade or not.
        let mut gone: Vec<&String> = ledger
            .keys()
            .filter(|k| !versions.contains_key(&ItemKey::new(k.as_str())))
            .collect();
        gone.sort();
        deletes.extend(gone.into_iter().map(|k| ItemRef {
            source: source.clone(),
            key: ItemKey::new(k.as_str()),
        }));
        if reset {
            // An extractor upgrade re-extracts every item: each one's old evidence is retracted
            // first, so what the old extractor got wrong (duplicates included) is re-resolved by
            // the new one, and entities left with nothing behind them are pruned.
            ledger.clear();
        }
        let mut keys: Vec<(&ItemKey, &Option<String>)> = versions.iter().collect();
        keys.sort_by(|a, b| a.0.cmp(b.0));
        for (key, version) in keys {
            let prev = ledger.get(key).cloned();
            if let Some(e) = &prev {
                retry_failed |= failure_retry_due(e);
                retry_llm |= llm_retry_due(e, max_llm_attempts);
            }
            let (todo, llm_retry) = decide(prev.as_ref(), version, llm_ready, max_llm_attempts);
            if todo {
                upserts.push(Work {
                    item: ItemRef {
                        source: source.clone(),
                        key: key.clone(),
                    },
                    version: version.clone(),
                    prev,
                    llm_retry,
                });
            }
        }
        ledgers.insert(source, ledger);
    }
    state.retry_failed = retry_failed;
    state.retry_llm = retry_llm;
    Ok(Plan {
        state,
        notes,
        ledgers,
        deletes,
        upserts,
        next_cursor: head,
    })
}

fn plan_incremental(
    feed: &dyn ItemFeed,
    mut state: State,
    notes: Notes,
    changes: Vec<ItemChange>,
    next: u64,
    llm_ready: bool,
    max_llm_attempts: u32,
) -> Result<Plan> {
    let mut ledgers: HashMap<SourceId, SourceLedger> = HashMap::new();
    let mut deletes = Vec::new();
    let mut upserts = Vec::new();
    let mut planned: HashSet<ItemRef> = HashSet::new();
    for c in changes {
        if !ledgers.contains_key(&c.item.source) {
            let l = SourceLedger::load(feed, &c.item.source)?;
            ledgers.insert(c.item.source.clone(), l);
        }
        let ledger = &ledgers[&c.item.source];
        let prev = ledger.get(&c.item.key).cloned();
        if c.deleted {
            if prev.is_some() {
                deletes.push(c.item.clone());
                planned.insert(c.item);
            }
            continue;
        }
        let (todo, llm_retry) = decide(prev.as_ref(), &c.version, llm_ready, max_llm_attempts);
        if todo {
            planned.insert(c.item.clone());
            upserts.push(Work {
                item: c.item,
                version: c.version,
                prev,
                llm_retry,
            });
        }
    }
    // Entries due for a retry are only in the ledgers: scan them when a hint says so.
    if state.retry_failed || (llm_ready && state.retry_llm) {
        let (mut retry_failed, mut retry_llm) = (false, false);
        for source in state.sources.clone() {
            if !ledgers.contains_key(&source) {
                let l = SourceLedger::load(feed, &source)?;
                ledgers.insert(source.clone(), l);
            }
            let ledger = &ledgers[&source];
            let mut keys: Vec<&String> = ledger.keys().collect();
            keys.sort();
            for k in keys {
                let key = ItemKey::new(k.as_str());
                let Some(e) = ledger.get(&key) else { continue };
                let failed = failure_retry_due(e);
                let llm = llm_retry_due(e, max_llm_attempts);
                retry_failed |= failed;
                retry_llm |= llm;
                let item = ItemRef {
                    source: source.clone(),
                    key,
                };
                if (failed || (llm_ready && llm)) && !planned.contains(&item) {
                    planned.insert(item.clone());
                    upserts.push(Work {
                        item,
                        version: e.version.clone(),
                        prev: Some(e.clone()),
                        llm_retry: !failed,
                    });
                }
            }
        }
        state.retry_failed = retry_failed;
        state.retry_llm = retry_llm;
    }
    Ok(Plan {
        state,
        notes,
        ledgers,
        deletes,
        upserts,
        next_cursor: Some(next),
    })
}

fn fingerprint(item: &Item) -> String {
    format!(
        "{}:{}",
        item.content_hash,
        item.updated_at.unix_timestamp_nanos()
    )
}

fn claim_key(subject: &EntityId, predicate: &str, object: &ClaimObject) -> String {
    format!(
        "{subject}\u{1f}{predicate}\u{1f}{}",
        serde_json::to_string(object).unwrap_or_default()
    )
}

/// The earliest date of a claim's evidence among `dates`.
fn earliest(c: &Claim, dates: &Dates) -> Option<OffsetDateTime> {
    c.evidence
        .iter()
        .filter_map(|e| dates.get(&e.item).copied())
        .min()
}

/// Noisy-OR of `n` independent observations at confidence `c`.
fn aggregate(c: f32, n: usize) -> f32 {
    let c = c.clamp(0.0, 1.0);
    let n = i32::try_from(n.clamp(1, 64)).unwrap_or(64);
    (1.0 - (1.0 - c).powi(n)).min(MAX_AGGREGATE.max(c))
}

/// Where a claim written during the current item lives: an op of the pending batch (its id is
/// known once the batch is applied) or an existing claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Queued {
    Op(usize),
    Id(ClaimId),
}

/// Claims retracted from an item just before it is re-extracted, to revive rather than
/// duplicate (stable claim ids across edits).
#[derive(Default)]
struct Retracted {
    reuse: HashMap<String, ClaimId>,
}

/// A claim written during the current item, looked at again once the batch is applied.
#[derive(Debug, Clone, Copy)]
struct Written {
    target: Queued,
    /// Base confidence (aggregated over evidence items with noisy-OR).
    base: f32,
    /// The item's date.
    date: OffsetDateTime,
    /// A validity start the extractor knows (a calendar event's start).
    explicit: Option<OffsetDateTime>,
    /// A single-valued predicate: [`Session::settle`] sets its intervals.
    single: bool,
}

/// One value (object) of a single-valued pair, summarised from its claims' evidence for
/// [`Session::settle`].
struct ValueGroup {
    /// The object's name and aliases (or the literal), for cues in quotes.
    names: Vec<String>,
    /// The object's name (the last tie-break).
    name: String,
    claims: Vec<Claim>,
    /// The user rejected this value.
    rejected: bool,
    /// Some claim is active or superseded with evidence, or user-asserted.
    live: bool,
    /// The newest user assertion (its newest evidence, else when it was asserted).
    user: Option<OffsetDateTime>,
    /// Last sighting: the newest evidence date whose quote does not say the fact ended.
    affirmed: Option<OffsetDateTime>,
    /// The newest evidence date whose quote says the fact ended.
    ended: Option<OffsetDateTime>,
    /// Dates of evidence whose quote says the fact started.
    started_at: Vec<OffsetDateTime>,
    /// Every date the value was seen holding (affirming evidence, user assertions).
    sightings: Vec<OffsetDateTime>,
    /// Cue of each quote read so far.
    cues: HashMap<String, Cue>,
}

impl ValueGroup {
    fn new(names: Vec<String>) -> Self {
        Self {
            name: names.first().cloned().unwrap_or_default(),
            names,
            claims: Vec::new(),
            rejected: false,
            live: false,
            user: None,
            affirmed: None,
            ended: None,
            started_at: Vec::new(),
            sightings: Vec::new(),
            cues: HashMap::new(),
        }
    }

    /// The cue of `quote` for this value (cached).
    fn cue(&mut self, quote: &str) -> Cue {
        if let Some(c) = self.cues.get(quote) {
            return *c;
        }
        let names: Vec<&str> = self.names.iter().map(String::as_str).collect();
        let c = temporal::cue(quote, &names);
        self.cues.insert(quote.to_owned(), c);
        c
    }

    fn add(&mut self, c: &Claim, dates: &Dates) {
        self.claims.push(c.clone());
        match c.status {
            ClaimStatus::Rejected => {
                self.rejected = true;
                return;
            }
            ClaimStatus::Retracted => return,
            ClaimStatus::Active | ClaimStatus::Superseded if c.evidence.is_empty() => return,
            _ => {}
        }
        self.live = true;
        // Every evidence item's date; quotes newest first (then by text), so which ones are read
        // for cues does not depend on the order evidence was added in.
        let mut dated: Vec<(OffsetDateTime, Option<&str>)> = c
            .evidence
            .iter()
            .filter_map(|e| dates.get(&e.item).map(|d| (*d, e.quote.as_deref())))
            .collect();
        dated.sort_by(|a, b| b.cmp(a));
        dated.dedup();
        let newest = dated.first().map(|d| d.0);
        let mut read: HashSet<&str> = HashSet::new();
        for (d, q) in dated {
            let cue = match q {
                Some(q) if read.contains(q) || read.len() < CUE_SCAN => {
                    read.insert(q);
                    self.cue(q)
                }
                _ => Cue::Neutral,
            };
            if cue == Cue::Ended {
                self.ended = self.ended.max(Some(d));
            } else {
                self.affirmed = self.affirmed.max(Some(d));
                self.sightings.push(d);
                if cue == Cue::Started {
                    self.started_at.push(d);
                }
            }
        }
        if c.status == ClaimStatus::UserAsserted {
            let t = newest.unwrap_or(c.created_at);
            self.user = self.user.max(Some(t));
            self.sightings.push(t);
        }
    }

    /// The last word on this value is that it ended (a user assertion never ends).
    fn ended(&self) -> bool {
        self.user.is_none() && self.ended.is_some() && self.ended >= self.affirmed
    }

    /// Its last sighting says it started then.
    fn started(&self) -> bool {
        self.affirmed.is_some_and(|a| self.started_at.contains(&a))
    }
}

pub(crate) struct Session<'a> {
    pub inner: &'a Inner,
    pub state: State,
    pub notes: Notes,
    pub ledgers: HashMap<SourceId, SourceLedger>,
    pub stats: RunStats,
    pub touched: BTreeSet<EntityId>,
    dates: HashMap<ItemRef, Option<OffsetDateTime>>,
    pub llm_left: usize,
    since_flush: usize,
    me_set: HashSet<String>,
    single_valued: HashSet<String>,
    /// One item's mention, claim, evidence and provenance writes, applied together
    /// ([`Session::commit`]).
    batch: GraphBatch,
    /// Claims already written in `batch`, by triple key, so a triple seen twice in one item
    /// lands in one claim.
    queued: HashMap<String, Queued>,
    /// Claims written in `batch`, to re-aggregate (noisy-OR) and date once it is applied.
    agg: Vec<Written>,
    /// Single-valued pairs (subject, predicate) to settle once `batch` is applied.
    settle_queue: BTreeSet<(EntityId, String)>,
    /// Multi-valued claims that lost evidence: their start is recomputed.
    redate: BTreeSet<ClaimId>,
    /// Item dates later than this are clamped to it (see [`FUTURE_SLACK`]).
    horizon: OffsetDateTime,
    /// Some item's provenance was retracted during this run (orphans may be left).
    pub retracted_any: bool,
}

impl<'a> Session<'a> {
    pub fn new(
        inner: &'a Inner,
        plan_state: State,
        notes: Notes,
        ledgers: HashMap<SourceId, SourceLedger>,
        llm_budget: usize,
    ) -> Self {
        let mut me_set: HashSet<String> = inner
            .cfg
            .me
            .emails
            .iter()
            .map(|e| e.trim().to_lowercase())
            .filter(|e| !e.is_empty())
            .collect();
        me_set.extend(plan_state.me_emails.iter().cloned());
        let single_valued = if inner.cfg.single_valued_predicates.is_empty() {
            SINGLE_VALUED.iter().map(|s| (*s).to_owned()).collect()
        } else {
            inner.cfg.single_valued_predicates.iter().cloned().collect()
        };
        Self {
            inner,
            state: plan_state,
            notes,
            ledgers,
            stats: RunStats::default(),
            touched: BTreeSet::new(),
            dates: HashMap::new(),
            llm_left: llm_budget,
            since_flush: 0,
            me_set,
            single_valued,
            batch: GraphBatch::new(),
            queued: HashMap::new(),
            agg: Vec::new(),
            settle_queue: BTreeSet::new(),
            redate: BTreeSet::new(),
            horizon: OffsetDateTime::now_utc() + FUTURE_SLACK,
            retracted_any: false,
        }
    }

    fn feed(&self) -> &dyn ItemFeed {
        self.inner.feed.as_ref()
    }

    fn graph(&self) -> &dyn nugget_graph::GraphStore {
        self.inner.graph.as_ref()
    }

    fn me_label(&self) -> String {
        self.inner
            .cfg
            .me
            .name
            .clone()
            .unwrap_or_else(|| "You".into())
    }

    fn touch(&mut self, id: &EntityId) {
        self.touched.insert(id.clone());
    }

    fn touch_claim(&mut self, c: &Claim) {
        self.touched.insert(c.subject.clone());
        if let ClaimObject::Entity(o) = &c.object {
            self.touched.insert(o.clone());
        }
    }

    // ---------------------------------------------------------------- persistence

    fn ledger(&mut self, source: &SourceId) -> &mut SourceLedger {
        self.ledgers.entry(source.clone()).or_default()
    }

    fn record(&mut self, item: &ItemRef, entry: Entry) -> Result<()> {
        self.state.retry_failed |= failure_retry_due(&entry);
        self.state.retry_llm |= entry.llm >= LLM_PENDING;
        self.ledger(&item.source).set(&item.key, entry);
        if !self.state.sources.contains(&item.source) {
            self.state.sources.push(item.source.clone());
            self.state.sources.sort();
        }
        self.state.processed_total += 1;
        self.since_flush += 1;
        if self.since_flush >= self.inner.cfg.flush_every.max(1) {
            self.flush()?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        let feed = self.inner.feed.clone();
        let mut empty = Vec::new();
        for (source, ledger) in &mut self.ledgers {
            ledger.flush(feed.as_ref(), source)?;
            if ledger.is_empty() {
                empty.push(source.clone());
            }
        }
        self.state.sources.retain(|s| !empty.contains(s));
        self.notes.save(feed.as_ref())?;
        self.state.save(feed.as_ref())?;
        self.since_flush = 0;
        Ok(())
    }

    // ---------------------------------------------------------------- the user

    /// The user's entity, created on first use.
    fn ensure_me(&mut self) -> Result<EntityId> {
        if let Some(id) = self.state.me.clone()
            && let Some(e) = self.graph().get_entity(&id)?
        {
            self.state.me = Some(e.id.clone());
            return Ok(e.id);
        }
        let mut emails: Vec<String> = self.me_set.iter().cloned().collect();
        emails.sort();
        let mut found = None;
        for e in &emails {
            if let Some(id) = self
                .graph()
                .find_by_identifier(&Identifier::new("email", e.as_str()))?
                && let Some(ent) = self.graph().get_entity(&id)?
                && ent.kind == EntityKind::Person
            {
                found = Some(ent.id);
                break;
            }
        }
        let id = match found {
            Some(id) => {
                self.graph().update_entity(
                    &id,
                    &EntityPatch {
                        attributes: Some(json!({"is_me": true})),
                        ..Default::default()
                    },
                )?;
                id
            }
            None => {
                let mut new = NewEntity::new(
                    EntityKind::Person,
                    self.inner
                        .cfg
                        .me
                        .name
                        .clone()
                        .unwrap_or_else(|| DEFAULT_ME_NAME.into()),
                );
                new.attributes = json!({"is_me": true});
                self.stats.entities_created += 1;
                self.graph().create_entity(&new)?
            }
        };
        self.state.me = Some(id.clone());
        self.touch(&id);
        for e in emails {
            self.add_me_email(&id, &e)?;
        }
        Ok(id)
    }

    fn add_me_email(&mut self, me: &EntityId, email: &str) -> Result<()> {
        let ident = Identifier::new("email", email).normalized();
        match self.graph().find_by_identifier(&ident)? {
            Some(id) => {
                let Some(other) = self.graph().get_entity(&id)? else {
                    return Ok(());
                };
                if &other.id == me || other.kind != EntityKind::Person {
                    return Ok(());
                }
                let splits = self.inner.splits()?;
                if !splits.is_empty() {
                    let live = crate::splits::live(self.graph(), &splits)?;
                    if crate::splits::apart(&live, me, &other.id) {
                        // The user split them apart: the address stays with the other person.
                        return Ok(());
                    }
                }
                // The address is the user's: the person holding it is the user.
                crate::resolve::merge_recorded(
                    self.graph(),
                    me,
                    &other.id,
                    vec!["shared_email".into()],
                    "Holds one of the user's own email addresses.".into(),
                    None,
                )?;
                self.stats.entities_merged += 1;
                self.touch(&other.id);
                self.touch(me);
                self.retract_self_loops(me)?;
            }
            None => match self.graph().add_identifier(me, &ident) {
                Ok(()) | Err(Error::Invalid(_)) => self.touch(me),
                Err(e) => return Err(e),
            },
        }
        Ok(())
    }

    fn retract_self_loops(&mut self, me: &EntityId) -> Result<()> {
        for c in self.graph().claims_for(me, false)? {
            if &c.subject == me
                && c.object == ClaimObject::Entity(me.clone())
                && c.status == ClaimStatus::Active
            {
                self.graph()
                    .set_claim_status(c.id, ClaimStatus::Retracted)?;
                self.stats.claims_retracted += 1;
            }
        }
        Ok(())
    }

    fn learn_me(&mut self, emails: &[String], name: Option<&str>) -> Result<()> {
        let fresh: Vec<String> = emails
            .iter()
            .map(|e| e.to_lowercase())
            .filter(|e| !self.me_set.contains(e))
            .collect();
        if fresh.is_empty() && name.is_none() {
            return Ok(());
        }
        for e in &fresh {
            self.me_set.insert(e.clone());
            self.state.me_emails.push(e.clone());
        }
        let me = self.ensure_me()?;
        for e in &fresh {
            self.add_me_email(&me, e)?;
        }
        if self.inner.cfg.me.name.is_none()
            && let Some(n) = name
            && let Some(ent) = self.graph().get_entity(&me)?
            && ent.name == DEFAULT_ME_NAME
        {
            self.graph().update_entity(
                &me,
                &EntityPatch {
                    name: Some(n.to_owned()),
                    ..Default::default()
                },
            )?;
            self.touch(&me);
        }
        Ok(())
    }

    // ---------------------------------------------------------------- retraction

    fn retract(&mut self, item: &ItemRef) -> Result<Retracted> {
        let affected = self.graph().retract_item(item)?;
        // Merge records citing the item go with it (resolution writes them again if the item
        // comes back and still corroborates the join).
        for id in crate::resolve::forget_item(self.graph(), self.feed(), item)? {
            self.touch(&id);
        }
        self.retracted_any = true;
        let mut out = Retracted::default();
        for id in affected {
            let Some(c) = self.graph().get_claim(id)? else {
                continue;
            };
            self.touch_claim(&c);
            if self.single_valued.contains(&c.predicate) {
                // Its value may have lost its last sighting (or the evidence that ended it).
                self.settle_queue
                    .insert((c.subject.clone(), c.predicate.clone()));
            } else if !c.evidence.is_empty() {
                self.redate.insert(c.id); // its earliest evidence may be gone
            }
            if c.status == ClaimStatus::Retracted && c.evidence.is_empty() {
                out.reuse
                    .insert(claim_key(&c.subject, &c.predicate, &c.object), c.id);
            }
        }
        let before = self.state.pending_links.len();
        self.state.pending_links.retain(|p| &p.item != item);
        if before != self.state.pending_links.len() {
            self.state.links_dirty = true;
        }
        self.notes.remove(item);
        Ok(out)
    }

    /// Settle every single-valued pair the last writes and retractions touched (see
    /// [`Session::settle`]). After a deletion, a pair left without an active value falls back to
    /// the best remaining superseded value (the freshest sighting that has not ended).
    fn restore_superseded(&mut self) -> Result<()> {
        let pairs = std::mem::take(&mut self.settle_queue);
        for (s, p) in pairs {
            self.settle(&s, &p)?;
        }
        self.redate_claims()
    }

    /// Multi-valued claims that lost evidence start at their earliest remaining evidence date
    /// (event-dated ones keep the event's start).
    fn redate_claims(&mut self) -> Result<()> {
        let ids = std::mem::take(&mut self.redate);
        let mut claims = Vec::new();
        for id in ids {
            if let Some(c) = self.graph().get_claim(id)?
                && matches!(c.status, ClaimStatus::Active | ClaimStatus::Superseded)
                && !c.evidence.is_empty()
                && !self.single_valued.contains(&c.predicate)
                && !EVENT_DATED.contains(&c.predicate.as_str())
            {
                claims.push(c);
            }
        }
        let dates = self.dates_of(
            claims
                .iter()
                .flat_map(|c| c.evidence.iter().map(|e| &e.item)),
        )?;
        let mut batch = GraphBatch::new();
        for c in &claims {
            let from = earliest(c, &dates);
            if from.is_some() && from != c.valid_from {
                batch.push(GraphOp::UpdateClaim {
                    claim: c.id,
                    patch: ClaimPatch {
                        valid_from: Some(from),
                        ..Default::default()
                    },
                });
                self.touch_claim(c);
            }
        }
        if !batch.is_empty() {
            self.graph().apply_batch(&batch)?;
        }
        Ok(())
    }

    pub fn process_delete(&mut self, item: &ItemRef) -> Result<()> {
        let r = self.retract(item)?;
        self.stats.claims_retracted += r.reuse.len() as u64;
        self.restore_superseded()?;
        self.ledger(&item.source).remove(&item.key);
        self.since_flush += 1;
        self.stats.items_deleted += 1;
        Ok(())
    }

    // ---------------------------------------------------------------- items

    /// An item date, clamped to [`Session::horizon`].
    fn clamp(&self, d: OffsetDateTime) -> OffsetDateTime {
        d.min(self.horizon)
    }

    /// The (clamped) dates of `items` known to the store, the ones not cached loaded in one feed
    /// call. The result is the caller's own: clearing the cache cannot lose any of it.
    fn dates_of<'i>(&mut self, items: impl IntoIterator<Item = &'i ItemRef>) -> Result<Dates> {
        let mut out = Dates::new();
        let mut missing: Vec<ItemRef> = Vec::new();
        let mut seen: HashSet<&ItemRef> = HashSet::new();
        for i in items {
            if !seen.insert(i) {
                continue;
            }
            match self.dates.get(i) {
                Some(Some(d)) => {
                    out.insert(i.clone(), *d);
                }
                Some(None) => {}
                None => missing.push(i.clone()),
            }
        }
        if missing.is_empty() {
            return Ok(out);
        }
        let found = self.feed().item_dates(&missing)?;
        if self.dates.len() + missing.len() > DATE_CACHE {
            self.dates.clear();
        }
        for i in missing {
            let d = found.get(&i).map(|d| self.clamp(*d));
            if let Some(d) = d {
                out.insert(i.clone(), d);
            }
            self.dates.insert(i, d);
        }
        Ok(out)
    }

    fn item_date(&mut self, item: &ItemRef) -> Result<Option<OffsetDateTime>> {
        Ok(self.dates_of(std::iter::once(item))?.get(item).copied())
    }

    /// Process a changed (or new) item. Returns `false` when it turned out unchanged.
    pub async fn process_upsert(&mut self, w: &Work) -> Result<bool> {
        let Some(item) = self.feed().get_item(&w.item)? else {
            self.process_delete(&w.item)?;
            return Ok(true);
        };
        let fp = fingerprint(&item);
        if let Some(p) = &w.prev
            && p.fingerprint == fp
            && (p.failures >= MAX_FAILURES || (p.failures == 0 && !w.llm_retry))
        {
            let entry = Entry {
                version: w.version.clone(),
                ..p.clone()
            };
            if w.version != p.version {
                self.ledger(&w.item.source).set(&w.item.key, entry);
            }
            self.stats.items_unchanged += 1;
            return Ok(false);
        }
        let prev_failures = w.prev.as_ref().map_or(0, |p| p.failures);
        // LLM attempts count per content version.
        let prev_llm = w
            .prev
            .as_ref()
            .filter(|p| p.fingerprint == fp)
            .map_or(LLM_NONE, |p| p.llm);
        match self.extract_and_apply(&item, prev_llm).await {
            Ok(llm) => {
                self.stats.items_processed += 1;
                self.record(
                    &w.item,
                    Entry {
                        version: w.version.clone(),
                        fingerprint: fp,
                        llm,
                        failures: 0,
                    },
                )?;
            }
            Err(e) => {
                tracing::warn!(item = %w.item.key, source = %w.item.source, error = %e, "knowledge extraction failed");
                self.stats.items_failed += 1;
                self.stats.last_error = Some(truncate_chars(&e.to_string(), 500));
                self.record(
                    &w.item,
                    Entry {
                        version: w.version.clone(),
                        fingerprint: fp,
                        llm: prev_llm,
                        failures: prev_failures + 1,
                    },
                )?;
            }
        }
        Ok(true)
    }

    /// Retract the item's old evidence, extract again, apply. Returns the LLM status.
    async fn extract_and_apply(&mut self, item: &Item, prev_llm: u32) -> Result<u32> {
        let date = self.clamp(crate::extract::item_date(item));
        self.dates.insert(item.item.clone(), Some(date));
        let retracted = self.retract(&item.item)?;
        let chunks = self
            .feed()
            .chunks(&item.item, self.inner.cfg.max_chunks_per_item.max(1))?;
        let label = self.me_label();
        let mut ex = {
            let ctx = crate::extract::ItemCtx {
                item,
                chunks: &chunks,
                me_emails: &self.me_set,
                me_label: &label,
                cfg: &self.inner.cfg,
            };
            crate::extract::deterministic(&ctx)
        };
        trust_note_identifiers(&mut ex);
        if !ex.me_emails.is_empty() || ex.me_name.is_some() {
            let emails = std::mem::take(&mut ex.me_emails);
            let name = ex.me_name.take();
            self.learn_me(&emails, name.as_deref())?;
        }
        let llm = self.llm_step(item, &chunks, &mut ex, prev_llm).await;
        let mut reuse = retracted.reuse;
        if let Err(e) = self.apply(ex, item, date, &mut reuse).await {
            self.discard();
            return Err(e);
        }
        // Also settles the single-valued pairs this item touched or lost evidence of.
        self.commit()?;
        self.stats.claims_retracted += reuse.len() as u64;
        Ok(llm)
    }

    async fn llm_step(
        &mut self,
        item: &Item,
        chunks: &[nugget_store::Chunk],
        ex: &mut Extraction,
        prev_llm: u32,
    ) -> u32 {
        let cfg = &self.inner.cfg.llm;
        let attempts = prev_llm.saturating_sub(LLM_PENDING);
        let junk = item.metadata.get("junk") == Some(&Value::Bool(true));
        let Some(router) = self.inner.router.clone() else {
            return LLM_NONE;
        };
        if !cfg.enabled || junk || chunks.is_empty() {
            return LLM_NONE;
        }
        let any = !router
            .candidates(
                nugget_core::capability::Tier::Synthesis,
                &crate::llm::json_caps(),
            )
            .is_empty();
        let providers = crate::llm::allowed_providers(&router, cfg, item);
        if providers.is_empty() {
            if any {
                self.stats.llm_skipped_privacy += 1;
                return LLM_NONE;
            }
            return LLM_PENDING + attempts; // no provider yet: try again when one is configured
        }
        if self.llm_left == 0 {
            return LLM_PENDING + attempts;
        }
        let me_name = self.inner.cfg.me.name.clone();
        let run = crate::llm::extract(
            &router,
            &providers,
            item,
            chunks,
            cfg,
            me_name.as_deref(),
            self.llm_left,
        )
        .await;
        self.llm_left = self.llm_left.saturating_sub(run.requests);
        self.stats.llm_requests += run.requests as u64;
        self.stats.llm_failed_requests += run.failed_requests as u64;
        self.stats.llm_claims_accepted += run.gate.claims_accepted;
        self.stats.llm_claims_dropped += run.gate.claims_dropped;
        self.stats.llm_quotes_rejected += run.gate.quotes_rejected;
        self.stats.llm_entities_dropped += run.gate.entities_dropped;
        let ok = run.requests > run.failed_requests;
        let mut extra = run.extraction;
        crate::llm::adopt_known_kinds(
            self.inner.graph.as_ref(),
            self.state.me.as_ref(),
            ex,
            &mut extra,
        );
        fold_first_names(ex, &mut extra);
        crate::llm::fold_first_names_in_graph(
            self.inner.graph.as_ref(),
            self.state.me.as_ref(),
            &mut extra,
        );
        ex.absorb(extra);
        if !ok && run.requests > 0 {
            LLM_PENDING + attempts + 1
        } else if run.deferred {
            LLM_PENDING + attempts
        } else {
            LLM_DONE
        }
    }

    // ---------------------------------------------------------------- apply

    async fn apply(
        &mut self,
        ex: Extraction,
        item: &Item,
        date: OffsetDateTime,
        reuse: &mut HashMap<String, ClaimId>,
    ) -> Result<()> {
        let mut ids: Vec<EntityId> = Vec::with_capacity(ex.entities.len());
        let splits = self.inner.splits()?;
        let feed: &dyn ItemFeed = self.inner.feed.as_ref();
        let item_ctx = crate::resolve::ItemContext::new(item, &ex, Some(feed));
        for spec in &ex.entities {
            let me = self.state.me.clone();
            let resolver = Resolver::new(
                self.inner.graph.as_ref(),
                &self.inner.cfg,
                self.inner.router.as_deref(),
                &self.inner.emb_cache,
                me,
            )
            .with_splits(&splits, Some(&item.item))
            .with_context(&item_ctx);
            let r = resolver.resolve(spec).await?;
            if r.created {
                self.stats.entities_created += 1;
            }
            self.stats.entities_merged += r.merged.len() as u64;
            for t in &r.touched {
                self.touch(t);
            }
            if !r.merged.is_empty() {
                self.reconcile_single_valued(&r.id)?;
            }
            for m in &spec.mentions {
                self.batch.push(GraphOp::AddMention {
                    mention: Mention {
                        entity: r.id.clone(),
                        item: item.item.clone(),
                        chunk_id: m.chunk_id,
                        surface: truncate_chars(&m.surface, 200),
                        confidence: m.confidence.clamp(0.0, 1.0),
                    },
                });
                self.stats.mentions_added += 1;
            }
            // Which item supplied each alias and identifier (scoped MCP clients see only the
            // ones from sources they may read).
            let mut details: Vec<EntityDetail> = spec
                .identifiers
                .iter()
                .cloned()
                .map(EntityDetail::Identifier)
                .collect();
            if !spec.weak_name {
                details.push(EntityDetail::Alias(spec.name.clone()));
            }
            details.extend(spec.aliases.iter().cloned().map(EntityDetail::Alias));
            for detail in details {
                self.batch.push(GraphOp::AddDetailSource {
                    entity: r.id.clone(),
                    detail,
                    item: item.item.clone(),
                });
            }
            self.touch(&r.id);
            ids.push(r.id);
        }
        let mut me: Option<EntityId> = None;
        let mut get = |s: &mut Self, r: ERef| -> Result<EntityId> {
            Ok(match r {
                ERef::Local(i) => ids[i].clone(),
                ERef::Me => match &me {
                    Some(m) => m.clone(),
                    None => {
                        let m = s.ensure_me()?;
                        me = Some(m.clone());
                        m
                    }
                },
            })
        };
        for c in ex.claims {
            let subject = get(self, c.subject)?;
            let object = match c.object {
                OSpec::Entity(r) => ClaimObject::Entity(get(self, r)?),
                OSpec::Literal(l) => ClaimObject::Literal(l),
            };
            if object == ClaimObject::Entity(subject.clone()) {
                continue;
            }
            let new = NewClaim {
                subject,
                predicate: c.predicate,
                object,
                text: truncate_chars(&c.text, 500),
                confidence: c.confidence.clamp(0.0, 1.0),
                status: ClaimStatus::Active,
                evidence: vec![c.evidence],
                valid_from: c.valid_from,
                valid_to: None,
            };
            self.add_claim(new, date, reuse)?;
        }
        for l in ex.links {
            let from = get(self, l.from)?;
            let targets = self.link_targets(&l.target)?;
            if targets.is_empty() {
                self.state.links_dirty = true;
                self.state.pending_links.push(PendingLink {
                    item: item.item.clone(),
                    from,
                    target: l.target,
                    chunk_id: l.evidence.chunk_id,
                    quote: l.evidence.quote,
                    confidence: l.confidence,
                });
                let excess = self
                    .state
                    .pending_links
                    .len()
                    .saturating_sub(MAX_PENDING_LINKS);
                self.state.pending_links.drain(..excess);
                continue;
            }
            for t in targets {
                self.link_claim(&from, &t, &l.evidence, l.confidence, date, reuse)?;
            }
        }
        if !ex.note_names.is_empty() {
            let mut subjects = Vec::new();
            for r in ex.note_subjects {
                subjects.push(get(self, r)?);
            }
            self.notes.set(&item.item, &ex.note_names, subjects);
        }
        Ok(())
    }

    fn link_claim(
        &mut self,
        from: &EntityId,
        to: &EntityId,
        evidence: &Evidence,
        confidence: f32,
        date: OffsetDateTime,
        reuse: &mut HashMap<String, ClaimId>,
    ) -> Result<bool> {
        let (Some(f), Some(t)) = (self.graph().get_entity(from)?, self.graph().get_entity(to)?)
        else {
            return Ok(false);
        };
        if f.id == t.id {
            return Ok(false);
        }
        self.add_claim(
            NewClaim {
                subject: f.id,
                predicate: "references".into(),
                object: ClaimObject::Entity(t.id),
                text: format!("{} references {}.", f.name, t.name),
                confidence,
                status: ClaimStatus::Active,
                evidence: vec![evidence.clone()],
                valid_from: None,
                valid_to: None,
            },
            date,
            reuse,
        )?;
        self.stats.links_resolved += 1;
        Ok(true)
    }

    /// A wikilink target: the subjects of the one note with that name, else the one entity of
    /// that name (preferring projects, then topics, then other kinds).
    fn link_targets(&mut self, target: &str) -> Result<Vec<EntityId>> {
        let from_notes = self.notes.lookup(target);
        if !from_notes.is_empty() {
            return Ok(from_notes);
        }
        let mut by_kind: HashMap<EntityKind, Vec<EntityId>> = HashMap::new();
        for id in self.graph().find_by_name(target, None)? {
            if let Some(e) = self.graph().get_entity(&id)?
                && !by_kind.get(&e.kind).is_some_and(|v| v.contains(&e.id))
            {
                by_kind.entry(e.kind).or_default().push(e.id);
            }
        }
        for kind in [
            EntityKind::Project,
            EntityKind::Topic,
            EntityKind::Person,
            EntityKind::Organization,
            EntityKind::Place,
            EntityKind::Event,
        ] {
            if let Some(v) = by_kind.get(&kind) {
                return Ok(if v.len() == 1 { v.clone() } else { Vec::new() });
            }
        }
        Ok(Vec::new())
    }

    /// Retry unresolved wikilinks (their targets may have appeared this run).
    pub fn resolve_pending_links(&mut self) -> Result<()> {
        let pending = std::mem::take(&mut self.state.pending_links);
        let before = pending.len();
        let mut keep = Vec::with_capacity(pending.len());
        let mut reuse = HashMap::new();
        for p in pending {
            let targets = self.link_targets(&p.target)?;
            if targets.is_empty() {
                keep.push(p);
                continue;
            }
            let date = self
                .item_date(&p.item)?
                .unwrap_or_else(OffsetDateTime::now_utc);
            let ev = Evidence {
                item: p.item.clone(),
                chunk_id: p.chunk_id,
                quote: p.quote.clone(),
            };
            let mut any = false;
            for t in targets {
                any |= self.link_claim(&p.from, &t, &ev, p.confidence, date, &mut reuse)?;
            }
            if !any {
                keep.push(p);
            }
        }
        if keep.len() != before {
            self.state.links_dirty = true;
        }
        self.state.pending_links = keep;
        self.commit()
    }

    // ---------------------------------------------------------------- claims

    /// Apply the pending batch (one transaction for the item's writes), then re-aggregate the
    /// confidence of the claims it wrote, date them, and settle the single-valued pairs it
    /// touched.
    pub fn commit(&mut self) -> Result<()> {
        let batch = std::mem::take(&mut self.batch);
        self.queued.clear();
        let written = std::mem::take(&mut self.agg);
        if !batch.is_empty() {
            let results = self.graph().apply_batch(&batch)?;
            let mut seen = HashSet::new();
            let mut follow = GraphBatch::new();
            for w in written {
                let id = match w.target {
                    Queued::Id(id) => Some(id),
                    Queued::Op(i) => results.get(i).and_then(|r| r.claim()),
                };
                if let Some(id) = id
                    && seen.insert(id)
                    && let Some(patch) = self.follow_up(id, &w)?
                {
                    follow.push(GraphOp::UpdateClaim { claim: id, patch });
                }
            }
            if !follow.is_empty() {
                self.graph().apply_batch(&follow)?;
            }
        }
        self.restore_superseded()
    }

    /// Drop the pending batch (the item failed). Pairs queued for settling stay queued: the
    /// item's retraction happened.
    fn discard(&mut self) {
        self.batch = GraphBatch::new();
        self.queued.clear();
        self.agg.clear();
    }

    fn add_evidence_op(&mut self, claim: ClaimId, evidence: &[Evidence]) {
        for ev in evidence {
            self.batch.push(GraphOp::AddEvidence {
                claim,
                evidence: ev.clone(),
            });
        }
    }

    /// Add a claim with its lifecycle: revive a just-retracted twin, date it, queue its
    /// single-valued pair for settling ([`Session::settle`]), and aggregate confidence over
    /// evidence. The writes go to the item's batch.
    fn add_claim(
        &mut self,
        mut new: NewClaim,
        date: OffsetDateTime,
        reuse: &mut HashMap<String, ClaimId>,
    ) -> Result<()> {
        for ev in &mut new.evidence {
            if let Some(q) = ev.quote.as_mut() {
                *q = truncate_chars(q, MAX_QUOTE_CHARS);
            }
        }
        // Valid from the item's date unless the extractor knows better (an event's start).
        let explicit = new.valid_from;
        new.valid_from = Some(explicit.unwrap_or(date));
        let key = claim_key(&new.subject, &new.predicate, &new.object);
        self.touched.insert(new.subject.clone());
        if let ClaimObject::Entity(o) = &new.object {
            self.touched.insert(o.clone());
        }
        // The same triple again in this item: one claim, more evidence.
        if let Some(q) = self.queued.get(&key).copied() {
            match q {
                Queued::Op(i) => {
                    if let Some(GraphOp::AddClaim { claim }) = self.batch.ops.get_mut(i) {
                        for ev in &new.evidence {
                            if !claim.evidence.contains(ev) {
                                claim.evidence.push(ev.clone());
                            }
                        }
                        claim.confidence = claim.confidence.max(new.confidence);
                    }
                }
                Queued::Id(id) => self.add_evidence_op(id, &new.evidence),
            }
            return Ok(());
        }
        let single = self.single_valued.contains(&new.predicate);
        let target = if single {
            self.settle_queue
                .insert((new.subject.clone(), new.predicate.clone()));
            self.add_single_valued(&new, reuse.remove(&key))?
        } else if let Some(id) = reuse.remove(&key) {
            self.add_evidence_op(id, &new.evidence);
            Queued::Id(id)
        } else {
            self.stats.claims_added += 1;
            Queued::Op(self.batch.push(GraphOp::AddClaim { claim: new.clone() }))
        };
        self.queued.insert(key, target);
        self.agg.push(Written {
            target,
            base: new.confidence,
            date,
            explicit,
            single,
        });
        Ok(())
    }

    /// Write a single-valued claim: as evidence of the claim already holding that value (active,
    /// user-asserted, rejected, superseded, or about to be revived), else as a new active claim.
    /// Which value is current, and the intervals, are decided after the batch is applied
    /// ([`Session::settle`]), from all the values' evidence.
    fn add_single_valued(&mut self, new: &NewClaim, revive: Option<ClaimId>) -> Result<Queued> {
        let same = self
            .graph()
            .claims_for(&new.subject, true)?
            .into_iter()
            .filter(|c| {
                c.subject == new.subject && c.predicate == new.predicate && c.object == new.object
            })
            .filter(|c| c.status != ClaimStatus::Retracted || Some(c.id) == revive)
            .min_by_key(|c| {
                let rank = match c.status {
                    ClaimStatus::Rejected => 0,
                    ClaimStatus::Active | ClaimStatus::UserAsserted => 1,
                    ClaimStatus::Superseded => 2,
                    ClaimStatus::Retracted => 3,
                };
                (rank, c.id)
            });
        Ok(match same {
            // Adding evidence revives a retracted claim; a rejected one stays rejected.
            Some(c) => {
                self.add_evidence_op(c.id, &new.evidence);
                Queued::Id(c.id)
            }
            None => {
                self.stats.claims_added += 1;
                Queued::Op(self.batch.push(GraphOp::AddClaim { claim: new.clone() }))
            }
        })
    }

    /// After a merge, the entity (and the subjects of claims about it) may hold several live
    /// values of a single-valued predicate that lived on different entities before: settle each.
    fn reconcile_single_valued(&mut self, id: &EntityId) -> Result<()> {
        let mut subjects: Vec<EntityId> = vec![id.clone()];
        for c in self.graph().claims_for(id, false)? {
            if !subjects.contains(&c.subject) {
                subjects.push(c.subject.clone());
            }
        }
        for subject in subjects {
            let preds: BTreeSet<String> = self
                .graph()
                .claims_for(&subject, true)?
                .into_iter()
                .filter(|c| c.subject == subject && self.single_valued.contains(&c.predicate))
                .map(|c| c.predicate)
                .collect();
            for p in preds {
                self.settle(&subject, &p)?;
            }
        }
        Ok(())
    }

    /// Claims (any status) of one subject and predicate.
    fn pair_claims(&self, subject: &EntityId, predicate: &str) -> Result<Vec<Claim>> {
        Ok(self
            .graph()
            .claims_for(subject, true)?
            .into_iter()
            .filter(|c| &c.subject == subject && c.predicate == predicate)
            .collect())
    }

    /// What a claim's object is called (for temporal cues in quotes): an entity's name and
    /// aliases, or the literal.
    fn object_names(&self, object: &ClaimObject) -> Result<Vec<String>> {
        Ok(match object {
            ClaimObject::Literal(l) => vec![l.clone()],
            ClaimObject::Entity(id) => match self.graph().get_entity(id)? {
                Some(e) => std::iter::once(e.name).chain(e.aliases).collect(),
                None => Vec::new(),
            },
        })
    }

    /// Settle one single-valued predicate of one subject from scratch: which value is current,
    /// which are history, and every interval. A deterministic function of the claims' evidence
    /// (item dates, clamped to the near future, and quotes), so the order items were processed in
    /// does not matter.
    ///
    /// Claims are grouped by value (object). A value the user rejected takes no part (an active
    /// twin of it is superseded). For each other value: its *last sighting* is its newest
    /// evidence date whose quote does not say the fact ended ([`temporal::Cue::Ended`]); it has
    /// *ended* when its newest ended-quote is at least as recent as that (so "my last day at X"
    /// ends X, and only a later sighting brings it back).
    ///
    /// The current value is a user-asserted one if any (the newest assertion); otherwise the
    /// value with the latest sighting among those not ended, ties broken by a start cue at that
    /// date ("joined", "now", "moved to"), then by the latest place in the text (chunk, quote
    /// position in the chunk, object position in the quote: "from X to Y"), then by the
    /// object's name. Its claims are active; every other value's active claims are superseded.
    /// With every value ended, none is current. (A user assertion stays current until the user
    /// changes it, whatever newer evidence says: a known limit.)
    ///
    /// Intervals (of active and superseded claims with evidence): `valid_from` is the earliest
    /// evidence date (an explicit start of [`EXPLICIT_START`] predicates is kept); an active
    /// claim has no `valid_to`. A superseded value is valid until the first date another value is
    /// seen at or after its last sighting; an ended one until its last ended-quote, or that first
    /// other sighting when earlier; with neither, `valid_to` stays open.
    ///
    /// Reads the pair with one `claims_for` (the graph has no subject-and-predicate lookup) and
    /// re-reads only the claims an activation may have folded.
    fn settle(&mut self, subject: &EntityId, predicate: &str) -> Result<()> {
        let Some(ent) = self.graph().get_entity(subject)? else {
            return Ok(());
        };
        let subject = ent.id;
        let claims = self.pair_claims(&subject, predicate)?;
        if claims.is_empty() {
            return Ok(());
        }
        let dates = self.dates_of(
            claims
                .iter()
                .flat_map(|c| c.evidence.iter().map(|e| &e.item)),
        )?;
        let mut groups: BTreeMap<String, ValueGroup> = BTreeMap::new();
        for c in &claims {
            let key = serde_json::to_string(&c.object).unwrap_or_default();
            if !groups.contains_key(&key) {
                let names = self.object_names(&c.object)?;
                groups.insert(key.clone(), ValueGroup::new(names));
            }
            if let Some(g) = groups.get_mut(&key) {
                g.add(c, &dates);
            }
        }
        let winner = self.winner(&mut groups, &dates)?;

        // Statuses; `current` holds the claims as they are afterwards.
        let mut current: Vec<Claim> = Vec::with_capacity(claims.len());
        for (key, g) in &groups {
            let win = winner.as_ref() == Some(key);
            if g.rejected || (g.live && !win) {
                for c in &g.claims {
                    let mut c = c.clone();
                    if c.status == ClaimStatus::Active {
                        self.graph()
                            .set_claim_status(c.id, ClaimStatus::Superseded)?;
                        self.stats.claims_superseded += 1;
                        self.touch_claim(&c);
                        c.status = ClaimStatus::Superseded;
                    }
                    current.push(c);
                }
                continue;
            }
            let revive: Vec<&Claim> = g
                .claims
                .iter()
                .filter(|c| c.status == ClaimStatus::Superseded && !c.evidence.is_empty())
                .collect();
            if !win || g.user.is_some() || revive.is_empty() {
                current.extend(g.claims.iter().cloned());
                continue;
            }
            for c in &revive {
                // Folds into a live twin of the same value, if any.
                self.graph().set_claim_status(c.id, ClaimStatus::Active)?;
                self.touch_claim(c);
            }
            if !g.claims.iter().any(|c| c.status == ClaimStatus::Active) {
                self.stats.claims_reactivated += 1;
            }
            for c in &g.claims {
                if let Some(now) = self.graph().get_claim(c.id)?
                    && !current.iter().any(|x| x.id == now.id)
                {
                    current.push(now);
                }
            }
        }

        // Intervals.
        let explicit_ok = EXPLICIT_START.contains(&predicate);
        for c in &current {
            if !matches!(c.status, ClaimStatus::Active | ClaimStatus::Superseded)
                || c.evidence.is_empty()
            {
                continue;
            }
            let key = serde_json::to_string(&c.object).unwrap_or_default();
            let Some(g) = groups.get(&key).filter(|g| !g.rejected) else {
                continue;
            };
            let explicit = explicit_ok
                && c.valid_from
                    .is_some_and(|v| !c.evidence.iter().any(|e| dates.get(&e.item) == Some(&v)));
            let from = if explicit {
                c.valid_from
            } else {
                earliest(c, &dates).or(c.valid_from)
            };
            let next_other = g.affirmed.and_then(|last| {
                groups
                    .iter()
                    .filter(|(k, o)| **k != key && !o.rejected)
                    .flat_map(|(_, o)| o.sightings.iter().copied())
                    .filter(|d| *d >= last)
                    .min()
            });
            let to = if c.status == ClaimStatus::Active {
                None
            } else if g.ended() {
                match (g.ended, next_other) {
                    (Some(e), Some(n)) => Some(e.min(n)),
                    (e, _) => e,
                }
            } else {
                next_other
            };
            let mut patch = ClaimPatch::default();
            if c.valid_from != from {
                patch.valid_from = Some(from);
            }
            if c.valid_to != to {
                patch.valid_to = Some(to);
            }
            if patch != ClaimPatch::default() {
                self.graph().update_claim(c.id, &patch)?;
                self.touch_claim(c);
            }
        }
        Ok(())
    }

    /// The current value of a settled pair (see [`Session::settle`]).
    fn winner(
        &mut self,
        groups: &mut BTreeMap<String, ValueGroup>,
        dates: &Dates,
    ) -> Result<Option<String>> {
        let cands: Vec<(&String, &ValueGroup)> = groups
            .iter()
            .filter(|(_, g)| g.live && !g.rejected && !g.ended())
            .collect();
        if cands.iter().any(|(_, g)| g.user.is_some()) {
            return Ok(cands
                .iter()
                .filter(|(_, g)| g.user.is_some())
                .max_by(|a, b| (a.1.user, &a.1.name, a.0).cmp(&(b.1.user, &b.1.name, b.0)))
                .map(|(k, _)| (*k).clone()));
        }
        let Some(best) = cands.iter().map(|(_, g)| (g.affirmed, g.started())).max() else {
            return Ok(None);
        };
        let tied: Vec<String> = cands
            .into_iter()
            .filter(|(_, g)| (g.affirmed, g.started()) == best)
            .map(|(k, _)| k.clone())
            .collect();
        if tied.len() == 1 {
            return Ok(tied.into_iter().next());
        }
        let mut chunks: HashMap<ItemRef, Vec<nugget_store::Chunk>> = HashMap::new();
        let mut scored = Vec::with_capacity(tied.len());
        for k in tied {
            let Some(g) = groups.get_mut(&k) else {
                continue;
            };
            let pos = self.position(g, dates, &mut chunks)?;
            scored.push((pos, g.name.clone(), k));
        }
        Ok(scored.into_iter().max().map(|(_, _, k)| k))
    }

    /// Where a value's latest affirming evidence is in its text: (chunk, quote offset in the
    /// chunk, object offset in the quote), the latest of its quotes at its last sighting.
    fn position(
        &self,
        g: &mut ValueGroup,
        dates: &Dates,
        chunks: &mut HashMap<ItemRef, Vec<nugget_store::Chunk>>,
    ) -> Result<(i64, i64, i64)> {
        // A maximum: the order the evidence is visited in does not matter.
        let at: Vec<Evidence> = g
            .claims
            .iter()
            .flat_map(|c| c.evidence.iter())
            .filter(|e| e.quote.is_some() && dates.get(&e.item).copied() == g.affirmed)
            .cloned()
            .collect();
        let mut best = (-1, -1, -1);
        for e in at {
            let Some(q) = e.quote.as_deref() else {
                continue;
            };
            if g.cue(q) == Cue::Ended {
                continue;
            }
            let chunk = e.chunk_id.unwrap_or(-1);
            if !chunks.contains_key(&e.item) {
                let cs = self
                    .feed()
                    .chunks(&e.item, self.inner.cfg.max_chunks_per_item.max(1))?;
                chunks.insert(e.item.clone(), cs);
            }
            let offset = chunks[&e.item]
                .iter()
                .find(|ch| Some(ch.id) == e.chunk_id)
                .and_then(|ch| {
                    normalize_ws(&ch.text)
                        .to_lowercase()
                        .find(&normalize_ws(q).to_lowercase())
                })
                .map_or(-1, |o| i64::try_from(o).unwrap_or(i64::MAX));
            let names: Vec<&str> = g.names.iter().map(String::as_str).collect();
            let word = temporal::object_position(q, &names)
                .map_or(-1, |w| i64::try_from(w).unwrap_or(i64::MAX));
            best = best.max((chunk, offset, word));
        }
        Ok(best)
    }

    /// One-time migration of a graph written before validity intervals (see
    /// `state::TEMPORAL_VERSION`), without the LLM: claims with evidence but no `valid_from`
    /// start at their earliest evidence date, and every single-valued pair is settled
    /// ([`Session::settle`]), which supersedes all but one of several active values and dates
    /// superseded ones. Entities are visited in id order; the last one done is kept in
    /// `State::temporal_cursor` and the walk stops at the deadline (after at least one entity),
    /// so a cut or killed run resumes where it stopped. Idempotent. Returns whether it finished.
    pub fn backfill_validity(&mut self, deadline: Option<Instant>) -> Result<bool> {
        let mut ids: Vec<EntityId> = self
            .graph()
            .list_entities(&EntityQuery::default())?
            .into_iter()
            .map(|e| e.id)
            .collect();
        ids.sort();
        if let Some(cursor) = &self.state.temporal_cursor {
            ids.retain(|i| i > cursor);
        }
        let mut undated: Vec<Claim> = Vec::new();
        let mut done_in_page = 0usize;
        let total = ids.len();
        for (n, id) in ids.into_iter().enumerate() {
            let mut pairs: BTreeSet<String> = BTreeSet::new();
            for c in self.graph().claims_for(&id, true)? {
                if c.subject != id {
                    continue;
                }
                if self.single_valued.contains(&c.predicate) {
                    pairs.insert(c.predicate.clone());
                } else if c.valid_from.is_none()
                    && !c.evidence.is_empty()
                    && matches!(c.status, ClaimStatus::Active | ClaimStatus::Superseded)
                {
                    undated.push(c);
                }
            }
            for p in pairs {
                self.settle(&id, &p)?;
            }
            done_in_page += 1;
            let stop = self.out_of_time(deadline);
            if stop || done_in_page >= BACKFILL_PAGE || n + 1 == total {
                self.date_undated(std::mem::take(&mut undated))?;
                self.state.temporal_cursor = Some(id);
                done_in_page = 0;
            }
            if stop && n + 1 < total {
                return Ok(false);
            }
        }
        self.state.temporal_cursor = None;
        Ok(true)
    }

    /// Set `valid_from` of claims that have none to their earliest evidence date (one batch).
    fn date_undated(&mut self, claims: Vec<Claim>) -> Result<()> {
        if claims.is_empty() {
            return Ok(());
        }
        let dates = self.dates_of(
            claims
                .iter()
                .flat_map(|c| c.evidence.iter().map(|e| &e.item)),
        )?;
        let mut batch = GraphBatch::new();
        for c in &claims {
            if let Some(from) = earliest(c, &dates) {
                batch.push(GraphOp::UpdateClaim {
                    claim: c.id,
                    patch: ClaimPatch {
                        valid_from: Some(Some(from)),
                        ..Default::default()
                    },
                });
                self.touch_claim(c);
            }
        }
        if !batch.is_empty() {
            self.graph().apply_batch(&batch)?;
        }
        Ok(())
    }

    /// The follow-up patch of a claim the batch wrote: confidence raised to the noisy-OR of its
    /// distinct evidence items (if higher), and its start lowered to the item's date when that is
    /// earlier (multi-valued predicates; an explicit start, such as an event's, is kept as given;
    /// single-valued ones are dated by [`Session::settle`]).
    fn follow_up(&self, id: ClaimId, w: &Written) -> Result<Option<ClaimPatch>> {
        let Some(c) = self.graph().get_claim(id)? else {
            return Ok(None);
        };
        let mut patch = ClaimPatch::default();
        if c.status == ClaimStatus::Active {
            let items: HashSet<&ItemRef> = c.evidence.iter().map(|e| &e.item).collect();
            if items.len() >= 2 {
                let target = aggregate(w.base, items.len());
                if target > c.confidence + 0.005 {
                    patch.confidence = Some(target);
                }
            }
        }
        if !w.single && !c.status.is_user_decision() {
            let from = match w.explicit {
                Some(t) => (c.valid_from != Some(t)).then_some(t),
                None if EVENT_DATED.contains(&c.predicate.as_str()) => {
                    c.valid_from.is_none().then_some(w.date)
                }
                None => c.valid_from.is_none_or(|v| v > w.date).then_some(w.date),
            };
            if let Some(t) = from {
                patch.valid_from = Some(Some(t));
            }
        }
        Ok((patch != ClaimPatch::default()).then_some(patch))
    }

    pub fn out_of_time(&self, deadline: Option<Instant>) -> bool {
        deadline.is_some_and(|d| Instant::now() >= d)
    }
}

/// A typed note is its entity's own record, written by the user: the identifiers it lists
/// (several addresses of one person, say) belong together, like a contact card's.
fn trust_note_identifiers(ex: &mut Extraction) {
    if ex.note_names.is_empty() {
        return;
    }
    for r in &ex.note_subjects {
        if let ERef::Local(i) = *r
            && let Some(e) = ex.entities.get_mut(i)
            && matches!(e.kind, EntityKind::Person | EntityKind::Organization)
        {
            e.trusted_identifiers = true;
        }
    }
}

impl Session<'_> {
    /// End of a run: place the short-form people ("Maggie", "Sam") whose corroboration this
    /// run may have supplied (see `Resolver::settle_pending`), so the graph does not depend on
    /// the order items were processed in.
    pub fn settle_short_names(&mut self) -> Result<()> {
        let splits = self.inner.splits()?;
        let resolver = Resolver::new(
            self.inner.graph.as_ref(),
            &self.inner.cfg,
            self.inner.router.as_deref(),
            &self.inner.emb_cache,
            self.state.me.clone(),
        )
        .with_splits(&splits, None)
        .with_feed(self.inner.feed.as_ref());
        let merges = resolver.settle_pending(self.inner.feed.as_ref(), &self.touched)?;
        for (into, from) in merges {
            self.stats.entities_merged += 1;
            self.touch(&from);
            self.touch(&into);
            self.reconcile_single_valued(&into)?;
        }
        Ok(())
    }
}

/// A person the model named by a short form only ("Maggie", "M. Thistlewood", "Maggie T") is
/// the one person on the same item whose name it may be short for ([`crate::names::compatible`]:
/// first name, nickname, initials), when there is exactly one: the occurrence takes that
/// person's name and identifiers, so both resolve together, and notes why (a merge record on
/// the entity). Being on the same item is the corroboration; two candidates (two Sams) fold
/// nothing.
fn fold_first_names(base: &Extraction, extra: &mut Extraction) {
    for i in 0..extra.entities.len() {
        let e = &extra.entities[i];
        if e.kind != EntityKind::Person
            || e.weak_name
            || !e.identifiers.is_empty()
            || !crate::names::is_short_form(&e.name)
        {
            continue;
        }
        let own = normalize_name(&e.name);
        let shorts: Vec<&String> = std::iter::once(&e.name).chain(&e.aliases).collect();
        // Candidates by display name, de-duplicated by normalised name.
        let mut cands: Vec<(String, &crate::spec::EntitySpec, Vec<&'static str>)> = Vec::new();
        let others = base.entities.iter().chain(
            extra
                .entities
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, x)| x),
        );
        for c in others {
            // A target is a full name, or a short one with an address of its own (a header).
            if c.kind != EntityKind::Person
                || c.weak_name
                || (c.identifiers.is_empty() && crate::names::is_short_form(&c.name))
            {
                continue;
            }
            let key = normalize_name(&c.name);
            if key == own || cands.iter().any(|(k, _, _)| *k == key) {
                continue;
            }
            let mut reasons: Vec<&'static str> = Vec::new();
            for full in std::iter::once(&c.name).chain(&c.aliases) {
                for s in &shorts {
                    if let Some(m) = crate::names::compatible(s, full) {
                        for r in m.reasons {
                            if !reasons.contains(&r) {
                                reasons.push(r);
                            }
                        }
                    }
                }
            }
            if !reasons.is_empty() {
                cands.push((key, c, reasons));
            }
        }
        let [(_, target, reasons)] = &cands[..] else {
            continue;
        };
        let (name, identifiers) = (target.name.clone(), target.identifiers.clone());
        let mut reasons: Vec<String> = reasons.iter().map(|r| (*r).to_owned()).collect();
        reasons.push("same_item".into());
        let e = &mut extra.entities[i];
        let short = std::mem::replace(&mut e.name, name.clone());
        let detail = crate::resolve::describe(&reasons);
        e.folds.push(crate::spec::NameFold {
            name: short.clone(),
            reasons,
            detail,
        });
        if !e.aliases.contains(&short) {
            e.aliases.push(short);
        }
        e.identifiers = identifiers;
    }
}

#[cfg(test)]
mod tests {
    use super::{aggregate, fold_first_names, trust_note_identifiers};
    use crate::spec::{EntitySpec, Extraction};
    use nugget_graph::{EntityKind, Identifier};

    fn header(name: &str, email: &str) -> EntitySpec {
        EntitySpec::new(EntityKind::Person, name).ident("email", email)
    }

    #[test]
    fn a_first_name_is_the_one_person_on_the_item_it_starts() {
        let mut base = Extraction::default();
        base.entity(header("Maggie T", "maggie.t@mail.test"));
        base.entity(header("Robin Ashgrove", "robin@ashgrove.test"));
        let mut extra = Extraction::default();
        extra.entity(EntitySpec::new(EntityKind::Person, "Maggie"));
        extra.entity(EntitySpec::new(EntityKind::Person, "Unknown"));
        fold_first_names(&base, &mut extra);
        let m = &extra.entities[0];
        assert_eq!(m.name, "Maggie T");
        assert_eq!(m.aliases, vec!["Maggie".to_string()]);
        assert_eq!(
            m.identifiers,
            vec![Identifier::new("email", "maggie.t@mail.test")]
        );
        assert_eq!(extra.entities[1].name, "Unknown");
        // Absorbed, both are one occurrence.
        base.absorb(extra);
        assert_eq!(
            base.entities
                .iter()
                .filter(|e| e.name == "Maggie T")
                .count(),
            1
        );
    }

    #[test]
    fn a_shared_first_name_stays_apart() {
        let mut base = Extraction::default();
        base.entity(header("Sam Okafor", "sam.okafor@ostrander.test"));
        base.entity(header("Sam Whitlock", "sam@marrowbank.test"));
        let mut extra = Extraction::default();
        extra.entity(EntitySpec::new(EntityKind::Person, "Sam"));
        fold_first_names(&base, &mut extra);
        assert_eq!(extra.entities[0].name, "Sam");
        assert!(extra.entities[0].identifiers.is_empty());
        assert!(extra.entities[0].folds.is_empty());
    }

    #[test]
    fn nicknames_and_initials_fold_into_the_one_person_on_the_item() {
        let mut base = Extraction::default();
        base.entity(header(
            "Margaret Thistlewood",
            "margaret.thistlewood@cindergate.test",
        ));
        base.entity(header("Mark Ellery", "mark@ellery.test"));
        let mut extra = Extraction::default();
        extra.entity(EntitySpec::new(EntityKind::Person, "Maggie"));
        extra.entity(EntitySpec::new(EntityKind::Person, "M. Thistlewood"));
        extra.entity(EntitySpec::new(EntityKind::Person, "Maggie T"));
        // An initial alone fits both Margaret and Mark: not folded.
        extra.entity(EntitySpec::new(EntityKind::Person, "M."));
        // "Mark" is not a form of "Margaret".
        extra.entity(EntitySpec::new(EntityKind::Person, "Marky"));
        fold_first_names(&base, &mut extra);
        let reasons: Vec<Vec<String>> = extra.entities[..3]
            .iter()
            .map(|e| {
                assert_eq!(e.name, "Margaret Thistlewood");
                assert_eq!(
                    e.identifiers,
                    vec![Identifier::new(
                        "email",
                        "margaret.thistlewood@cindergate.test"
                    )]
                );
                assert_eq!(e.folds.len(), 1);
                e.folds[0].reasons.clone()
            })
            .collect();
        assert_eq!(
            reasons,
            [
                vec!["nickname", "same_item"],
                vec!["initial", "same_item"],
                vec!["nickname", "initial", "same_item"],
            ]
        );
        assert_eq!(extra.entities[3].name, "M.");
        assert_eq!(extra.entities[4].name, "Marky");
        // The folds travel with the occurrence when it is absorbed into the header's.
        base.absorb(extra);
        let m = &base.entities[0];
        assert_eq!(m.folds.len(), 3);
        assert!(m.aliases.contains(&"Maggie".to_string()));
    }

    #[test]
    fn a_typed_notes_own_identifiers_are_trusted() {
        let mut ex = Extraction::default();
        let me = ex
            .entity(header("Priya Castellane", "priya@old.test").ident("email", "priya@new.test"));
        let topic = ex.entity(EntitySpec::new(EntityKind::Topic, "data").ident("tag", "data"));
        ex.note_subjects = vec![me, topic];
        trust_note_identifiers(&mut ex);
        assert!(!ex.entities[0].trusted_identifiers, "not a note");
        ex.note_names = vec!["Priya Castellane".into()];
        trust_note_identifiers(&mut ex);
        assert!(ex.entities[0].trusted_identifiers);
        assert!(
            !ex.entities[1].trusted_identifiers,
            "only people and organisations"
        );
    }

    #[test]
    fn noisy_or() {
        assert!((aggregate(0.5, 1) - 0.5).abs() < 1e-6);
        assert!((aggregate(0.5, 2) - 0.75).abs() < 1e-6);
        assert!(aggregate(0.9, 40) <= 0.99);
        assert!(aggregate(0.995, 3) <= 0.995 + 1e-6);
    }
}

#[cfg(test)]
mod temporal_tests {
    use std::sync::Arc;

    use nugget_core::ids::{ItemKey, ItemRef, SourceId};
    use nugget_graph::{Claim, ClaimObject, ClaimStatus, Evidence, MemGraph};
    use nugget_store::{NewChunk, NewItem, Store, content_hash};
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;

    fn at(s: &str) -> OffsetDateTime {
        OffsetDateTime::parse(s, &Rfc3339).unwrap()
    }

    use super::{CUE_SCAN, DATE_CACHE, Dates, Session, ValueGroup};
    use crate::state::{Notes, State};
    use crate::{Knowledge, KnowledgeConfig};

    fn item(key: &str) -> ItemRef {
        ItemRef {
            source: SourceId::new("notes"),
            key: ItemKey::new(key),
        }
    }

    fn put(store: &Store, key: &str, at: OffsetDateTime) {
        let it = NewItem::new("notes", key, content_hash(key)).with_modified_at(at);
        store
            .upsert_item_with_chunks(&it, &[NewChunk::text("x")])
            .unwrap();
    }

    fn knowledge(store: &Store) -> Knowledge {
        Knowledge::with_feed(
            Arc::new(store.clone()),
            Arc::new(MemGraph::new()),
            None,
            KnowledgeConfig::default(),
        )
    }

    #[test]
    fn dates_survive_the_cache_being_cleared() {
        let store = Store::open_in_memory().unwrap();
        let keys: Vec<String> = (0..DATE_CACHE + 3).map(|i| format!("n{i}")).collect();
        for (i, k) in keys.iter().enumerate() {
            put(
                &store,
                k,
                at("2026-01-01T00:00:00Z") + time::Duration::days(i as i64),
            );
        }
        let refs: Vec<ItemRef> = keys.iter().map(|k| item(k)).collect();
        let k = knowledge(&store);
        let mut s = Session::new(
            k.inner_for_tests(),
            State::default(),
            Notes::load(&store).unwrap(),
            Default::default(),
            0,
        );
        // Fill the cache, then ask for all: the misses clear it, the answer keeps everything.
        assert_eq!(
            s.dates_of(refs[..DATE_CACHE].iter()).unwrap().len(),
            DATE_CACHE
        );
        let all = s.dates_of(refs.iter()).unwrap();
        assert_eq!(all.len(), refs.len());
        assert_eq!(all[&refs[0]], at("2026-01-01T00:00:00Z"));
    }

    #[test]
    fn future_dates_are_clamped() {
        let store = Store::open_in_memory().unwrap();
        put(&store, "far", at("2099-01-01T00:00:00Z"));
        let k = knowledge(&store);
        let mut s = Session::new(
            k.inner_for_tests(),
            State::default(),
            Notes::load(&store).unwrap(),
            Default::default(),
            0,
        );
        let d = s.item_date(&item("far")).unwrap().unwrap();
        assert!(d <= OffsetDateTime::now_utc() + time::Duration::days(1));
    }

    fn claim(evidence: Vec<Evidence>) -> Claim {
        let t = at("2026-01-01T00:00:00Z");
        Claim {
            id: 1,
            subject: nugget_core::ids::EntityId::new("ent_a"),
            predicate: "works_at".into(),
            object: ClaimObject::Literal("Acme".into()),
            text: "A works at Acme.".into(),
            confidence: 0.7,
            status: ClaimStatus::Active,
            evidence,
            valid_from: None,
            valid_to: None,
            created_at: t,
            updated_at: t,
        }
    }

    #[test]
    fn a_value_is_read_from_every_evidence_item_in_any_order() {
        // More evidence than quotes are read for cues; the newest item, which says the job
        // ended, was added first (an old cap kept only the most recently added items).
        let n = CUE_SCAN * 2 + 10;
        let base = at("2020-01-01T00:00:00Z");
        let mut dates = Dates::new();
        let mut evidence = Vec::new();
        for i in (0..n).rev() {
            let it = item(&format!("m{i}"));
            dates.insert(it.clone(), base + time::Duration::days(i as i64));
            let quote = if i == n - 1 {
                "my last day at Acme".to_owned()
            } else {
                format!("Alice of Acme, note {i}")
            };
            evidence.push(Evidence {
                item: it,
                chunk_id: None,
                quote: Some(quote),
            });
        }
        let summary = |ev: Vec<Evidence>| {
            let mut g = ValueGroup::new(vec!["Acme".into()]);
            g.add(&claim(ev), &dates);
            let mut s = g.sightings.clone();
            s.sort();
            (g.affirmed, g.ended, g.ended(), s)
        };
        let forward = summary(evidence.clone());
        evidence.reverse();
        let backward = summary(evidence);
        assert_eq!(forward, backward);
        assert!(forward.2, "ended");
        assert_eq!(forward.1, Some(base + time::Duration::days(n as i64 - 1)));
        assert_eq!(forward.0, Some(base + time::Duration::days(n as i64 - 2)));
    }
}
