//! The background triage job: labels indexed items (junk / sensitive) with a local model.
//!
//! Triage used to run inline in the pipeline, one model call per item before the item was
//! written, so a new folder only became searchable after every file had been through the chat
//! model (QA GG2-6), and a small model's "junk" verdict stored an item without chunks, so it
//! could never be found (GG2-7). Now:
//!
//! - **Search first.** The pipeline writes every item with its chunks and embeddings right
//!   away. An item that still needs triage carries `triage: {state: "pending"}` and, until
//!   triage has run, `sensitive: true`: it is searchable locally at once and kept away from
//!   cloud models (fail closed). Items whose labels are already known skip the model: PII found
//!   by the deterministic scan (`state: "skipped"`), or unchanged content that was triaged
//!   before (the earlier labels are kept).
//! - **Labels never hide.** Triage only rewrites metadata (`junk`, `sensitive`, `triage`); the
//!   item's chunks, vectors and text are never touched, whatever the verdict.
//! - **Batched.** Small items share one request (up to [`IngestConfig::triage_batch`] items and
//!   [`IngestConfig::triage_batch_chars`] characters); the reply is two booleans per item. A
//!   batch whose reply is unusable is retried one item per request; an item that fails
//!   [`MAX_ATTEMPTS`] times is marked `state: "failed"` and stays sensitive.
//! - **Failures are retried.** A failed item records how often the model failed on it
//!   (`triage.failures`: unusable replies, server errors, timeouts; not an unreachable or
//!   unavailable provider, so an outage never uses up an item's tries) and when
//!   (`triage.failed_at`, Unix seconds). While the queue is empty and a triage model is
//!   routable (and power allows), the job requeues failed items whose wait
//!   ([`IngestConfig::triage_retry_ms`], fourfold per failure, capped at a day) has passed: at
//!   start, when the model labels items again after an outage, on
//!   [`Ingest::triage_now`](crate::Ingest::triage_now) (a model became ready), and when the
//!   earliest waiting item is due. Until the model has labelled something since its last
//!   outage, a scan requeues one item only (a probe); otherwise at most [`PAGE`]. Requeued
//!   items go one per request, and their failures do not slow down other items. After
//!   [`IngestConfig::triage_max_failures`] failures an item stays failed until its content
//!   changes. A requeued item is `pending` again (and still sensitive, also if triage is turned
//!   off meanwhile), so a restart picks it up; it leaves `TriageProgress::failed` while it is
//!   retried.
//! - **Low priority.** The job uses the model only while no interactive request (Ask, Double
//!   Tap, agent) is in flight or ended within the grace period ([`Foreground`]), and abandons
//!   its request the moment one starts; the batch is retried later. It also pauses under the
//!   power policy, like backfills.
//! - **Visible.** `ingest.status.triage` reports the backlog, counters and what the job waits
//!   for ([`TriageProgress`]).
//! - **Restart-safe.** At start the job lists `pending` items from the store.
//!
//! [`IngestConfig::triage_batch`]: crate::IngestConfig::triage_batch
//! [`IngestConfig::triage_batch_chars`]: crate::IngestConfig::triage_batch_chars
//! [`IngestConfig::triage_retry_ms`]: crate::IngestConfig::triage_retry_ms
//! [`IngestConfig::triage_max_failures`]: crate::IngestConfig::triage_max_failures

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use nugget_core::ids::ItemRef;
use nugget_core::ipc::{TriageProgress, TriageState};
use nugget_core::{Error, Result};
use nugget_infer::Provider;
use nugget_store::Item;
use serde_json::{Value, json};

use crate::ai::{self, Triage, TriageDoc};
use crate::ingest::{Inner, lock};
use crate::known::join_err;
use crate::util::cap_error;

/// Failed attempts (per item) before an item is given up on.
pub(crate) const MAX_ATTEMPTS: u32 = 3;
/// Items listed per page by the recovery scan.
const PAGE: usize = 256;
/// Characters of document text per request before any prefill has been measured.
const FIRST_REQUEST_CHARS: usize = 2_000;
/// Fewest characters of document text per request unless the hardware is very slow (see
/// [`request_budget`]). Every request
/// pays the fixed part of the prompt (instructions, schema, chat template) again, so requests of
/// one or two short items spend most of their time on it (HH2 JJ1-1 run 41269745457: about
/// 6.5 s per item with the 4B, against about 1 s per item in batches of 8).
const MIN_REQUEST_CHARS: usize = 1_000;
/// Fewest characters per request on hardware so slow that even [`MIN_REQUEST_CHARS`] would take
/// more than twice `triage_request_ms` (a request must carry some text).
const SLOWEST_REQUEST_CHARS: usize = 300;
/// The fixed part of every request, in characters: the instructions and the chat template.
/// Counted in the measured prefill rate, so the time to the first token is not charged to the
/// document text alone (that made a short batch look slow and shrank every later one).
fn fixed_prompt_chars() -> usize {
    ai::TRIAGE_SYSTEM.chars().count() + 200
}

/// How long the job lets a part-filled batch gather more items (a sync writes items quickly).
const GATHER: Duration = Duration::from_millis(250);
/// How often the job re-checks for a triage provider while none is routable.
const NO_PROVIDER_POLL: Duration = Duration::from_secs(60);

