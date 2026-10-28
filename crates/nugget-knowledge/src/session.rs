//! One run of the pipeline: plan the work queue, process items, apply claims.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Instant;

use nugget_core::ids::{EntityId, ItemKey, ItemRef, SourceId};
use nugget_core::{Error, Result};
use nugget_graph::{
    Claim, ClaimId, ClaimObject, ClaimStatus, EntityKind, EntityPatch, Evidence, Identifier,
    Mention, NewClaim, NewEntity,
};
use nugget_store::Item;
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
}

/// Diff every source's items against the ledger (blocking: run off the async runtime).
pub(crate) fn plan(feed: &dyn ItemFeed, llm_ready: bool, max_llm_attempts: u32) -> Result<Plan> {
    let mut state = State::load(feed)?;
    let notes = Notes::load(feed)?;
    let mut sources: BTreeSet<SourceId> = feed.sources()?.into_iter().collect();
    sources.extend(state.sources.iter().cloned());
    let reset = state.version != EXTRACTOR_VERSION;
    let mut ledgers = HashMap::new();
    let mut deletes = Vec::new();
    let mut upserts = Vec::new();
    for source in sources {
        let versions = feed.item_versions(&source)?;
        let mut ledger = SourceLedger::load(feed, &source)?;
        if reset {
            ledger.clear();
        }
        let mut gone: Vec<&String> = ledger
            .keys()
            .filter(|k| !versions.contains_key(&ItemKey::new(k.as_str())))
            .collect();
        gone.sort();
        deletes.extend(gone.into_iter().map(|k| ItemRef {
            source: source.clone(),
            key: ItemKey::new(k.as_str()),
        }));
        let mut keys: Vec<(&ItemKey, &Option<String>)> = versions.iter().collect();
        keys.sort_by(|a, b| a.0.cmp(b.0));
        for (key, version) in keys {
            let prev = ledger.get(key).cloned();
            let (todo, llm_retry) = match &prev {
                None => (true, false),
                Some(e) if e.failures > 0 && e.failures < MAX_FAILURES => (true, false),
                Some(_) if version.is_none() => (true, false),
                Some(e) if *version != e.version => (true, false),
                Some(e) => {
                    let retry =
                        llm_ready && e.llm >= LLM_PENDING && e.llm - LLM_PENDING < max_llm_attempts;
                    (retry, retry)
                }
            };
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
    if reset {
        state.version = EXTRACTOR_VERSION;
    }
    Ok(Plan {
        state,
        notes,
        ledgers,
        deletes,
        upserts,
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

    fn item_date(&mut self, item: &ItemRef) -> Result<Option<OffsetDateTime>> {
        if let Some(d) = self.dates.get(item) {
            return Ok(*d);
        }
        let d = self
            .feed()
            .get_item(item)?
            .map(|i| crate::extract::item_date(&i));
        if self.dates.len() > 10_000 {
            self.dates.clear();
        }
        self.dates.insert(item.clone(), d);
        Ok(d)
    }

    /// The newest date among a claim's evidence (user assertions: when asserted).
    fn freshness(&mut self, c: &Claim) -> Result<Option<OffsetDateTime>> {
        if c.evidence.is_empty() {
            return Ok(Some(c.updated_at));
        }
        let mut best = None;
        for ev in c.evidence.iter().rev().take(FRESHNESS_SCAN) {
            let d = self.item_date(&ev.item)?;
            if d > best {
                best = d;
            }
        }
        Ok(best)
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
        if !ex.me_emails.is_empty() || ex.me_name.is_some() {
            let emails = std::mem::take(&mut ex.me_emails);
            let name = ex.me_name.take();
            self.learn_me(&emails, name.as_deref())?;
        }
        let llm = self.llm_step(item, &chunks, &mut ex, prev_llm).await;
        let mut reuse = retracted.reuse;
        self.apply(ex, item, date, &mut reuse).await?;
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
        ex.absorb(run.extraction);
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
        for spec in &ex.entities {
            let me = self.state.me.clone();
            let resolver = Resolver {
                graph: self.inner.graph.as_ref(),
                cfg: &self.inner.cfg,
                router: self.inner.router.as_deref(),
                emb_cache: &self.inner.emb_cache,
                me,
            };
            let r = resolver.resolve(spec).await?;
            if r.created {
                self.stats.entities_created += 1;
            }
            self.stats.entities_merged += r.merged.len() as u64;
            for t in &r.touched {
                self.touch(t);
            }
            for m in &spec.mentions {
                self.graph().add_mention(&Mention {
                    entity: r.id.clone(),
                    item: item.item.clone(),
                    chunk_id: m.chunk_id,
                    surface: truncate_chars(&m.surface, 200),
                    confidence: m.confidence.clamp(0.0, 1.0),
                })?;
                self.stats.mentions_added += 1;
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
        Ok(())
    }

    // ---------------------------------------------------------------- claims

    /// Add a claim with its lifecycle: revive a just-retracted twin, supersede older values of
    /// single-valued predicates, and aggregate confidence over evidence.
    fn add_claim(
        &mut self,
        mut new: NewClaim,
        date: OffsetDateTime,
        reuse: &mut HashMap<String, ClaimId>,
    ) -> Result<ClaimId> {
        if let Some(q) = new.evidence.iter_mut().find_map(|e| e.quote.as_mut()) {
            *q = truncate_chars(q, MAX_QUOTE_CHARS);
        }
        let key = claim_key(&new.subject, &new.predicate, &new.object);
        let id = if self.single_valued.contains(&new.predicate) {
            self.add_single_valued(&mut new, date, reuse.remove(&key))?
        } else if let Some(id) = reuse.remove(&key) {
            for ev in &new.evidence {
                self.graph().add_evidence(id, ev)?;
            }
            id
        } else {
            let id = self.graph().add_claim(&new)?;
            self.stats.claims_added += 1;
            id
        };
        self.aggregate_confidence(id, new.confidence)?;
        if let Some(c) = self.graph().get_claim(id)? {
            self.touch_claim(&c);
        }
        Ok(id)
    }

    fn add_single_valued(
        &mut self,
        new: &mut NewClaim,
        date: OffsetDateTime,
        revive: Option<ClaimId>,
    ) -> Result<ClaimId> {
        let all: Vec<Claim> = self
            .graph()
            .claims_for(&new.subject, true)?
            .into_iter()
            .filter(|c| c.subject == new.subject && c.predicate == new.predicate)
            .collect();
        // The same value, already known (active, superseded, or about to be revived).
        let same = all
            .iter()
            .filter(|c| c.object == new.object)
            .filter(|c| {
                matches!(
                    c.status,
                    ClaimStatus::Active | ClaimStatus::UserAsserted | ClaimStatus::Superseded
                ) || Some(c.id) == revive
            })
            .min_by_key(|c| match c.status {
                ClaimStatus::Active | ClaimStatus::UserAsserted => 0,
                ClaimStatus::Superseded => 1,
                ClaimStatus::Retracted => 2,
            })
            .cloned();
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

        let id = match same {
            Some(c) => {
                for ev in &new.evidence {
                    self.graph().add_evidence(c.id, ev)?;
                }
                let status = self.graph().get_claim(c.id)?.map(|x| x.status);
                if historical && status == Some(ClaimStatus::Active) {
                    self.graph()
                        .set_claim_status(c.id, ClaimStatus::Superseded)?;
                    self.stats.claims_superseded += 1;
                } else if !historical && status == Some(ClaimStatus::Superseded) {
                    self.graph().set_claim_status(c.id, ClaimStatus::Active)?;
                    self.stats.claims_reactivated += 1;
                }
                c.id
            }
            None => {
                if historical {
                    new.status = ClaimStatus::Superseded;
                    new.valid_to = newest_other;
                }
                let id = self.graph().add_claim(new)?;
                self.stats.claims_added += 1;
                if historical {
                    self.stats.claims_superseded += 1;
                }
                id
            }
        };
        if !historical {
            for (c, f) in others {
                if c.status == ClaimStatus::Active && f < fresh {
                    self.graph()
                        .set_claim_status(c.id, ClaimStatus::Superseded)?;
                    self.stats.claims_superseded += 1;
                    self.touch_claim(&c);
                }
            }
        }
        Ok(id)
    }

    /// Raise a claim's confidence to the noisy-OR of its distinct evidence items.
    fn aggregate_confidence(&mut self, id: ClaimId, base: f32) -> Result<()> {
        let Some(c) = self.graph().get_claim(id)? else {
            return Ok(());
        };
        if c.status != ClaimStatus::Active {
            return Ok(());
        }
        let items: HashSet<&ItemRef> = c.evidence.iter().map(|e| &e.item).collect();
        if items.len() < 2 {
            return Ok(());
        }
        let target = aggregate(base, items.len());
        if target > c.confidence + 0.005 {
            // `add_claim` on an active triple adds the evidence (already there) and keeps the
            // higher confidence.
            self.graph().add_claim(&NewClaim {
                subject: c.subject.clone(),
                predicate: c.predicate.clone(),
                object: c.object.clone(),
                text: c.text.clone(),
                confidence: target,
                status: ClaimStatus::Active,
                evidence: c.evidence.iter().rev().take(1).cloned().collect(),
                valid_from: c.valid_from,
                valid_to: c.valid_to,
            })?;
        }
        Ok(())
    }

    pub fn out_of_time(&self, deadline: Option<Instant>) -> bool {
        deadline.is_some_and(|d| Instant::now() >= d)
    }
}

#[cfg(test)]
mod tests {
    use super::aggregate;

    #[test]
    fn noisy_or() {
        assert!((aggregate(0.5, 1) - 0.5).abs() < 1e-6);
        assert!((aggregate(0.5, 2) - 0.75).abs() < 1e-6);
        assert!(aggregate(0.9, 40) <= 0.99);
        assert!(aggregate(0.995, 3) <= 0.995 + 1e-6);
    }
}
