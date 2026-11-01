//! The per-source indexing pipeline: a bounded queue (backpressure) feeding one worker that applies
//! changes strictly in order.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::FuturesOrdered;
use nugget_connector::{CancellationToken, Change, ChangeSink, Content, Document};
use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_core::ipc::{IngestPhase, IngestProgress, ScanReport};
use nugget_core::{Error, Result};
use nugget_store::{NewChunk, NewItem, Store, content_hash};
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::ai;
use crate::ingest::lock;
use crate::ingest::{IngestEvent, Inner};
use crate::known::join_err;
use crate::metrics::Metrics;
use crate::pii::{self, PiiFindings};
use crate::sensitivity;
use crate::supervisor::Control;
use crate::triage;
use crate::util::cap_error;

/// How a change reached the pipeline. Backfill work pauses under power constraints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// A sync pass started without a cursor (initial import).
    Backfill,
    /// A sync pass resuming from a cursor.
    Incremental,
    /// Watch-driven live changes.
    Live,
}

pub(crate) enum OpKind {
    Change(Change),
    Checkpoint(Value, oneshot::Sender<Result<()>>),
    /// Start of a sync pass: clears the persistence-failure flag and the progress counter.
    BeginPass(oneshot::Sender<()>),
    /// Something the connector could not read ([`ChangeSink::report_problem`]).
    Problem(ItemKey, String),
}

pub(crate) struct Op {
    kind: OpKind,
    mode: Mode,
    observed: Instant,
}

/// The [`ChangeSink`] handed to connectors.
#[derive(Clone)]
pub(crate) struct PipelineSink {
    tx: mpsc::Sender<Op>,
    mode: Mode,
    inner: Arc<Inner>,
    source: SourceId,
}

impl PipelineSink {
    pub fn new(tx: mpsc::Sender<Op>, mode: Mode, inner: Arc<Inner>, source: SourceId) -> Self {
        Self {
            tx,
            mode,
            inner,
            source,
        }
    }

    /// Mark the start of a sync pass (ordered with the queue).
    pub async fn begin_pass(&self) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.push(OpKind::BeginPass(ack)).await?;
        rx.await.map_err(|_| Error::Cancelled)?;
        // The previous pass's report no longer gives this pass's total.
        self.inner.scan_pass_begins(&self.source);
        Ok(())
    }

    async fn push(&self, kind: OpKind) -> Result<()> {
        let op = Op {
            kind,
            mode: self.mode,
            observed: Instant::now(),
        };
        self.tx.send(op).await.map_err(|_| Error::Cancelled)
    }
}

/// Counts one change in the queue-depth gauge from before it is queued.
pub(crate) struct Queued<'a> {
    metrics: &'a Metrics,
    armed: bool,
}

impl<'a> Queued<'a> {
    pub(crate) fn new(metrics: &'a Metrics) -> Self {
        metrics.enqueued();
        Self {
            metrics,
            armed: true,
        }
    }

    pub(crate) fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for Queued<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.metrics.dequeued();
        }
    }
}

#[async_trait]
impl ChangeSink for PipelineSink {
    async fn send(&self, change: Change) -> Result<()> {
        let queued = Queued::new(&self.inner.metrics);
        let r = self.push(OpKind::Change(change)).await;
        if r.is_ok() {
            queued.disarm();
        }
        r
    }

    async fn checkpoint(&self, cursor: Value) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.push(OpKind::Checkpoint(cursor, ack)).await?;
        rx.await.map_err(|_| Error::Cancelled)?
    }

    async fn report_problem(&self, key: ItemKey, message: String) -> Result<()> {
        self.push(OpKind::Problem(key, message)).await
    }

    /// Not queued: the report describes the walk, which is done long before its changes are
    /// applied, and the UI should learn about an empty folder right away.
    async fn report_scan(&self, report: ScanReport) {
        self.inner.set_scan(&self.source, report).await;
    }
}

/// Most upserts written in one batch transaction.
pub(crate) const BATCH_MAX: usize = 64;
/// Longest a prepared upsert waits for its batch to fill.
pub(crate) const BATCH_MAX_WAIT: Duration = Duration::from_millis(250);

/// A prepared upsert waiting for its batch to be written.
struct PendingWrite {
    prepared: Prepared,
    item: ItemRef,
    version: Option<String>,
    /// When the pipeline received the change (freshness runs from here).
    observed: Instant,
    /// When it joined the pending batch.
    since: Instant,
    live_upsert: bool,
}

/// An upsert that finished preparing.
pub(crate) struct InFlight {
    item: ItemRef,
    version: Option<String>,
    observed: Instant,
    live_upsert: bool,
    result: std::result::Result<Prepared, String>,
}

type Preparing = std::pin::Pin<Box<dyn std::future::Future<Output = InFlight> + Send>>;

/// Upserts prepared at once per source, at most (their extraction also waits for one of the shared
/// extraction slots).
pub(crate) const PREPARE_AHEAD: usize = 4;

/// What is left of a [`PendingWrite`] once its item went to the store.
struct Written {
    item: ItemRef,
    version: Option<String>,
    observed: Instant,
    live_upsert: bool,
    mark: TriageMark,
}