/// `metadata.triage.state` values.
pub(crate) const PENDING: &str = "pending";
pub(crate) const DONE: &str = "done";
pub(crate) const SKIPPED: &str = "skipped";
pub(crate) const FAILED: &str = "failed";
pub(crate) const UNKNOWN: &str = "unknown";

/// The job's queue and counters (behind `Inner::triage`).
#[derive(Debug)]
pub(crate) struct TriageQueue {
    queue: VecDeque<ItemRef>,
    queued: HashSet<ItemRef>,
    /// Items being labelled right now.
    in_flight: usize,
    /// Items to retry one per request (their batch reply was unusable).
    solo: HashSet<ItemRef>,
    attempts: HashMap<ItemRef, u32>,
    done: u64,
    skipped: u64,
    failed: u64,
    /// Items counted in `failed` (so a retried item leaves the count, and is never counted
    /// twice).
    counted_failed: HashSet<ItemRef>,
    /// Scan for failed items to retry at the next chance (at start, after an outage, on
    /// `triage_now`).
    retry_due: bool,
    /// When the earliest failed item waiting for its retry is due.
    next_retry_at: Option<tokio::time::Instant>,
    /// The model labelled items since the provider last failed to answer (so failed items may
    /// be retried in bulk, not only one probe at a time).
    healthy: bool,
    /// Requeued failed items: their failures do not back off the whole job.
    retrying: HashSet<ItemRef>,
    state: TriageState,
    /// Measured prefill cost of triage prompts, ms per character of document text
    /// (exponential moving average of time to first token over prompt size).
    ms_per_char: Option<f64>,
}

impl Default for TriageQueue {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            queued: HashSet::new(),
            in_flight: 0,
            solo: HashSet::new(),
            attempts: HashMap::new(),
            done: 0,
            skipped: 0,
            failed: 0,
            counted_failed: HashSet::new(),
            retry_due: true,
            next_retry_at: None,
            healthy: false,
            retrying: HashSet::new(),
            state: TriageState::Idle,
            ms_per_char: None,
        }
    }
}

impl TriageQueue {
    pub(crate) fn push(&mut self, item: ItemRef) -> bool {
        if self.queued.insert(item.clone()) {
            self.queue.push_back(item);
            true
        } else {
            false
        }
    }

    pub(crate) fn skipped(&mut self) {
        self.skipped += 1;
    }

    /// Look for failed items to retry as soon as the job can.
    pub(crate) fn retry_now(&mut self) {
        self.retry_due = true;
    }

    /// Scan for failed items no later than `at`.
    fn retry_by(&mut self, at: tokio::time::Instant) {
        self.next_retry_at = Some(self.next_retry_at.map_or(at, |n| n.min(at)));
    }

    fn progress(&self) -> TriageProgress {
        let pending = (self.queue.len() + self.in_flight) as u64;
        let state = match self.state {
            TriageState::Running if pending == 0 => TriageState::Idle,
            s => s,
        };
        TriageProgress {
            state,
            pending,
            done: self.done,
            skipped: self.skipped,
            failed: self.failed,
        }
    }
}

impl Inner {
    /// Queue an indexed item for background triage.
    pub(crate) fn triage_enqueue(&self, item: ItemRef) {
        let mut q = lock(&self.triage);
        // Re-indexed: a fresh item, no longer a failed one or a retry.
        if q.counted_failed.remove(&item) {
            q.failed = q.failed.saturating_sub(1);
        }
        q.retrying.remove(&item);
        if q.push(item) {
            drop(q);
            self.triage_wake.notify_one();
        }
    }

    pub(crate) fn triage_progress(&self) -> TriageProgress {
        lock(&self.triage).progress()
    }

    fn set_triage_state(&self, state: TriageState) {
        lock(&self.triage).state = state;
    }
}

