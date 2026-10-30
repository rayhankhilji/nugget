//! One run of the pipeline: plan the work queue, process items, apply claims.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Instant;

use nugget_core::ids::{EntityId, ItemKey, ItemRef, SourceId};
use nugget_core::{Error, Result};
use nugget_graph::{
    Claim, ClaimId, ClaimObject, ClaimPatch, ClaimStatus, EntityDetail, EntityKind, EntityPatch,
    Evidence, GraphBatch, GraphOp, Identifier, Mention, NewClaim, NewEntity, normalize_name,
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
use crate::text::{MAX_QUOTE_CHARS, truncate_chars};

/// Processing failures after which an item is left alone until it changes.
pub(crate) const MAX_FAILURES: u32 = 3;
/// Single-valued predicates (see [`crate::KnowledgeConfig::single_valued_predicates`]).
pub const SINGLE_VALUED: &[&str] = &[
    "works_at",
    "lives_in",
    "has_title",
    "reports_to",
    "born_on",
    "due_on",
    "located_at",
];
/// Evidence items consulted per claim when judging freshness.
const FRESHNESS_SCAN: usize = 32;
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
    single_valued: Vec<(EntityId, String)>,
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
    /// Claims to re-aggregate (noisy-OR) once `batch` is applied, with their base confidence.
    agg: Vec<(Queued, f32)>,
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
                self.graph().merge(me, &other.id)?;
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
        self.retracted_any = true;
        let mut out = Retracted::default();
        for id in affected {
            let Some(c) = self.graph().get_claim(id)? else {
                continue;
            };
            self.touch_claim(&c);
            if c.status == ClaimStatus::Retracted && c.evidence.is_empty() {
                out.reuse
                    .insert(claim_key(&c.subject, &c.predicate, &c.object), c.id);
                if self.single_valued.contains(&c.predicate) {
                    out.single_valued
                        .push((c.subject.clone(), c.predicate.clone()));
                }
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

    /// After retraction, a single-valued predicate left without an active value falls back to
    /// the freshest superseded claim that still has evidence.
    fn restore_superseded(&mut self, pairs: &[(EntityId, String)]) -> Result<()> {
        let mut done: HashSet<(EntityId, String)> = HashSet::new();
        for (s, p) in pairs {
            if !done.insert((s.clone(), p.clone())) {
                continue;
            }
            let claims: Vec<Claim> = self
                .graph()
                .claims_for(s, true)?
                .into_iter()
                .filter(|c| &c.subject == s && &c.predicate == p)
                .collect();
            if claims.iter().any(|c| c.status.is_active()) {
                continue;
            }
            let mut best: Option<(Option<OffsetDateTime>, ClaimId)> = None;
            for c in claims
                .iter()
                .filter(|c| c.status == ClaimStatus::Superseded && !c.evidence.is_empty())
            {
                let f = self.freshness(c)?;
                if best.as_ref().is_none_or(|(bf, _)| f > *bf) {
                    best = Some((f, c.id));
                }
            }
            if let Some((_, id)) = best {
                self.graph().set_claim_status(id, ClaimStatus::Active)?;
                // It is current again (if it folded into a live twin, the twin is unaffected).
                if self.graph().get_claim(id)?.is_some() {
                    self.graph().update_claim(
                        id,
                        &ClaimPatch {
                            valid_to: Some(None),
                            ..Default::default()
                        },
                    )?;
                }
                self.stats.claims_reactivated += 1;
                if let Some(c) = self.graph().get_claim(id)? {
                    self.touch_claim(&c);
                }
            }
        }
        Ok(())
    }

    pub fn process_delete(&mut self, item: &ItemRef) -> Result<()> {
        let r = self.retract(item)?;
        self.stats.claims_retracted += r.reuse.len() as u64;
        self.restore_superseded(&r.single_valued)?;
        self.ledger(&item.source).remove(&item.key);
        self.since_flush += 1;
        self.stats.items_deleted += 1;
        Ok(())
    }

    // ---------------------------------------------------------------- items

    /// Load the dates of `items` not cached yet, in one feed call.
    fn prefetch_dates<'i>(&mut self, items: impl IntoIterator<Item = &'i ItemRef>) -> Result<()> {
        let mut missing: Vec<ItemRef> = Vec::new();
        let mut seen: HashSet<&ItemRef> = HashSet::new();
        for i in items {
            if !self.dates.contains_key(i) && seen.insert(i) {
                missing.push(i.clone());
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        let found = self.feed().item_dates(&missing)?;
        if self.dates.len() + missing.len() > 10_000 {
            self.dates.clear();
        }
        for i in missing {
            let d = found.get(&i).copied();
            self.dates.insert(i, d);
        }
        Ok(())
    }

    fn item_date(&mut self, item: &ItemRef) -> Result<Option<OffsetDateTime>> {
        self.prefetch_dates(std::iter::once(item))?;
        Ok(self.dates.get(item).copied().flatten())
    }

    /// The newest date among a claim's evidence (user assertions: when asserted).
    fn freshness(&mut self, c: &Claim) -> Result<Option<OffsetDateTime>> {
        if c.evidence.is_empty() {
            return Ok(Some(c.updated_at));
        }
        let scan: Vec<&Evidence> = c.evidence.iter().rev().take(FRESHNESS_SCAN).collect();
        self.prefetch_dates(scan.iter().map(|e| &e.item))?;
        Ok(scan
            .iter()
            .filter_map(|e| self.dates.get(&e.item).copied().flatten())
            .max())
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
        let date = crate::extract::item_date(item);
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
        self.commit()?;
        self.stats.claims_retracted += reuse.len() as u64;
        self.restore_superseded(&retracted.single_valued)?;
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
        for spec in &ex.entities {
            let me = self.state.me.clone();
            let resolver = Resolver::new(
                self.inner.graph.as_ref(),
                &self.inner.cfg,
                self.inner.router.as_deref(),
                &self.inner.emb_cache,
                me,
            )
            .with_splits(&splits, Some(&item.item));
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
    /// confidence of the claims it wrote.
    pub fn commit(&mut self) -> Result<()> {
        let batch = std::mem::take(&mut self.batch);
        self.queued.clear();
        let agg = std::mem::take(&mut self.agg);
        if batch.is_empty() {
            return Ok(());
        }
        let results = self.graph().apply_batch(&batch)?;
        let mut seen = HashSet::new();
        let mut follow = GraphBatch::new();
        for (target, base) in agg {
            let id = match target {
                Queued::Id(id) => Some(id),
                Queued::Op(i) => results.get(i).and_then(|r| r.claim()),
            };
            if let Some(id) = id
                && seen.insert(id)
                && let Some(patch) = self.aggregated(id, base)?
            {
                follow.push(GraphOp::UpdateClaim { claim: id, patch });
            }
        }
        if !follow.is_empty() {
            self.graph().apply_batch(&follow)?;
        }
        Ok(())
    }

    /// Drop the pending batch (the item failed).
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

    fn status_op(
        &mut self,
        claim: ClaimId,
        status: ClaimStatus,
        valid_to: Option<Option<OffsetDateTime>>,
    ) {
        self.batch.push(GraphOp::SetClaimStatus { claim, status });
        if let Some(v) = valid_to {
            self.batch.push(GraphOp::UpdateClaim {
                claim,
                patch: ClaimPatch {
                    valid_to: Some(v),
                    ..Default::default()
                },
            });
        }
    }

    /// Add a claim with its lifecycle: revive a just-retracted twin, supersede older values of
    /// single-valued predicates, and aggregate confidence over evidence. The writes go to the
    /// item's batch.
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
        let target = if self.single_valued.contains(&new.predicate) {
            self.add_single_valued(&mut new, date, reuse.remove(&key))?
        } else if let Some(id) = reuse.remove(&key) {
            self.add_evidence_op(id, &new.evidence);
            Queued::Id(id)
        } else {
            self.stats.claims_added += 1;
            Queued::Op(self.batch.push(GraphOp::AddClaim { claim: new.clone() }))
        };
        self.queued.insert(key, target);
        self.agg.push((target, new.confidence));
        Ok(())
    }

    fn add_single_valued(
        &mut self,
        new: &mut NewClaim,
        date: OffsetDateTime,
        revive: Option<ClaimId>,
    ) -> Result<Queued> {
        let all: Vec<Claim> = self
            .graph()
            .claims_for(&new.subject, true)?
            .into_iter()
            .filter(|c| c.subject == new.subject && c.predicate == new.predicate)
            .collect();
        // The same value, already known (active, rejected, superseded, or about to be revived).
        let same = all
            .iter()
            .filter(|c| c.object == new.object)
            .filter(|c| {
                matches!(
                    c.status,
                    ClaimStatus::Active
                        | ClaimStatus::UserAsserted
                        | ClaimStatus::Rejected
                        | ClaimStatus::Superseded
                ) || Some(c.id) == revive
            })
            .min_by_key(|c| match c.status {
                ClaimStatus::Rejected => 0,
                ClaimStatus::Active | ClaimStatus::UserAsserted => 1,
                ClaimStatus::Superseded => 2,
                ClaimStatus::Retracted => 3,
            })
            .cloned();
        // The user rejected this value: keep the evidence, change nothing else.
        if let Some(c) = &same
            && c.status == ClaimStatus::Rejected
        {
            self.add_evidence_op(c.id, &new.evidence);
            return Ok(Queued::Id(c.id));
        }
        let all_evidence: Vec<ItemRef> = all
            .iter()
            .flat_map(|c| c.evidence.iter().rev().take(FRESHNESS_SCAN))
            .map(|e| e.item.clone())
            .collect();
        self.prefetch_dates(all_evidence.iter())?;
        let mut fresh = Some(date);
        if let Some(c) = &same {
            fresh = fresh.max(self.freshness(c)?);
        }
        let mut others: Vec<(Claim, Option<OffsetDateTime>)> = Vec::new();
        for c in all
            .into_iter()
            .filter(|c| c.object != new.object && c.status.is_active())
        {
            let f = self.freshness(&c)?;
            others.push((c, f));
        }
        let newest_other = others.iter().filter_map(|(_, f)| *f).max();
        // Older than a current value: recorded as history.
        let historical = newest_other.is_some_and(|o| Some(o) > fresh);

        let target = match same {
            Some(c) => {
                self.add_evidence_op(c.id, &new.evidence);
                // Adding evidence revives a retracted claim.
                let status = match c.status {
                    ClaimStatus::Retracted => ClaimStatus::Active,
                    s => s,
                };
                if historical && status == ClaimStatus::Active {
                    self.status_op(c.id, ClaimStatus::Superseded, Some(newest_other));
                    self.stats.claims_superseded += 1;
                } else if !historical && status == ClaimStatus::Superseded {
                    self.status_op(c.id, ClaimStatus::Active, Some(None));
                    self.stats.claims_reactivated += 1;
                }
                Queued::Id(c.id)
            }
            None => {
                if historical {
                    new.status = ClaimStatus::Superseded;
                    new.valid_to = newest_other;
                }
                self.stats.claims_added += 1;
                if historical {
                    self.stats.claims_superseded += 1;
                }
                Queued::Op(self.batch.push(GraphOp::AddClaim { claim: new.clone() }))
            }
        };
        if !historical {
            for (c, f) in others {
                if c.status == ClaimStatus::Active && f < fresh {
                    // Valid until the newer value's evidence.
                    self.status_op(c.id, ClaimStatus::Superseded, fresh.map(Some));
                    self.stats.claims_superseded += 1;
                    self.touch_claim(&c);
                }
            }
        }
        Ok(target)
    }

    /// After a merge, the entity (and the subjects of claims about it) may hold several live
    /// values of a single-valued predicate that lived on different entities before: keep the
    /// freshest (a user-asserted one first) and supersede the older ones.
    fn reconcile_single_valued(&mut self, id: &EntityId) -> Result<()> {
        let mut subjects: Vec<EntityId> = vec![id.clone()];
        for c in self.graph().claims_for(id, false)? {
            if !subjects.contains(&c.subject) {
                subjects.push(c.subject.clone());
            }
        }
        for subject in subjects {
            let mut by_pred: HashMap<String, Vec<Claim>> = HashMap::new();
            for c in self.graph().claims_for(&subject, false)? {
                if c.subject == subject
                    && c.status.is_active()
                    && self.single_valued.contains(&c.predicate)
                {
                    by_pred.entry(c.predicate.clone()).or_default().push(c);
                }
            }
            for (_, claims) in by_pred {
                if claims.iter().all(|c| c.object == claims[0].object) {
                    continue;
                }
                let mut scored = Vec::with_capacity(claims.len());
                for c in claims {
                    let f = self.freshness(&c)?;
                    scored.push((c.status == ClaimStatus::UserAsserted, f, c));
                }
                scored.sort_by_key(|x| std::cmp::Reverse((x.0, x.1)));
                let (best_user, best_f, best) = (scored[0].0, scored[0].1, scored[0].2.clone());
                for (_, f, c) in scored.into_iter().skip(1) {
                    if c.status == ClaimStatus::Active
                        && c.object != best.object
                        && (best_user || f < best_f)
                    {
                        self.graph()
                            .set_claim_status(c.id, ClaimStatus::Superseded)?;
                        self.graph().update_claim(
                            c.id,
                            &ClaimPatch {
                                valid_to: Some(best_f),
                                ..Default::default()
                            },
                        )?;
                        self.stats.claims_superseded += 1;
                        self.touch_claim(&c);
                    }
                }
            }
        }
        Ok(())
    }

    /// The patch raising a claim's confidence to the noisy-OR of its distinct evidence items,
    /// if that is higher.
    fn aggregated(&self, id: ClaimId, base: f32) -> Result<Option<ClaimPatch>> {
        let Some(c) = self.graph().get_claim(id)? else {
            return Ok(None);
        };
        if c.status != ClaimStatus::Active {
            return Ok(None);
        }
        let items: HashSet<&ItemRef> = c.evidence.iter().map(|e| &e.item).collect();
        if items.len() < 2 {
            return Ok(None);
        }
        let target = aggregate(base, items.len());
        Ok((target > c.confidence + 0.005).then(|| ClaimPatch {
            confidence: Some(target),
            ..Default::default()
        }))
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

/// A person the model named by one word only ("Maggie") is the one person on the same item
/// whose name starts with that word ("Maggie T" in the headers), when there is exactly one:
/// the occurrence takes that person's name and identifiers, so both resolve together.
fn fold_first_names(base: &Extraction, extra: &mut Extraction) {
    let first_word = |n: &str| normalize_name(n).split(' ').next().map(str::to_owned);
    let words = |n: &str| normalize_name(n).split(' ').count();
    for i in 0..extra.entities.len() {
        let e = &extra.entities[i];
        if e.kind != EntityKind::Person
            || e.weak_name
            || !e.identifiers.is_empty()
            || words(&e.name) != 1
        {
            continue;
        }
        let Some(word) = first_word(&e.name).filter(|w| w.chars().count() >= 2) else {
            continue;
        };
        // Candidates by display name, de-duplicated by normalised name.
        let mut cands: Vec<(String, &crate::spec::EntitySpec)> = Vec::new();
        let others = base.entities.iter().chain(
            extra
                .entities
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, x)| x),
        );
        for c in others {
            if c.kind != EntityKind::Person || c.weak_name || words(&c.name) < 2 {
                continue;
            }
            let matches = std::iter::once(&c.name)
                .chain(&c.aliases)
                .any(|n| words(n) >= 2 && first_word(n).as_deref() == Some(word.as_str()));
            let key = normalize_name(&c.name);
            if matches && !cands.iter().any(|(k, _)| *k == key) {
                cands.push((key, c));
            }
        }
        if cands.len() != 1 {
            continue;
        }
        let (name, identifiers) = (cands[0].1.name.clone(), cands[0].1.identifiers.clone());
        let e = &mut extra.entities[i];
        let short = std::mem::replace(&mut e.name, name);
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