impl PendingWrite {
    fn split(self) -> (Prepared, Written) {
        let mark = self.prepared.triage;
        (
            self.prepared,
            Written {
                item: self.item,
                version: self.version,
                observed: self.observed,
                live_upsert: self.live_upsert,
                mark,
            },
        )
    }
}

enum Outcome {
    Applied,
    Skipped,
    /// Permanent per-item failure (recorded; the pass continues).
    Rejected(String),
    /// Persistence failure: the pass must not commit its cursor.
    Failed(String),
}

/// Applies one source's queue in order.
pub(crate) struct Worker {
    inner: Arc<Inner>,
    source: SourceId,
    ctl: Arc<Control>,
    rx: mpsc::Receiver<Op>,
    /// Stored versions, loaded lazily and kept in step with our own writes.
    versions: Option<HashMap<ItemKey, Option<String>>>,
    /// Changes that failed to persist since the pass began.
    failed: u64,
    done: u64,
    last_progress: Instant,
    paused: bool,
    /// The last change came from the watcher (no scan total applies to it).
    live: bool,
    /// Prepared upserts not yet written (see "Batched writes" above), in queue order.
    pending: Vec<PendingWrite>,
    /// Upserts being prepared, oldest first (results come out in this order)...
    inflight: FuturesOrdered<Preparing>,
    /// ...and their keys and versions, for the version skip.
    inflight_keys: VecDeque<(ItemKey, Option<String>)>,
    /// How many upserts may be prepared at once.
    ahead: usize,
}

impl Worker {
    pub fn new(
        inner: Arc<Inner>,
        source: SourceId,
        ctl: Arc<Control>,
        rx: mpsc::Receiver<Op>,
    ) -> Self {
        let inner_slots = inner.cfg.extraction_slots();
        Self {
            inner,
            source,
            ctl,
            rx,
            versions: None,
            failed: 0,
            done: 0,
            last_progress: Instant::now(),
            paused: false,
            live: false,
            pending: Vec::new(),
            inflight: FuturesOrdered::new(),
            inflight_keys: VecDeque::new(),
            ahead: PREPARE_AHEAD.min(inner_slots).max(1),
        }
    }