/// The job loop; ends on shutdown.
pub(crate) async fn run(inner: Arc<Inner>) {
    tokio::select! {
        biased;
        _ = inner.shutdown.cancelled() => return,
        _ = recover(&inner) => {}
    }
    let mut consecutive_failures = 0u32;
    loop {
        if inner.shutdown.is_cancelled() {
            return;
        }
        if lock(&inner.triage).queue.is_empty() {
            inner.set_triage_state(TriageState::Idle);
            let (requeued, next) = retry_failed(&inner).await;
            if requeued > 0 {
                continue;
            }
            tokio::select! {
                _ = inner.shutdown.cancelled() => return,
                _ = inner.triage_wake.notified() => {}
                _ = async {
                    match next {
                        Some(d) => tokio::time::sleep(d).await,
                        None => std::future::pending().await,
                    }
                } => {}
            }
            continue;
        }
        if !inner.cfg.triage {
            // Pending items from a run with triage on: label them as triage-off indexing would.
            resolve_without_model(&inner).await;
            continue;
        }
        let Some(provider) = inner.router.as_ref().and_then(|r| ai::triage_provider(r)) else {
            inner.set_triage_state(TriageState::Off);
            wait(&inner, NO_PROVIDER_POLL).await;
            continue;
        };
        if inner.power.constrained() {
            inner.set_triage_state(TriageState::Paused);
            wait(
                &inner,
                Duration::from_millis(inner.cfg.power_poll_ms.max(10)),
            )
            .await;
            continue;
        }
        if inner.foreground.busy() {
            inner.set_triage_state(TriageState::Yielding);
            tokio::select! {
                _ = inner.shutdown.cancelled() => return,
                _ = inner.foreground.idle() => {}
            }
            continue;
        }
        if lock(&inner.triage).queue.len() < inner.cfg.triage_batch.max(1) {
            tokio::select! {
                _ = inner.shutdown.cancelled() => return,
                _ = tokio::time::sleep(GATHER) => {}
            }
            if inner.foreground.busy() {
                continue;
            }
        }
        inner.set_triage_state(TriageState::Running);
        match run_batch(&inner, provider.as_ref()).await {
            BatchOutcome::Labelled => {
                let mut q = lock(&inner.triage);
                if !q.healthy {
                    // The model answers (again): retry what failed meanwhile, once due.
                    q.healthy = true;
                    q.retry_now();
                }
                consecutive_failures = 0;
            }
            BatchOutcome::Nothing => consecutive_failures = 0,
            BatchOutcome::Yielded => {}
            // A retried item failing again says nothing about the provider: no backoff, so
            // one poison item never holds up fresh ones.
            BatchOutcome::Failed { global: false } => {}
            BatchOutcome::Failed { global: true } => {
                consecutive_failures += 1;
                // The provider is probably down or overloaded: back off before the next try.
                let delay = inner
                    .cfg
                    .backoff(consecutive_failures.saturating_sub(1).min(8));
                tokio::select! {
                    _ = inner.shutdown.cancelled() => return,
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }
    }
}

async fn wait(inner: &Inner, d: Duration) {
    tokio::select! {
        _ = inner.shutdown.cancelled() => {}
        _ = inner.triage_wake.notified() => {}
        _ = tokio::time::sleep(d) => {}
    }
}

/// Queue every item the store still lists as pending (after a restart).
async fn recover(inner: &Arc<Inner>) {
    migrate_legacy(inner).await;
    let mut after: Option<ItemRef> = None;
    loop {
        let store = inner.store.clone();
        let from = after.clone();
        let page = match tokio::task::spawn_blocking(move || {
            store.items_pending_triage(from.as_ref(), PAGE)
        })
        .await
        .map_err(join_err)
        .and_then(|r| r)
        {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "listing items pending triage failed");
                return;
            }
        };
        let Some(last) = page.last().cloned() else {
            return;
        };
        let n = page.len();
        {
            let mut q = lock(&inner.triage);
            for item in page {
                q.push(item);
            }
        }
        inner.triage_wake.notify_one();
        if n < PAGE {
            return;
        }
        after = Some(last);
    }
}

/// Give items written before background triage existed (no `triage.state`) the labels the
/// pipeline gives new items: the sensitivity rules, then pending triage (or unknown). Their
/// old `sensitive: false` came from patterns or a model verdict only (QA GG4-6, GG5-2).
/// Metadata only (`Store::set_item_metadata`); pending items are queued by the scan after.
async fn migrate_legacy(inner: &Arc<Inner>) {
    let mut after: Option<ItemRef> = None;
    let mut migrated = 0u64;
    loop {
        let me = inner.clone();
        let from = after.clone();
        let res = tokio::task::spawn_blocking(move || -> Result<(Option<ItemRef>, u64)> {
            let page = me.store.items_without_triage_state(from.as_ref(), PAGE)?;
            let last = page.last().cloned();
            for r in &page {
                migrate_item(&me, r)?;
            }
            Ok((last.filter(|_| page.len() == PAGE), page.len() as u64))
        })
        .await
        .map_err(join_err)
        .and_then(|r| r);
        match res {
            Ok((next, n)) => {
                migrated += n;
                match next {
                    Some(last) => after = Some(last),
                    None => break,
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "labelling items from before background triage failed");
                break;
            }
        }
        if inner.shutdown.is_cancelled() {
            return;
        }
    }
    if migrated > 0 {
        tracing::info!(
            items = migrated,
            "labelled items indexed before background triage"
        );
    }
}

