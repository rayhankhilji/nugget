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

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use nugget_core::Result;
use nugget_core::ids::ItemRef;
use nugget_core::ipc::{TriageProgress, TriageState};
use nugget_infer::Provider;
use nugget_store::{Item, NewItem};
use serde_json::{Value, json};

use crate::ai::{self, Triage, TriageDoc};
use crate::ingest::{Inner, lock};
use crate::known::join_err;
use crate::util::cap_error;

/// Failed attempts (per item) before an item is given up on.
pub(crate) const MAX_ATTEMPTS: u32 = 3;
/// Items listed per page by the recovery scan.
const PAGE: usize = 256;
/// How long the job lets a part-filled batch gather more items (a sync writes items quickly).
const GATHER: Duration = Duration::from_millis(250);
/// How often the job re-checks for a triage provider while none is routable.
const NO_PROVIDER_POLL: Duration = Duration::from_secs(60);

/// `metadata.triage.state` values.
pub(crate) const PENDING: &str = "pending";
pub(crate) const DONE: &str = "done";
pub(crate) const SKIPPED: &str = "skipped";
pub(crate) const FAILED: &str = "failed";

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
    state: TriageState,
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
            state: TriageState::Idle,
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
        if lock(&self.triage).push(item) {
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
            tokio::select! {
                _ = inner.shutdown.cancelled() => return,
                _ = inner.triage_wake.notified() => {}
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
            BatchOutcome::Labelled | BatchOutcome::Nothing => consecutive_failures = 0,
            BatchOutcome::Yielded => {}
            BatchOutcome::Failed => {
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

enum BatchOutcome {
    Labelled,
    Nothing,
    Yielded,
    Failed,
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
            finish_in_flight(inner);
            requeue_front(inner, refs);
            return BatchOutcome::Failed;
        }
    };
    // Pack by characters: the first item always goes; others while the budget allows.
    let max_chars = inner.cfg.triage_max_chars.max(1);
    let budget = inner.cfg.triage_batch_chars.max(1);
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

    let docs: Vec<TriageDoc<'_>> = batch
        .iter()
        .map(|l| TriageDoc {
            item: &l.item.item,
            title: l.item.title.as_deref(),
            text: &l.text,
        })
        .collect();
    let timeout = inner
        .cfg
        .triage_timeout()
        .saturating_mul(u32::try_from(docs.len()).unwrap_or(u32::MAX));
    let result = tokio::select! {
        biased;
        _ = inner.shutdown.cancelled() => None,
        // An interactive request started: give the model back now, retry later.
        _ = inner.foreground.started() => None,
        r = ai::triage_batch(provider, &docs, max_chars, timeout) => Some(r),
    };
    drop(docs);
    let refs: Vec<ItemRef> = batch.iter().map(|l| l.item.item.clone()).collect();
    let Some(result) = result else {
        tracing::debug!(
            items = refs.len(),
            "triage yielded to an interactive request"
        );
        finish_in_flight(inner);
        requeue_front(inner, refs);
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
                    for (l, t) in batch.iter().zip(labels) {
                        match apply(&me, &l.item, Verdict::Labelled(t, &provider_id)) {
                            Ok(Some(sensitive)) => {
                                n += 1;
                                opened |= !sensitive;
                            }
                            Ok(None) => {}
                            Err(e) => {
                                tracing::warn!(item = %l.item.item.key, error = %e, "storing triage labels failed");
                            }
                        }
                    }
                    (n, opened)
                })
                .await
                .unwrap_or((0, false))
            };
            {
                let mut q = lock(&inner.triage);
                q.in_flight = 0;
                q.done += applied.0;
                for r in &refs {
                    q.attempts.remove(r);
                    q.solo.remove(r);
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
            let msg = cap_error(e.to_string());
            tracing::debug!(items = refs.len(), error = %msg, "triage failed");
            let mut give_up = Vec::new();
            {
                let mut q = lock(&inner.triage);
                q.in_flight = 0;
                let batched = refs.len() > 1;
                let mut retry = Vec::new();
                for (r, l) in refs.iter().zip(&batch) {
                    let n = q.attempts.entry(r.clone()).or_insert(0);
                    *n += 1;
                    if *n >= MAX_ATTEMPTS {
                        q.attempts.remove(r);
                        q.solo.remove(r);
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
            }
            if !give_up.is_empty() {
                let me = inner.clone();
                let msg2 = msg.clone();
                let failed = tokio::task::spawn_blocking(move || {
                    give_up
                        .iter()
                        .filter(|it| matches!(apply(&me, it, Verdict::Failed(&msg2)), Ok(Some(_))))
                        .count() as u64
                })
                .await
                .unwrap_or(0);
                lock(&inner.triage).failed += failed;
                tracing::warn!(items = failed, error = %msg, "triage gave up on items; they stay searchable and count as sensitive");
            }
            BatchOutcome::Failed
        }
    }
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
    Failed(&'a str),
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
    if now.content_hash != read.content_hash
        || now.updated_at != read.updated_at
        || !is_pending(&now.metadata)
    {
        return Ok(None);
    }
    let mut metadata = now.metadata.clone();
    let Value::Object(m) = &mut metadata else {
        return Ok(None);
    };
    let pii = m
        .get("pii")
        .is_some_and(|p| p.as_object().is_some_and(|o| !o.is_empty()));
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
        Verdict::Failed(error) => {
            m.insert(
                "triage".into(),
                json!({"state": FAILED, "error": cap_error(error)}),
            );
            // Fail closed: never confirmed as safe for cloud models.
            true
        }
        Verdict::Off => {
            m.insert("triage".into(), json!({"state": SKIPPED, "reason": "off"}));
            pii
        }
    };
    m.insert("sensitive".into(), Value::Bool(sensitive));
    let mut new = NewItem::new(
        now.item.source.as_str(),
        now.item.key.as_str(),
        now.content_hash.clone(),
    )
    .with_metadata(metadata);
    new.title = now.title.clone();
    new.mime = now.mime.clone();
    new.text = now.text.clone();
    new.source_modified_at = now.source_modified_at;
    new.version = now.version.clone();
    // Same content hash: the item's chunks and vectors stay as they are.
    store.upsert_item(&new)?;
    Ok(Some(sensitive))
}