    pub async fn run(mut self, cancel: CancellationToken) {
        loop {
            let deadline = self
                .pending
                .first()
                .map(|p| tokio::time::Instant::from_std(p.since + BATCH_MAX_WAIT));
            let take = self.inflight.len() < self.ahead;
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                Some(done) = self.inflight.next(), if !self.inflight.is_empty() => {
                    self.prepared(done).await;
                }
                op = self.rx.recv(), if take => match op {
                    Some(op) => self.handle(op, &cancel).await,
                    None => {
                        // The source is gone: finish what was received, as before batching.
                        self.drain(&cancel).await;
                        break;
                    }
                },
                // A slow preparation never holds back the items before it for long.
                _ = sleep_until(deadline), if deadline.is_some() => self.flush().await,
            }
            // Write the batch once it is full or nothing more is coming right now.
            if self.pending.len() >= BATCH_MAX
                || (!self.pending.is_empty() && self.inflight.is_empty() && self.rx.is_empty())
                || self
                    .pending
                    .first()
                    .is_some_and(|p| p.since.elapsed() >= BATCH_MAX_WAIT)
            {
                self.flush().await;
            }
        }
        // Discard what is left: nothing after the last checkpoint is committed, so the next pass
        // re-sends it.
        for _ in 0..self.pending.len() + self.inflight.len() {
            self.inner.metrics.dequeued();
        }
        self.pending.clear();
        self.inflight = FuturesOrdered::new();
        self.inflight_keys.clear();
        self.rx.close();
        while let Ok(op) = self.rx.try_recv() {
            if matches!(op.kind, OpKind::Change(_)) {
                self.inner.metrics.dequeued();
            }
        }
    }

    /// Finish every upsert in preparation and write the batch.
    async fn drain(&mut self, cancel: &CancellationToken) -> bool {
        self.flush().await;
        loop {
            let next = tokio::select! {
                biased;
                _ = cancel.cancelled() => return false,
                next = self.inflight.next() => next,
            };
            match next {
                Some(done) => self.prepared(done).await,
                None => break,
            }
        }
        self.flush().await;
        true
    }

    async fn handle(&mut self, op: Op, cancel: &CancellationToken) {
        // Anything but an upsert is ordered after the upserts before it: write them first.
        if !matches!(op.kind, OpKind::Change(Change::Upsert(_))) && !self.drain(cancel).await {
            // Cancelled: the op is dropped (a checkpoint's ack resolves to `Cancelled`).
            if matches!(op.kind, OpKind::Change(_)) {
                self.inner.metrics.dequeued();
            }
            return;
        }
        match op.kind {
            OpKind::BeginPass(ack) => {
                self.failed = 0;
                self.done = 0;
                let _ = ack.send(());
            }
            OpKind::Problem(key, msg) => {
                let msg = cap_error(msg);
                tracing::warn!(source = %self.source, key = %key, "source problem reported");
                self.inner.record_item_error(&self.source, Some(key), msg);
            }
            OpKind::Checkpoint(cursor, ack) => {
                let r = self.checkpoint(cursor).await;
                if r.is_ok() && self.done > 0 {
                    // Flush the throttled counter so UIs see the pass's final count.
                    self.progress(true);
                }
                let _ = ack.send(r);
            }
            OpKind::Change(change) => {
                self.live = op.mode == Mode::Live;
                let live_upsert = self.live && matches!(change, Change::Upsert(_));
                if op.mode == Mode::Backfill && !self.wait_for_power(cancel).await {
                    self.inner.metrics.dequeued();
                    return;
                }
                match change {
                    Change::Upsert(doc) => {
                        let key = doc.key.clone();
                        if let Some(outcome) = self.upsert(doc, op.observed, live_upsert).await {
                            self.finish(key, outcome, op.observed, live_upsert).await;
                        }
                    }
                    Change::Delete(key) => {
                        let outcome = self.delete(key.clone()).await;
                        self.finish(key, outcome, op.observed, live_upsert).await;
                    }
                }
            }
        }
    }

    /// An upsert finished preparing (in queue order): into the batch, or rejected.
    async fn prepared(&mut self, done: InFlight) {
        self.inflight_keys.pop_front();
        match done.result {
            Ok(prepared) => self.pending.push(PendingWrite {
                prepared,
                item: done.item,
                version: done.version,
                observed: done.observed,
                since: Instant::now(),
                live_upsert: done.live_upsert,
            }),
            // A file that changed (or was swapped) since the connector scanned it is skipped, not
            // stored: nothing is written, so the next pass sees it as new or changed and resends
            // it.
            Err(skip) => {
                self.finish(
                    done.item.key,
                    Outcome::Rejected(skip),
                    done.observed,
                    done.live_upsert,
                )
                .await
            }
        }
    }

    /// Account for one applied (or failed) change.
    async fn finish(
        &mut self,
        key: ItemKey,
        outcome: Outcome,
        observed: Instant,
        live_upsert: bool,
    ) {
        self.inner.metrics.dequeued();
        match outcome {
            Outcome::Applied => {
                if live_upsert {
                    self.inner.scan_live_change(&self.source).await;
                }
                self.inner.metrics.record_freshness(observed.elapsed());
                self.done += 1;
                self.progress(false);
            }
            Outcome::Skipped => {
                self.done += 1;
                self.progress(false);
            }
            Outcome::Rejected(msg) => {
                let msg = cap_error(msg);
                tracing::warn!(source = %self.source, key = %key, error = %msg, "item rejected");
                self.inner
                    .record_item_error(&self.source, Some(key), msg.clone());
                self.done += 1;
                self.progress_error(msg);
            }
            Outcome::Failed(msg) => {
                let msg = cap_error(msg);
                tracing::warn!(source = %self.source, key = %key, error = %msg, "item failed to persist");
                self.failed += 1;
                self.inner
                    .record_item_error(&self.source, Some(key), msg.clone());
                self.progress_error(msg);
            }
        }
    }

    /// Write the pending upserts: in one transaction, or one by one if that fails (so a bad item
    /// fails alone).
    async fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let (batch, written): (Vec<Prepared>, Vec<Written>) = std::mem::take(&mut self.pending)
            .into_iter()
            .map(PendingWrite::split)
            .unzip();
        let inner = self.inner.clone();
        let res = tokio::task::spawn_blocking(move || {
            let _w = lock(&inner.write_lock);
            persist_all(&inner.store, batch)
        })
        .await;
        let results: Vec<Result<()>> = match res {
            Ok(r) => r,
            // The blocking task panicked: what it wrote is unknown, so every item counts as failed
            // (the pass does not commit its cursor and re-sends them).
            Err(e) => {
                let msg = join_err(e).to_string();
                written
                    .iter()
                    .map(|_| Err(Error::Storage(msg.clone())))
                    .collect()
            }
        };
        for (w, r) in written.into_iter().zip(results) {
            let outcome = match r {
                Ok(()) => {
                    self.versions
                        .get_or_insert_with(HashMap::new)
                        .insert(w.item.key.clone(), w.version);
                    match w.mark {
                        TriageMark::Pending => self.inner.triage_enqueue(w.item.clone()),
                        TriageMark::Skipped => lock(&self.inner.triage).skipped(),
                        TriageMark::None => {}
                    }
                    Outcome::Applied
                }
                Err(e @ (Error::Invalid(_) | Error::NotFound(_))) => {
                    Outcome::Rejected(format!("store rejected item: {e}"))
                }
                Err(e) => Outcome::Failed(format!("store: {e}")),
            };
            self.finish(w.item.key, outcome, w.observed, w.live_upsert)
                .await;
        }
    }

    /// Wait while heavy work should pause. Returns `false` if cancelled.
    async fn wait_for_power(&mut self, cancel: &CancellationToken) -> bool {
        loop {
            if self.ctl.power_override.load(Ordering::Relaxed) || !self.inner.power.constrained() {
                if self.paused {
                    self.paused = false;
                    self.inner.set_paused(&self.source, false);
                }
                return true;
            }
            if !self.paused {
                // Nothing prepared stays unwritten while paused.
                if !self.drain(cancel).await {
                    return false;
                }
                self.paused = true;
                tracing::info!(source = %self.source, "backfill paused: low power");
                self.inner.set_paused(&self.source, true);
            }
            let poll = Duration::from_millis(self.inner.cfg.power_poll_ms.max(10));
            tokio::select! {
                _ = cancel.cancelled() => return false,
                _ = tokio::time::sleep(poll) => {}
                _ = self.ctl.power_changed.notified() => {}
            }
        }
    }

    /// What this pass is expected to index, once the connector's scan finished (never below `done`:
    /// deletes and live changes count too).
    fn total(&self) -> Option<u64> {
        if self.live {
            return None;
        }
        self.inner
            .scan_total(&self.source)
            .map(|t| t.max(self.done))
    }

    fn progress(&mut self, force: bool) {
        if force
            || self.done == 1
            || self.done.is_multiple_of(50)
            || self.last_progress.elapsed() >= Duration::from_millis(500)
        {
            self.last_progress = Instant::now();
            self.inner.emit(IngestEvent::Progress(IngestProgress {
                source: self.source.clone(),
                phase: IngestPhase::Indexing,
                done: self.done,
                total: self.total(),
                error: None,
            }));
        }
    }

    fn progress_error(&mut self, msg: String) {
        self.last_progress = Instant::now();
        self.inner.emit(IngestEvent::Progress(IngestProgress {
            source: self.source.clone(),
            phase: IngestPhase::Indexing,
            done: self.done,
            total: self.total(),
            error: Some(msg),
        }));
    }

    async fn checkpoint(&mut self, cursor: Value) -> Result<()> {
        if self.failed > 0 {
            return Err(Error::Storage(format!(
                "{} change(s) failed to persist since the pass began; cursor not committed",
                self.failed
            )));
        }
        let store = self.inner.store.clone();
        let source = self.source.clone();
        tokio::task::spawn_blocking(move || store.set_cursor(&source, cursor))
            .await
            .map_err(join_err)?
    }

    async fn known_version(&mut self, key: &ItemKey) -> Result<Option<Option<String>>> {
        if self.versions.is_none() {
            let store = self.inner.store.clone();
            let source = self.source.clone();
            let v = tokio::task::spawn_blocking(move || store.item_versions(&source))
                .await
                .map_err(join_err)??;
            self.versions = Some(v);
        }
        Ok(self.versions.as_ref().and_then(|m| m.get(key).cloned()))
    }

    async fn delete(&mut self, key: ItemKey) -> Outcome {
        let inner = self.inner.clone();
        let item = ItemRef {
            source: self.source.clone(),
            key: key.clone(),
        };
        let res = tokio::task::spawn_blocking(move || {
            let _w = lock(&inner.write_lock);
            inner.store.delete_item(&item)
        })
        .await;
        match res {
            Ok(Ok(_)) => {
                if let Some(v) = &mut self.versions {
                    v.remove(&key);
                }
                Outcome::Applied
            }
            Ok(Err(e)) => Outcome::Failed(format!("delete: {e}")),
            Err(e) => Outcome::Failed(format!("delete: {}", join_err(e))),
        }
    }

    /// Start preparing an upsert (`None`: it joins the batch once prepared), or the outcome when it
    /// ends here (version skip).
    async fn upsert(
        &mut self,
        doc: Document,
        observed: Instant,
        live_upsert: bool,
    ) -> Option<Outcome> {
        // 1.
        let queued = self
            .inflight_keys
            .iter()
            .rev()
            .find(|(k, _)| *k == doc.key)
            .map(|(_, v)| v.clone())
            .or_else(|| {
                self.pending
                    .iter()
                    .rev()
                    .find(|p| p.item.key == doc.key)
                    .map(|p| p.version.clone())
            });
        let stored = match queued {
            Some(v) => Some(v),
            None if doc.version.is_some() || self.versions.is_some() => {
                match self.known_version(&doc.key).await {
                    Ok(v) => v,
                    Err(e) => return Some(Outcome::Failed(format!("read versions: {e}"))),
                }
            }
            None => None,
        };
        if let (Some(v), Some(Some(stored))) = (&doc.version, &stored)
            && stored == v
        {
            return Some(Outcome::Skipped);
        }
        // Known not to be stored (versions loaded, key absent, nothing queued): no earlier triage
        // labels to look up.
        let maybe_stored = stored.is_some() || self.versions.is_none();
        let item = ItemRef {
            source: self.source.clone(),
            key: doc.key.clone(),
        };
        let version = doc.version.clone();
        self.inflight_keys
            .push_back((doc.key.clone(), version.clone()));

        // 2–6.
        let inner = self.inner.clone();
        self.inflight.push_back(Box::pin(async move {
            let result = prepare(&inner, &item, doc, maybe_stored).await;
            InFlight {
                item,
                version,
                observed,
                live_upsert,
                result,
            }
        }));
        None
    }
}