fn migrate_item(inner: &Inner, r: &ItemRef) -> Result<()> {
    let store = &inner.store;
    let _w = lock(&inner.write_lock);
    let Some(item) = store.get_item(r)? else {
        return Ok(());
    };
    let mut metadata = item.metadata.clone();
    let Value::Object(m) = &mut metadata else {
        return Ok(());
    };
    if m.get("triage").and_then(|t| t.get("state")).is_some() {
        return Ok(());
    }
    let text = match item.text.clone().filter(|t| !t.trim().is_empty()) {
        Some(t) => t,
        None => crate::reembed::chunk_texts(store, r)?.join("\n"),
    };
    let has_pii = m
        .get("pii")
        .is_some_and(|p| p.as_object().is_some_and(|o| !o.is_empty()));
    let tags: Vec<String> = m
        .get("tags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| t.as_str().map(str::to_owned))
        .collect();
    let extracted = m.get("extracted").cloned().unwrap_or(Value::Null);
    let rule = if has_pii {
        Some("pii")
    } else if m.get("source").and_then(|s| s.get("sensitive")) == Some(&Value::Bool(true)) {
        Some("connector")
    } else {
        crate::sensitivity::classify(
            r.key.as_str(),
            &tags,
            item.title.as_deref(),
            &text,
            &extracted,
        )
    };
    let routable = inner
        .router
        .as_ref()
        .is_some_and(|rt| ai::triage_provider(rt).is_some());
    let can_triage = inner.cfg.triage
        && !text.trim().is_empty()
        && (routable || !inner.cfg.assume_unflagged_safe);
    m.remove("triage_error");
    if let Some(rule) = rule {
        m.insert("sensitive_rule".into(), Value::String(rule.into()));
    }
    let (state, sensitive) = if can_triage {
        (json!({"state": PENDING}), true)
    } else if let Some(rule) = rule {
        (json!({"state": SKIPPED, "reason": rule}), true)
    } else {
        (
            json!({"state": UNKNOWN, "reason": "migrated"}),
            !inner.cfg.assume_unflagged_safe,
        )
    };
    m.insert("triage".into(), state);
    m.insert("sensitive".into(), Value::Bool(sensitive));
    store.set_item_metadata(r, &item.content_hash, &metadata)?;
    Ok(())
}

enum BatchOutcome {
    Labelled,
    Nothing,
    Yielded,
    /// `global`: the failure should back off the whole job (the provider or store failed, or
    /// items other than retried ones failed).
    Failed {
        global: bool,
    },
}

/// One item loaded for triage.
struct Loaded {
    item: Item,
    text: String,
}

/// Take the next batch off the queue (marking it in flight).
fn take_batch(inner: &Inner) -> Vec<ItemRef> {
    let mut q = lock(&inner.triage);
    let Some(first) = q.queue.pop_front() else {
        return Vec::new();
    };
    q.queued.remove(&first);
    let mut out = vec![first.clone()];
    if !q.solo.contains(&first) {
        let max = inner.cfg.triage_batch.max(1);
        while out.len() < max {
            let Some(next) = q.queue.front() else { break };
            if q.solo.contains(next) {
                break;
            }
            let next = q.queue.pop_front().expect("front exists");
            q.queued.remove(&next);
            out.push(next);
        }
    }
    q.in_flight = out.len();
    out
}

/// Put items back at the front of the queue, in order.
fn requeue_front(inner: &Inner, items: Vec<ItemRef>) {
    let mut q = lock(&inner.triage);
    for item in items.into_iter().rev() {
        if q.queued.insert(item.clone()) {
            q.queue.push_front(item);
        }
    }
}

fn finish_in_flight(inner: &Inner) {
    lock(&inner.triage).in_flight = 0;
}

async fn run_batch(inner: &Arc<Inner>, provider: &dyn Provider) -> BatchOutcome {
    let refs = take_batch(inner);
    if refs.is_empty() {
        finish_in_flight(inner);
        return BatchOutcome::Nothing;
    }
    // Load the items still pending, with their text.
    let loaded = {
        let store = inner.store.clone();
        let refs = refs.clone();
        let keep_text = inner.cfg.keep_text;
        tokio::task::spawn_blocking(move || -> Result<Vec<Option<Loaded>>> {
            let mut out = Vec::with_capacity(refs.len());
            for r in &refs {
                out.push(load(&store, r, keep_text)?);
            }
            Ok(out)
        })
        .await
        .map_err(join_err)
        .and_then(|r| r)
    };
    let loaded = match loaded {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, "reading items for triage failed");
            requeue_front(inner, refs);
            finish_in_flight(inner);
            return BatchOutcome::Failed { global: true };
        }
    };
    // Pack by characters within this request's budget: the first item always goes (a long one
    // alone, in parts); others while the budget allows.
    let max_chars = inner.cfg.triage_max_chars.max(1);
    let budget = request_budget(inner);
    let mut batch: Vec<Loaded> = Vec::new();
    let mut rest: Vec<ItemRef> = Vec::new();
    let mut used = 0usize;
    for (r, l) in refs.into_iter().zip(loaded) {
        let Some(l) = l else { continue };
        let len = l.text.chars().count().min(max_chars);
        if !batch.is_empty() && (used + len > budget || !rest.is_empty()) {
            rest.push(r);
            continue;
        }
        used += len;
        batch.push(l);
    }
    requeue_front(inner, rest);
    if batch.is_empty() {
        finish_in_flight(inner);
        return BatchOutcome::Nothing;
    }
    lock(&inner.triage).in_flight = batch.len();

    let result = label(inner, provider, &batch, max_chars, budget).await;
    let refs: Vec<ItemRef> = batch.iter().map(|l| l.item.item.clone()).collect();
    let result = match result {
        Ok(labels) => Some(Ok(labels)),
        Err(Some(e)) => Some(Err(e)),
        Err(None) => None,
    };
    let Some(result) = result else {
        tracing::debug!(
            items = refs.len(),
            "triage yielded to an interactive request"
        );
        requeue_front(inner, refs);
        finish_in_flight(inner);
        return BatchOutcome::Yielded;
    };
    let router = inner.router.as_ref();
    match result {
        Ok(labels) => {
            if let Some(r) = router {
                r.report_success(provider.id());
            }
            let provider_id = provider.id().as_str().to_owned();
            let applied = {
                let me = inner.clone();
                tokio::task::spawn_blocking(move || {
                    let mut n = 0u64;
                    let mut opened = false;
                    let mut retry = Vec::new();
                    for (l, t) in batch.iter().zip(labels) {
                        match apply(&me, &l.item, Verdict::Labelled(t, &provider_id)) {
                            Ok(Some(sensitive)) => {
                                n += 1;
                                opened |= !sensitive;
                            }
                            Ok(None) => {}
                            Err(e) => {
                                // Keep it queued: the next batch tries again.
                                tracing::warn!(item = %l.item.item.key, error = %e, "storing triage labels failed");
                                retry.push(l.item.item.clone());
                            }
                        }
                    }
                    (n, opened, retry)
                })
                .await
                .unwrap_or_else(|_| (0, false, refs.clone()))
            };
            // Requeue before clearing `in_flight`, so `pending` never reads 0 in between.
            requeue_front(inner, applied.2);
            {
                let mut q = lock(&inner.triage);
                q.in_flight = 0;
                q.done += applied.0;
                for r in &refs {
                    q.attempts.remove(r);
                    q.solo.remove(r);
                    q.retrying.remove(r);
                }
            }
            // Items now known not to be sensitive may use cloud embedders if allowed.
            if applied.1 && inner.cfg.allow_cloud_embeddings {
                inner.reembed.notify_one();
            }
            BatchOutcome::Labelled
        }
        Err(e) => {
            if let Some(r) = router {
                r.report_failure(provider.id());
            }
            let outage = is_outage(&e);
            let msg = cap_error(e.to_string());
            tracing::debug!(items = refs.len(), error = %msg, "triage failed");
            let mut give_up = Vec::new();
            let global;
            {
                let mut q = lock(&inner.triage);
                if outage {
                    q.healthy = false;
                }
                global = outage || !refs.iter().all(|r| q.retrying.contains(r));
                let batched = refs.len() > 1;
                let mut retry = Vec::new();
                for (r, l) in refs.iter().zip(&batch) {
                    let n = q.attempts.entry(r.clone()).or_insert(0);
                    *n += 1;
                    if *n >= MAX_ATTEMPTS {
                        q.attempts.remove(r);
                        q.solo.remove(r);
                        q.retrying.remove(r);
                        give_up.push(l.item.clone());
                    } else {
                        if batched {
                            // A batch reply may fail on one odd item: retry each alone.
                            q.solo.insert(r.clone());
                        }
                        retry.push(r.clone());
                    }
                }
                drop(q);
                requeue_front(inner, retry);
                finish_in_flight(inner);
            }
            if !give_up.is_empty() {
                let me = inner.clone();
                let msg2 = msg.clone();
                let marked = tokio::task::spawn_blocking(move || {
                    give_up
                        .iter()
                        .filter(|it| {
                            matches!(apply(&me, it, Verdict::Failed(&msg2, !outage)), Ok(Some(_)))
                        })
                        .map(|it| it.item.clone())
                        .collect::<Vec<_>>()
                })
                .await
                .unwrap_or_default();
                let failed = marked.len() as u64;
                {
                    let mut q = lock(&inner.triage);
                    for r in marked {
                        if q.counted_failed.insert(r) {
                            q.failed += 1;
                        }
                    }
                    // Retried once their wait passed: scan then (the shortest wait).
                    q.retry_by(tokio::time::Instant::now() + inner.cfg.triage_retry_after(0));
                }
                tracing::warn!(items = failed, error = %msg, "triage gave up on items; they stay searchable and count as sensitive");
            }
            BatchOutcome::Failed { global }
        }
    }
}

/// Characters of document text one triage request may carry: what the measured prefill rate
/// gets through in [`IngestConfig::triage_request_ms`], so an interactive request never waits
/// longer than that for a triage prompt the local server has already started (a server cannot
/// stop part-way through a prompt step; it cancels at the next one once the connection
/// closes). Capped by [`IngestConfig::triage_batch_chars`].
///
/// [`IngestConfig::triage_request_ms`]: crate::IngestConfig::triage_request_ms
/// [`IngestConfig::triage_batch_chars`]: crate::IngestConfig::triage_batch_chars
pub(crate) fn request_budget(inner: &Inner) -> usize {
    let rate = lock(&inner.triage).ms_per_char;
    budget_chars(
        rate,
        inner.cfg.triage_request_ms,
        inner.cfg.triage_batch_chars,
    )
}

/// [`request_budget`] for a measured prefill rate (ms per prompt character), a target in ms and
/// a cap in characters.
fn budget_chars(ms_per_char: Option<f64>, target_ms: u64, cap: usize) -> usize {
    let cap = cap.max(MIN_REQUEST_CHARS);
    let Some(rate) = ms_per_char.filter(|r| *r > 0.0) else {
        return FIRST_REQUEST_CHARS.min(cap);
    };
    let fits = |ms: f64| ((ms / rate) as usize).saturating_sub(fixed_prompt_chars());
    // The floor keeps batches large enough to label well, but on very slow hardware it may
    // stretch a request to at most twice the target; beyond that requests get smaller, down
    // to `SLOWEST_REQUEST_CHARS` (EE4 security review TB-2).
    let floor = fits(2.0 * target_ms as f64).clamp(SLOWEST_REQUEST_CHARS, MIN_REQUEST_CHARS);
    fits(target_ms as f64).clamp(floor, cap)
}