/// `sleep_until(d)`, or never without a deadline.
async fn sleep_until(d: Option<tokio::time::Instant>) {
    match d {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

/// An item ready to write.
pub(crate) struct Prepared {
    item: NewItem,
    chunks: Vec<NewChunk>,
    triage: TriageMark,
}

/// Output of the blocking extraction step.
struct Extraction {
    title: Option<String>,
    text: Option<String>,
    chunks: Vec<String>,
    extracted_meta: Value,
    links: Vec<String>,
    tags: Vec<String>,
    truncated: bool,
    error: Option<String>,
    pii: PiiFindings,
    hash: String,
    /// The file no longer matches what the connector scanned (see `nugget_extract::FileExpect`).
    file_changed: bool,
}

/// Build the item to write, or `Err(reason)` when it must be skipped this pass.
async fn prepare(
    inner: &Arc<Inner>,
    item: &ItemRef,
    doc: Document,
    maybe_stored: bool,
) -> std::result::Result<Prepared, String> {
    let Document {
        key,
        title: doc_title,
        mime,
        content,
        version,
        modified_at,
        url,
        metadata: source_meta,
    } = doc;
    // The connector's title is sender text like the body: drop what extraction drops from bodies
    // (see `visible_title`).
    let doc_title = doc_title.map(|t| visible_title(&t));

    let expect = nugget_extract::FileExpect::from_metadata(&source_meta);
    let mut ex = extract_step(
        inner,
        &mime,
        content,
        expect,
        doc_title.clone(),
        version.as_deref(),
    )
    .await;
    if ex.file_changed {
        return Err(format!(
            "skipped until the next pass: {}",
            ex.error
                .as_deref()
                .unwrap_or("file changed since it was scanned")
        ));
    }
    let title = doc_title
        .clone()
        .or_else(|| ex.title.clone())
        .filter(|t| !t.trim().is_empty());

    // 4.
    let rule = if !ex.pii.is_empty() {
        Some("pii")
    } else if source_meta.get("sensitive") == Some(&Value::Bool(true)) {
        Some("connector")
    } else {
        sensitivity::classify(
            key.as_str(),
            &ex.tags,
            title.as_deref(),
            ex.text.as_deref().unwrap_or(""),
            &ex.extracted_meta,
        )
    };
    let rule_sensitive = rule.is_some();
    let has_text = ex.text.as_deref().is_some_and(|t| !t.trim().is_empty());
    let routable = inner
        .router
        .as_ref()
        .is_some_and(|r| ai::triage_provider(r).is_some());
    // Unknown counts as sensitive (GG5-2): an item the rules don't flag waits for triage, also
    // while no local triage model is routable yet (the job picks it up once one is).
    let can_triage = inner.cfg.triage && has_text && (routable || !inner.cfg.assume_unflagged_safe);
    // With a model, rule hits are triaged too (for the junk label) but stay sensitive whatever it
    // answers (`triage::apply` reads `sensitive_rule`).
    let plan = if can_triage {
        match maybe_stored {
            true => earlier_labels(inner, item, &ex.hash).await,
            false => None,
        }
        .map_or(TriagePlan::Pending, TriagePlan::Reused)
    } else if let Some(rule) = rule {
        TriagePlan::Skipped(rule)
    } else {
        TriagePlan::Unknown(if !inner.cfg.triage {
            "off"
        } else if !has_text {
            "no_text"
        } else {
            "no_model"
        })
    };
    let (junk, sensitive) = match &plan {
        TriagePlan::Skipped(_) => (false, true),
        TriagePlan::Unknown(_) => (false, !inner.cfg.assume_unflagged_safe),
        // Until triage has run the item counts as sensitive: searchable locally, never sent to
        // cloud models.
        TriagePlan::Pending => (false, true),
        TriagePlan::Reused(v) => (
            v.get("junk").and_then(Value::as_bool).unwrap_or(false),
            rule_sensitive || v.get("sensitive").and_then(Value::as_bool).unwrap_or(true),
        ),
    };
    let title = title.or_else(|| Some(fallback_title(&key)));

    // 5–6. Chunks and embeddings (every item, junk or not).
    let chunk_texts = std::mem::take(&mut ex.chunks);
    let mut embeddings: Option<Vec<Vec<f32>>> = None;
    let mut embed_error = None;
    let mut embed_provider = None;
    if inner.cfg.embeddings
        && !chunk_texts.is_empty()
        && let Some(router) = &inner.router
    {
        let providers =
            ai::embedding_providers(router, sensitive, inner.cfg.allow_cloud_embeddings);
        if !providers.is_empty() {
            match ai::embed(
                &inner.store,
                router,
                &providers,
                item,
                &chunk_texts,
                inner.cfg.embed_batch,
                inner.cfg.embed_timeout(),
            )
            .await
            {
                Ok((pid, v)) => {
                    embed_provider = Some(pid);
                    embeddings = Some(v);
                }
                Err(e) => embed_error = Some(e.to_string()),
            }
        }
    }

    // Metadata.
    let mut meta = Map::new();
    meta.insert("source".into(), source_meta);
    if let Some(u) = url {
        meta.insert("url".into(), Value::String(u));
    }
    meta.insert("mime".into(), Value::String(mime.clone()));
    if !ex.extracted_meta.is_null() && ex.extracted_meta.as_object().is_none_or(|o| !o.is_empty()) {
        meta.insert("extracted".into(), std::mem::take(&mut ex.extracted_meta));
    }
    if !ex.links.is_empty() {
        meta.insert("links".into(), json!(ex.links));
    }
    if !ex.tags.is_empty() {
        meta.insert("tags".into(), json!(ex.tags));
    }
    meta.insert("sensitive".into(), Value::Bool(sensitive));
    if let Some(rule) = rule {
        meta.insert("sensitive_rule".into(), Value::String(rule.into()));
    }
    if !ex.pii.is_empty() {
        let mut p = Map::new();
        for (k, n) in [
            ("card", ex.pii.card),
            ("ssn", ex.pii.ssn),
            ("iban", ex.pii.iban),
            ("secret", ex.pii.secret),
        ] {
            if n > 0 {
                p.insert(k.into(), json!(n));
            }
        }
        meta.insert("pii".into(), Value::Object(p));
    }
    meta.insert("junk".into(), Value::Bool(junk));
    match &plan {
        TriagePlan::Unknown(reason) => {
            meta.insert(
                "triage".into(),
                json!({"state": triage::UNKNOWN, "reason": reason}),
            );
        }
        TriagePlan::Pending => {
            meta.insert("triage".into(), json!({"state": triage::PENDING}));
        }
        TriagePlan::Skipped(rule) => {
            meta.insert(
                "triage".into(),
                json!({"state": triage::SKIPPED, "reason": rule}),
            );
        }
        TriagePlan::Reused(v) => {
            meta.insert("triage".into(), v.clone());
        }
    }
    if ex.truncated {
        meta.insert("truncated".into(), Value::Bool(true));
    }
    if let Some(e) = ex.error.take() {
        meta.insert("extract_error".into(), Value::String(e));
    }
    if let Some(e) = embed_error {
        meta.insert("embed_error".into(), Value::String(e));
    }
    if let Some(p) = embed_provider {
        meta.insert("embedding_provider".into(), Value::String(p.0));
    }

    let mut new = NewItem::new(item.source.as_str(), key.as_str(), ex.hash.clone())
        .with_mime(mime)
        .with_metadata(Value::Object(meta));
    new.title = title;
    new.version = version;
    new.source_modified_at = modified_at;
    if inner.cfg.keep_text {
        new.text = ex.text.take();
    }

    let chunks = match embeddings {
        Some(vecs) => chunk_texts
            .into_iter()
            .zip(vecs)
            .map(|(t, e)| NewChunk::embedded(t, e))
            .collect(),
        None => chunk_texts.into_iter().map(NewChunk::text).collect(),
    };
    Ok(Prepared {
        item: new,
        chunks,
        triage: plan.mark(),
    })
}

/// How an item gets its triage labels.
enum TriagePlan {
    /// Triage cannot decide (off, no text, or no model with `assume_unflagged_safe`): the reason.
    Unknown(&'static str),
    /// Sensitive by rule (PII or `sensitivity` rules; the rule's name): no model call.
    Skipped(&'static str),
    /// To be labelled by the background job.
    Pending,
    /// Same content as the stored item, which was already labelled: keep its labels.
    Reused(Value),
}

impl TriagePlan {
    fn mark(&self) -> TriageMark {
        match self {
            TriagePlan::Pending => TriageMark::Pending,
            TriagePlan::Skipped(_) => TriageMark::Skipped,
            TriagePlan::Unknown(_) | TriagePlan::Reused(_) => TriageMark::None,
        }
    }
}

/// What the worker tells the triage job once the item is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TriageMark {
    None,
    Pending,
    Skipped,
}

/// The stored item's triage labels (`metadata.triage`, state `done`) if its content is `hash`.
async fn earlier_labels(inner: &Arc<Inner>, item: &ItemRef, hash: &str) -> Option<Value> {
    let store = inner.store.clone();
    let r = item.clone();
    let stored = tokio::task::spawn_blocking(move || store.get_item(&r))
        .await
        .ok()?
        .ok()??;
    if stored.content_hash != hash {
        return None;
    }
    let t = stored.metadata.get("triage")?;
    (t.get("state").and_then(Value::as_str) == Some(triage::DONE)).then(|| t.clone())
}

/// Extraction, PII scan and chunking on a blocking thread, bounded by the extraction timeout.
async fn extract_step(
    inner: &Arc<Inner>,
    mime: &str,
    content: Content,
    expect: Option<nugget_extract::FileExpect>,
    title: Option<String>,
    version: Option<&str>,
) -> Extraction {
    let failed = |error: String, title: Option<String>| {
        let pii = pii::scan_all(title.as_deref());
        let hash = content_hash(format!(
            "nugget-ingest/v1/error\0{}\0{}\0{}",
            version.unwrap_or(""),
            title.as_deref().unwrap_or(""),
            error
        ));
        Extraction {
            chunks: title.clone().into_iter().collect(),
            title,
            text: None,
            extracted_meta: Value::Null,
            links: Vec::new(),
            tags: Vec::new(),
            truncated: false,
            error: Some(error),
            pii,
            hash,
            file_changed: false,
        }
    };

    let permit = match inner.extract_slots.clone().acquire_owned().await {
        Ok(p) => p,
        Err(_) => return failed("ingest is shutting down".into(), title),
    };
    let opts = inner.cfg.extract_options();
    let chunk_opts = inner.cfg.chunk_options();
    let mime_owned = mime.to_owned();
    let task = tokio::task::spawn_blocking(move || {
        // Held until extraction really ends, even after a timeout: bounds runaway threads.
        let _permit = permit;
        let src = match &content {
            Content::Text(t) => nugget_extract::Source::Text(t),
            Content::Bytes(b) => nugget_extract::Source::Bytes(b),
            Content::File(p) => match expect {
                Some(expect) => nugget_extract::Source::CheckedFile { path: p, expect },
                None => nugget_extract::Source::File(p),
            },
        };
        let mime = if mime_owned.trim().is_empty() {
            match &content {
                Content::File(p) => nugget_extract::detect_mime(Some(p), &[]),
                Content::Bytes(b) => nugget_extract::detect_mime(None, &b[..b.len().min(512)]),
                Content::Text(_) => "text/plain".to_owned(),
            }
        } else {
            mime_owned
        };
        let ex = nugget_extract::extract(&mime, src, &opts)?;
        let chunks: Vec<String> = nugget_extract::chunk(&ex, &chunk_opts)
            .into_iter()
            .map(|c| c.text)
            .filter(|t| !t.trim().is_empty())
            .collect();
        let pii = pii::scan_all([ex.title.as_deref().unwrap_or(""), ex.text.as_str()]);
        let hash = content_hash(format!("nugget-ingest/v1\0{mime}\0{}", ex.text));
        Ok::<_, Error>((ex, chunks, pii, hash))
    });
    match tokio::time::timeout(inner.cfg.extract_timeout(), task).await {
        Ok(Ok(Ok((ex, chunks, mut pii_found, hash)))) => {
            // The connector-provided title is scanned too.
            if let Some(t) = &title {
                let f = pii::scan(t);
                pii_found.card += f.card;
                pii_found.ssn += f.ssn;
                pii_found.iban += f.iban;
                pii_found.secret += f.secret;
            }
            Extraction {
                title: ex.title,
                text: Some(ex.text),
                chunks,
                extracted_meta: ex.metadata,
                links: ex.links,
                tags: ex.tags,
                truncated: ex.truncated,
                error: None,
                pii: pii_found,
                hash,
                file_changed: false,
            }
        }
        Ok(Ok(Err(e))) if nugget_extract::is_file_changed(&e) => Extraction {
            file_changed: true,
            ..failed(format!("extract: {e}"), title)
        },
        Ok(Ok(Err(e))) => failed(format!("extract: {e}"), title),
        Ok(Err(e)) => failed(format!("extract: {}", join_err(e)), title),
        Err(_) => failed(
            format!(
                "extract: timed out after {} ms",
                inner.cfg.extract_timeout().as_millis()
            ),
            title,
        ),
    }
}

/// `title` without invisible format characters a reader never sees but a model reads: zero-width
/// space, LRM/RLM, word joiner and invisible operators, BOM, bidi embeddings, overrides and
/// isolates, and Unicode tag characters (which spell out hidden ASCII).
pub(crate) fn visible_title(title: &str) -> String {
    title
        .chars()
        .filter(|c| {
            !matches!(
                c,
                '\u{200B}'
                    | '\u{200E}'
                    | '\u{200F}'
                    | '\u{202A}'..='\u{202E}'
                    | '\u{2060}'..='\u{2064}'
                    | '\u{2066}'..='\u{2069}'
                    | '\u{FEFF}'
                    | '\u{E0000}'..='\u{E007F}'
            )
        })
        .collect()
}

/// Last path-ish segment of a key, as a title of last resort.
fn fallback_title(key: &ItemKey) -> String {
    let k = key.as_str();
    k.rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or(k)
        .to_owned()
}

/// Store an item text-only (with a note) when its vectors' dimension is not `dim`, the database's:
/// never mix embedding dimensions within a database.
fn fit_dimension(p: &mut Prepared, dim: Option<usize>) {
    let Some(dim) = dim else { return };
    let Some(got) = p
        .chunks
        .iter()
        .find_map(|c| c.embedding.as_ref())
        .map(Vec::len)
    else {
        return;
    };
    if got == dim {
        return;
    }
    for c in &mut p.chunks {
        c.embedding = None;
    }
    if let Value::Object(m) = &mut p.item.metadata {
        m.insert(
            "embed_error".into(),
            Value::String(format!(
                "embedding dimension {got} does not match the index ({dim}); stored text-only"
            )),
        );
        m.remove("embedding_provider");
    }
}

/// Write an item with its chunks and version in one transaction: on any error the stored item and
/// its chunks stay as they were.
fn persist(store: &Store, mut p: Prepared) -> Result<()> {
    fit_dimension(&mut p, store.embedding_dim()?);
    store.upsert_item_with_chunks(&p.item, &p.chunks)?;
    Ok(())
}

/// Write items in order, each with its chunks and version: all in one transaction, or, if that
/// fails, each in its own (so one bad item fails alone and the others are written).
fn persist_all(store: &Store, mut batch: Vec<Prepared>) -> Vec<Result<()>> {
    if batch.len() > 1 {
        // The first vectors written fix the dimension when the database has none yet.
        let dim = match store.embedding_dim() {
            Ok(d) => d.or_else(|| {
                batch
                    .iter()
                    .flat_map(|p| &p.chunks)
                    .find_map(|c| c.embedding.as_ref())
                    .map(Vec::len)
            }),
            Err(e) => {
                let msg = e.to_string();
                return batch
                    .iter()
                    .map(|_| Err(Error::Storage(msg.clone())))
                    .collect();
            }
        };
        for p in &mut batch {
            fit_dimension(p, dim);
        }
        let rows: Vec<(NewItem, Vec<NewChunk>)> =
            batch.into_iter().map(|p| (p.item, p.chunks)).collect();
        match store.upsert_items_with_chunks(&rows) {
            Ok(_) => return rows.iter().map(|_| Ok(())).collect(),
            Err(e) => {
                tracing::debug!(error = %e, items = rows.len(), "batch write failed; writing items one by one");
                batch = rows
                    .into_iter()
                    .map(|(item, chunks)| Prepared {
                        item,
                        chunks,
                        triage: TriageMark::None,
                    })
                    .collect();
            }
        }
    }
    batch.into_iter().map(|p| persist(store, p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared(text: &str, version: &str, embedding: Option<Vec<f32>>) -> Prepared {
        let mut item = NewItem::new("src", "doc", content_hash(text))
            .with_text(text)
            .with_version(version);
        item.title = Some("Doc".into());
        let chunk = match embedding {
            Some(e) => NewChunk::embedded(text, e),
            None => NewChunk::text(text),
        };
        Prepared {
            item,
            chunks: vec![chunk],
            triage: TriageMark::None,
        }
    }

    #[test]
    fn a_failed_persist_keeps_the_previous_item_and_chunks() {
        let store = Store::open_in_memory().unwrap();
        persist(&store, prepared("old heron text", "1", None)).unwrap();
        // The store rejects the new chunks (non-finite vector): nothing may change.
        let err = persist(
            &store,
            prepared("new kestrel text", "2", Some(vec![f32::NAN; 3])),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err:?}");
        let item = store
            .get_item(&ItemRef {
                source: SourceId::new("src"),
                key: ItemKey::new("doc"),
            })
            .unwrap()
            .unwrap();
        assert_eq!(item.version.as_deref(), Some("1"));
        assert_eq!(item.text.as_deref(), Some("old heron text"));
        assert_eq!(store.search_text("heron", 10).unwrap().len(), 1);
        assert!(store.search_text("kestrel", 10).unwrap().is_empty());
        // A good write replaces item and chunks together.
        persist(
            &store,
            prepared("new kestrel text", "2", Some(vec![0.5; 3])),
        )
        .unwrap();
        assert!(store.search_text("heron", 10).unwrap().is_empty());
        assert_eq!(store.search_text("kestrel", 10).unwrap().len(), 1);
        assert_eq!(store.embedding_dim().unwrap(), Some(3));
    }

    fn keyed(key: &str, text: &str, embedding: Option<Vec<f32>>) -> Prepared {
        let mut p = prepared(text, "1", embedding);
        p.item.item.key = ItemKey::new(key);
        p.item.metadata = json!({});
        p
    }

    fn stored(store: &Store, key: &str) -> Option<nugget_store::Item> {
        store
            .get_item(&ItemRef {
                source: SourceId::new("src"),
                key: ItemKey::new(key),
            })
            .unwrap()
    }

    #[test]
    fn a_batch_is_written_in_one_go_and_a_bad_item_fails_alone() {
        let store = Store::open_in_memory().unwrap();
        let ok = persist_all(
            &store,
            vec![
                keyed("a", "alpha heron", Some(vec![0.5; 3])),
                keyed("b", "bravo heron", None),
            ],
        );
        assert!(ok.iter().all(Result::is_ok), "{ok:?}");
        assert_eq!(store.search_text("heron", 10).unwrap().len(), 2);

        // A non-finite vector makes the batch transaction fail: the other items are still written,
        // one by one, and only the bad one reports the store's rejection.
        let res = persist_all(
            &store,
            vec![
                keyed("c", "charlie kestrel", Some(vec![0.25; 3])),
                keyed("d", "delta kestrel", Some(vec![f32::NAN; 3])),
                keyed("e", "echo kestrel", None),
            ],
        );
        assert!(res[0].is_ok() && res[2].is_ok(), "{res:?}");
        assert!(matches!(res[1], Err(Error::Invalid(_))), "{res:?}");
        assert!(stored(&store, "c").is_some() && stored(&store, "e").is_some());
        assert!(stored(&store, "d").is_none());
        assert_eq!(store.search_text("kestrel", 10).unwrap().len(), 2);
    }

    #[test]
    fn a_batch_never_mixes_embedding_dimensions() {
        let store = Store::open_in_memory().unwrap();
        // No dimension yet: the first vectors in the batch fix it; others go text-only.
        let res = persist_all(
            &store,
            vec![
                keyed("a", "alpha", Some(vec![0.5; 3])),
                keyed("b", "bravo", Some(vec![0.5; 4])),
            ],
        );
        assert!(res.iter().all(Result::is_ok), "{res:?}");
        assert_eq!(store.embedding_dim().unwrap(), Some(3));
        let b = stored(&store, "b").unwrap();
        assert!(
            b.metadata["embed_error"]
                .as_str()
                .unwrap()
                .contains("dimension 4"),
            "{}",
            b.metadata
        );
        assert_eq!(store.search_text("bravo", 10).unwrap().len(), 1);
    }

    #[test]
    fn fallback_titles() {
        assert_eq!(fallback_title(&ItemKey::new("notes/a/b.md")), "b.md");
        assert_eq!(fallback_title(&ItemKey::new("dir/")), "dir");
        assert_eq!(fallback_title(&ItemKey::new("msg-1")), "msg-1");
    }
}