/// Fold one request's prefill into the measured rate.
fn record_prefill(inner: &Inner, chars: usize, first_token: Option<Duration>) {
    let Some(t) = first_token else { return };
    if chars < 50 {
        return;
    }
    let sample = t.as_secs_f64() * 1000.0 / (chars + fixed_prompt_chars()) as f64;
    let mut q = lock(&inner.triage);
    q.ms_per_char = Some(match q.ms_per_char {
        Some(old) => 0.7 * old + 0.3 * sample,
        None => sample,
    });
}

/// Label `batch` with requests of at most `budget` characters of document text: one request for
/// a packed batch, or, for a single item longer than the budget, one request per part (the
/// item is sensitive if any part is; junk is the first part's verdict). Between requests and
/// during each one, an interactive request makes it give up (`Err(None)`): the request in
/// flight is dropped, which closes its connection.
async fn label(
    inner: &Arc<Inner>,
    provider: &dyn Provider,
    batch: &[Loaded],
    max_chars: usize,
    budget: usize,
) -> std::result::Result<Vec<Triage>, Option<nugget_core::Error>> {
    let timeout_per_item = inner.cfg.triage_timeout();
    // Parts: one per item for a packed batch; several for one long item.
    let text_of = |l: &Loaded| -> Vec<char> { l.text.chars().take(max_chars).collect() };
    let requests: Vec<Vec<(usize, String, Option<String>)>> =
        if batch.len() == 1 && batch[0].text.chars().take(max_chars).count() > budget {
            let chars = text_of(&batch[0]);
            let parts: Vec<String> = chars.chunks(budget).map(|c| c.iter().collect()).collect();
            let n = parts.len();
            let title = batch[0]
                .item
                .title
                .clone()
                .unwrap_or_else(|| "(none)".into());
            parts
                .into_iter()
                .enumerate()
                .map(|(i, p)| vec![(0, p, Some(format!("{title} (part {} of {n})", i + 1)))])
                .collect()
        } else {
            vec![
                batch
                    .iter()
                    .enumerate()
                    .map(|(i, l)| (i, text_of(l).into_iter().collect(), None))
                    .collect(),
            ]
        };
    let mut out: Vec<Option<Triage>> = vec![None; batch.len()];
    for req in requests {
        if inner.foreground.busy() {
            return Err(None);
        }
        let docs: Vec<TriageDoc<'_>> = req
            .iter()
            .map(|(i, text, title)| TriageDoc {
                item: &batch[*i].item.item,
                title: title.as_deref().or(batch[*i].item.title.as_deref()),
                text,
            })
            .collect();
        let chars: usize = req.iter().map(|(_, t, _)| t.chars().count()).sum();
        let timeout = timeout_per_item.saturating_mul(u32::try_from(docs.len()).unwrap_or(1));
        let r = tokio::select! {
            biased;
            _ = inner.shutdown.cancelled() => return Err(None),
            // An interactive request started: drop this one now (closing its connection).
            _ = inner.foreground.started() => return Err(None),
            r = ai::triage_batch(provider, &docs, max_chars, timeout) => r,
        };
        let labelled = r.map_err(Some)?;
        record_prefill(inner, chars, labelled.first_token);
        for ((i, _, _), t) in req.iter().zip(labelled.labels) {
            out[*i] = Some(match out[*i] {
                None => t,
                Some(prev) => Triage {
                    junk: prev.junk,
                    sensitive: prev.sensitive || t.sensitive,
                },
            });
        }
    }
    out.into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or(Some(nugget_core::Error::Provider(
            "triage skipped a document".into(),
        )))
}

/// Errors that say the provider could not be reached or would not serve (down, unavailable,
/// refused us), not that the model failed on these items: they never count toward an item's
/// `triage.failures`, so an outage does not use up its tries.
fn is_outage(e: &Error) -> bool {
    matches!(
        e,
        Error::Network(_)
            | Error::RateLimited { .. }
            | Error::Unsupported(_)
            | Error::Unauthorized(_)
            | Error::Cancelled
    )
}

/// Resolve every queued item without a model (triage turned off since they were indexed): the
/// labels triage-off indexing gives (sensitive only when PII was found).
async fn resolve_without_model(inner: &Arc<Inner>) {
    let refs: Vec<ItemRef> = {
        let mut q = lock(&inner.triage);
        q.queued.clear();
        q.queue.drain(..).collect()
    };
    let me = inner.clone();
    let n = tokio::task::spawn_blocking(move || {
        let mut n = 0u64;
        for r in &refs {
            if let Ok(Some(l)) = load(&me.store, r, true)
                && matches!(apply(&me, &l.item, Verdict::Off), Ok(Some(_)))
            {
                n += 1;
            }
        }
        n
    })
    .await
    .unwrap_or(0);
    lock(&inner.triage).skipped += n;
}

/// The item if it is still pending, with the text triage reads (the stored text, else its
/// chunks).
fn load(store: &nugget_store::Store, r: &ItemRef, keep_text: bool) -> Result<Option<Loaded>> {
    let Some(item) = store.get_item(r)? else {
        return Ok(None);
    };
    if !is_pending(&item.metadata) {
        return Ok(None);
    }
    let text = match item
        .text
        .clone()
        .filter(|t| keep_text && !t.trim().is_empty())
    {
        Some(t) => t,
        None => crate::reembed::chunk_texts(store, r)?.join("\n"),
    };
    Ok(Some(Loaded { item, text }))
}

pub(crate) fn is_pending(meta: &Value) -> bool {
    meta.pointer("/triage/state").and_then(Value::as_str) == Some(PENDING)
}

enum Verdict<'a> {
    Labelled(Triage, &'a str),
    /// The error, and whether it counts toward the item's failures (see [`is_outage`]).
    Failed(&'a str, bool),
    Off,
}

/// Write a verdict into the metadata of `read` (as read for triage), under the write lock.
/// Nothing is written if the item changed or is no longer pending meanwhile (a concurrent
/// pipeline write wins). Returns the item's new `sensitive` flag if written.
fn apply(inner: &Inner, read: &Item, verdict: Verdict<'_>) -> Result<Option<bool>> {
    let store = &inner.store;
    let _w = lock(&inner.write_lock);
    let Some(now) = store.get_item(&read.item)? else {
        return Ok(None);
    };
    // A changed content hash means the pipeline rewrote the item (and queued it again).
    // `updated_at` may move for other reasons (the re-embed job), which do not affect labels.
    if now.content_hash != read.content_hash || !is_pending(&now.metadata) {
        return Ok(None);
    }
    let mut metadata = now.metadata.clone();
    let Value::Object(m) = &mut metadata else {
        return Ok(None);
    };
    // Sensitive by rule (PII or `sensitivity` rules) whatever the model says.
    let pii = m
        .get("pii")
        .is_some_and(|p| p.as_object().is_some_and(|o| !o.is_empty()))
        || m.get("sensitive_rule").is_some_and(Value::is_string);
    let sensitive = match verdict {
        Verdict::Labelled(t, provider) => {
            m.insert("junk".into(), Value::Bool(t.junk));
            m.insert(
                "triage".into(),
                json!({
                    "state": DONE,
                    "junk": t.junk,
                    "sensitive": t.sensitive,
                    "provider": provider,
                }),
            );
            pii || t.sensitive
        }
        Verdict::Failed(error, counts) => {
            // A retried item carries its earlier failures.
            let failures = m
                .get("triage")
                .and_then(|t| t.get("failures"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .saturating_add(u64::from(counts));
            m.insert(
                "triage".into(),
                json!({
                    "state": FAILED,
                    "error": cap_error(error),
                    "failures": failures,
                    "failed_at": unix_now(),
                }),
            );
            // Fail closed: never confirmed as safe for cloud models.
            true
        }
        Verdict::Off => {
            // An item whose triage failed before stays sensitive until a model labels it,
            // whatever `assume_unflagged_safe` says.
            let failed_before = m.get("triage").is_some_and(|t| t.get("failures").is_some());
            m.insert("triage".into(), json!({"state": UNKNOWN, "reason": "off"}));
            // Unknown counts as sensitive unless the user opted out (GG5-2).
            pii || failed_before || !inner.cfg.assume_unflagged_safe
        }
    };
    m.insert("sensitive".into(), Value::Bool(sensitive));
    // Metadata only: chunks, vectors and `updated_at` stay as they are, so consumers that
    // fingerprint items by `content_hash:updated_at` (knowledge extraction) do not redo them.
    if !store.set_item_metadata(&now.item, &now.content_hash, &metadata)? {
        return Ok(None);
    }
    Ok(Some(sensitive))
}

fn unix_now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// Requeue failed items whose retry wait has passed (see the module docs). Returns how many
/// were requeued and when to look again (`None`: only when woken).
async fn retry_failed(inner: &Arc<Inner>) -> (usize, Option<Duration>) {
    let max = inner.cfg.triage_max_failures;
    if !inner.cfg.triage || max <= 1 {
        return (0, None);
    }
    let now = tokio::time::Instant::now();
    let (due, next, healthy) = {
        let q = lock(&inner.triage);
        (q.retry_due, q.next_retry_at, q.healthy)
    };
    if !due && next.is_none_or(|t| t > now) {
        return (0, next.map(|t| t - now));
    }
    // Same gates as the batches: no model to retry with, or power says wait.
    if inner
        .router
        .as_ref()
        .and_then(|r| ai::triage_provider(r))
        .is_none()
    {
        return (0, Some(NO_PROVIDER_POLL));
    }
    if inner.power.constrained() {
        return (
            0,
            Some(Duration::from_millis(inner.cfg.power_poll_ms.max(10))),
        );
    }
    // Until the model has labelled something since it last failed to answer, one probe.
    let cap = if healthy { PAGE } else { 1 };
    let probe_every = inner.cfg.triage_retry_after(0);
    let me = inner.clone();
    let res = tokio::task::spawn_blocking(move || -> Result<(Vec<ItemRef>, bool, Option<u64>)> {
        let now = unix_now();
        let mut after: Option<ItemRef> = None;
        let mut requeue = Vec::new();
        // Due items left for a later scan; the earliest wait of the others, in seconds.
        let mut left = false;
        let mut earliest: Option<u64> = None;
        loop {
            let page = me.store.items_failed_triage(after.as_ref(), max, PAGE)?;
            let n = page.len();
            for (r, failures, failed_at) in page {
                after = Some(r.clone());
                let wait = me.cfg.triage_retry_after(failures).as_secs();
                let age = u64::try_from(now.saturating_sub(failed_at)).unwrap_or(0);
                if age < wait {
                    let rest = wait - age;
                    earliest = Some(earliest.map_or(rest, |e| e.min(rest)));
                } else if requeue.len() >= cap {
                    left = true;
                    if cap == PAGE {
                        // The rest on the next scan, once these are through.
                        return Ok((requeue, left, earliest));
                    }
                } else if reopen(&me, &r)? {
                    requeue.push(r);
                }
            }
            if n < PAGE || me.shutdown.is_cancelled() {
                return Ok((requeue, left, earliest));
            }
        }
    })
    .await
    .map_err(join_err)
    .and_then(|r| r);
    let mut q = lock(&inner.triage);
    q.retry_due = false;
    q.next_retry_at = None;
    let (requeue, left, earliest) = match res {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "listing items whose triage failed failed");
            q.retry_by(now + probe_every);
            return (0, Some(probe_every));
        }
    };
    if left && healthy {
        q.retry_due = true;
    } else if left {
        // Probing: the next probe after the shortest wait (or sooner, once labelling works).
        q.retry_by(now + probe_every);
    }
    if let Some(secs) = earliest {
        q.retry_by(now + Duration::from_secs(secs));
    }
    let n = requeue.len();
    for r in requeue {
        if q.counted_failed.remove(&r) {
            q.failed = q.failed.saturating_sub(1);
        }
        // One per request, so one odd item cannot fail a batch of others.
        q.solo.insert(r.clone());
        q.retrying.insert(r.clone());
        q.push(r);
    }
    let next = q.next_retry_at.map(|t| t.saturating_duration_since(now));
    drop(q);
    if n > 0 {
        tracing::info!(items = n, "retrying triage of items that failed before");
    }
    (n, next)
}

/// Mark a failed item `pending` again, keeping its failure count and time. Under the write
/// lock; nothing is written if it is no longer failed. Returns whether it was written.
fn reopen(inner: &Inner, r: &ItemRef) -> Result<bool> {
    let store = &inner.store;
    let _w = lock(&inner.write_lock);
    let Some(item) = store.get_item(r)? else {
        return Ok(false);
    };
    let mut metadata = item.metadata.clone();
    let Some(Value::Object(t)) = metadata.get_mut("triage") else {
        return Ok(false);
    };
    if t.get("state").and_then(Value::as_str) != Some(FAILED) {
        return Ok(false);
    }
    t.insert("state".into(), Value::String(PENDING.into()));
    t.remove("error");
    // Items that failed before failures were counted had one failure. Keeping the count also
    // marks the item as failed before (see `Verdict::Off`).
    if !t.get("failures").is_some_and(Value::is_u64) {
        t.insert("failures".into(), json!(1));
    }
    // Still sensitive (it was while failed): pending items count as sensitive until labelled.
    store.set_item_metadata(&item.item, &item.content_hash, &metadata)
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn requests_fit_the_target_but_never_shrink_below_what_labels_well_unless_very_slow() {
        let fixed = fixed_prompt_chars();
        // Not measured yet.
        assert_eq!(budget_chars(None, 8_000, 6_000), FIRST_REQUEST_CHARS);
        // Fast: capped by triage_batch_chars.
        assert_eq!(budget_chars(Some(0.1), 8_000, 6_000), 6_000);
        // About 2 ms per character (a 4-core CPU with the 4B): what fits in 8 s.
        assert_eq!(budget_chars(Some(2.0), 8_000, 6_000), 4_000 - fixed);
        // Slower: the 1 000-character floor holds while it costs at most twice the target.
        let r = 16_000.0 / (fixed + 1_000) as f64;
        assert_eq!(
            budget_chars(Some(r * 0.99), 8_000, 6_000),
            MIN_REQUEST_CHARS
        );
        // Slower still: the floor gives way (at most twice the target), down to 300 characters.
        let slow = r * 1.2;
        let b = budget_chars(Some(slow), 8_000, 6_000);
        let twice = ((16_000.0 / slow) as usize).saturating_sub(fixed);
        assert!(b < MIN_REQUEST_CHARS, "{b}");
        assert_eq!(b, twice.max(SLOWEST_REQUEST_CHARS));
        assert_eq!(
            budget_chars(Some(100.0), 8_000, 6_000),
            SLOWEST_REQUEST_CHARS
        );
    }
}
