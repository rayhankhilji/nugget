//! The per-source indexing pipeline: a bounded queue (backpressure) feeding one worker that
//! applies changes strictly in order.
//!
//! For each `Upsert`:
//! 1. skip if `version` equals the stored version;
//! 2. extract on a blocking thread with a timeout (on failure or timeout: title-only item, with
//!    the error in metadata);
//! 3. deterministic PII scan (counts only; any finding marks the item sensitive);
//! 4. optional triage on a local provider (junk → metadata-only item);
//! 5. chunk; 6. embed in batches (local providers; cloud only if allowed and not sensitive);
//! 7. persist: `upsert_item` without a version, `replace_chunks`, then `upsert_item` with the
//!    version. The version is written last, so a crash in between leaves an item that the next
//!    pass re-processes instead of skipping.
//!
//! `Delete` removes the item. `checkpoint(cursor)` is queued behind every change sent before it,
//! so the cursor is stored only after they were applied; if any change since the start of the
//! pass failed to persist, the checkpoint fails instead (the pass is retried from the last good
//! cursor). Errors on a single item (bad content, extractor failures) never stop the source.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nugget_connector::{CancellationToken, Change, ChangeSink, Content, Document};
use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_core::ipc::{IngestPhase, IngestProgress};
use nugget_core::{Error, Result};
use nugget_store::{NewChunk, NewItem, Store, content_hash};
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, oneshot};

use crate::ai::{self, Triage};
use crate::ingest::{IngestEvent, Inner};
use crate::known::join_err;
use crate::pii::{self, PiiFindings};
use crate::supervisor::Control;

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
}

impl PipelineSink {
    pub fn new(tx: mpsc::Sender<Op>, mode: Mode, inner: Arc<Inner>) -> Self {
        Self { tx, mode, inner }
    }

    /// Mark the start of a sync pass (ordered with the queue).
    pub async fn begin_pass(&self) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.push(OpKind::BeginPass(ack)).await?;
        rx.await.map_err(|_| Error::Cancelled)
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

#[async_trait]
impl ChangeSink for PipelineSink {
    async fn send(&self, change: Change) -> Result<()> {
        self.inner.metrics.enqueued();
        let r = self.push(OpKind::Change(change)).await;
        if r.is_err() {
            self.inner.metrics.dequeued();
        }
        r
    }

    async fn checkpoint(&self, cursor: Value) -> Result<()> {
        let (ack, rx) = oneshot::channel();
        self.push(OpKind::Checkpoint(cursor, ack)).await?;
        rx.await.map_err(|_| Error::Cancelled)?
    }
}

enum Outcome {
    Applied,
    Skipped,
    /// Permanent per-item failure (recorded; the pass continues).
    Rejected(String),
    /// Persistence failure: the pass must not commit its cursor.
    Failed(String),
    /// Cancelled before anything was written.
    Abandoned,
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
}

impl Worker {
    pub fn new(
        inner: Arc<Inner>,
        source: SourceId,
        ctl: Arc<Control>,
        rx: mpsc::Receiver<Op>,
    ) -> Self {
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
        }
    }

    pub async fn run(mut self, cancel: CancellationToken) {
        loop {
            let op = tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                op = self.rx.recv() => match op {
                    Some(op) => op,
                    None => break,
                },
            };
            self.handle(op, &cancel).await;
        }
        // Discard what is left: nothing after the last checkpoint is committed, so the next
        // pass re-sends it. Dropped checkpoint acks resolve to `Cancelled`.
        self.rx.close();
        while let Ok(op) = self.rx.try_recv() {
            if matches!(op.kind, OpKind::Change(_)) {
                self.inner.metrics.dequeued();
            }
        }
    }

    async fn handle(&mut self, op: Op, cancel: &CancellationToken) {
        match op.kind {
            OpKind::BeginPass(ack) => {
                self.failed = 0;
                self.done = 0;
                let _ = ack.send(());
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
                if op.mode == Mode::Backfill && !self.wait_for_power(cancel).await {
                    self.inner.metrics.dequeued();
                    return;
                }
                let key = match &change {
                    Change::Upsert(d) => d.key.clone(),
                    Change::Delete(k) => k.clone(),
                };
                let outcome = match change {
                    Change::Upsert(doc) => self.upsert(doc, cancel).await,
                    Change::Delete(key) => self.delete(key).await,
                };
                self.inner.metrics.dequeued();
                match outcome {
                    Outcome::Applied => {
                        self.inner.metrics.record_freshness(op.observed.elapsed());
                        self.done += 1;
                        self.progress(false);
                    }
                    Outcome::Skipped => {
                        self.done += 1;
                        self.progress(false);
                    }
                    Outcome::Rejected(msg) => {
                        tracing::warn!(source = %self.source, key = %key, error = %msg, "item rejected");
                        self.inner
                            .record_item_error(&self.source, Some(key), msg.clone());
                        self.done += 1;
                        self.progress_error(msg);
                    }
                    Outcome::Failed(msg) => {
                        tracing::warn!(source = %self.source, key = %key, error = %msg, "item failed to persist");
                        self.failed += 1;
                        self.inner
                            .record_item_error(&self.source, Some(key), msg.clone());
                        self.progress_error(msg);
                    }
                    Outcome::Abandoned => {}
                }
            }
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
                total: None,
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
            total: None,
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
        let store = self.inner.store.clone();
        let item = ItemRef {
            source: self.source.clone(),
            key: key.clone(),
        };
        match tokio::task::spawn_blocking(move || store.delete_item(&item)).await {
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

    async fn upsert(&mut self, doc: Document, cancel: &CancellationToken) -> Outcome {
        // 1. Version skip.
        if let Some(v) = &doc.version {
            match self.known_version(&doc.key).await {
                Ok(Some(Some(stored))) if stored == *v => return Outcome::Skipped,
                Ok(_) => {}
                Err(e) => return Outcome::Failed(format!("read versions: {e}")),
            }
        }
        let item = ItemRef {
            source: self.source.clone(),
            key: doc.key.clone(),
        };
        let version = doc.version.clone();

        // 2–6. Everything before the store write may be abandoned on cancellation.
        let new = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Outcome::Abandoned,
            n = prepare(&self.inner, &item, doc) => n,
        };

        // 7. Persist (never abandoned half-way).
        let store = self.inner.store.clone();
        let res = tokio::task::spawn_blocking(move || persist(&store, new)).await;
        match res {
            Ok(Ok(())) => {
                self.versions
                    .get_or_insert_with(HashMap::new)
                    .insert(item.key, version);
                Outcome::Applied
            }
            Ok(Err(e @ (Error::Invalid(_) | Error::NotFound(_)))) => {
                Outcome::Rejected(format!("store rejected item: {e}"))
            }
            Ok(Err(e)) => Outcome::Failed(format!("store: {e}")),
            Err(e) => Outcome::Failed(format!("store: {}", join_err(e))),
        }
    }
}

/// An item ready to write.
pub(crate) struct Prepared {
    item: NewItem,
    chunks: Vec<NewChunk>,
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
}

async fn prepare(inner: &Arc<Inner>, item: &ItemRef, doc: Document) -> Prepared {
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

    let mut ex = extract_step(inner, &mime, content, doc_title.clone(), version.as_deref()).await;
    let title = doc_title
        .clone()
        .or_else(|| ex.title.clone())
        .filter(|t| !t.trim().is_empty());

    // 4. Triage (local providers only).
    let mut triage: Option<Triage> = None;
    let mut triage_error = None;
    if inner.cfg.triage
        && let Some(router) = &inner.router
        && let Some(text) = ex.text.as_deref().filter(|t| !t.trim().is_empty())
        && let Some(provider) = ai::triage_provider(router)
    {
        match ai::triage(
            provider.as_ref(),
            item,
            title.as_deref(),
            text,
            inner.cfg.triage_max_chars,
            inner.cfg.triage_timeout(),
        )
        .await
        {
            Ok(t) => {
                router.report_success(provider.id());
                triage = Some(t);
            }
            Err(e) => {
                tracing::debug!(provider = %provider.id(), error = %e, "triage failed");
                router.report_failure(provider.id());
                triage_error = Some(e.to_string());
            }
        }
    }
    let junk = triage.as_ref().is_some_and(Triage::is_junk);
    let sensitive = !ex.pii.is_empty() || triage.as_ref().is_some_and(|t| t.sensitive);
    let title = title
        .or_else(|| {
            triage
                .as_ref()
                .map(|t| t.title.clone())
                .filter(|t| !t.is_empty())
        })
        .or_else(|| Some(fallback_title(&key)));

    // 5–6. Chunks and embeddings (junk is metadata-only).
    let chunk_texts = if junk {
        Vec::new()
    } else {
        std::mem::take(&mut ex.chunks)
    };
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
                router,
                &providers,
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
    if let Some(t) = &triage {
        meta.insert(
            "triage".into(),
            json!({
                "keep": t.keep,
                "junk": t.junk,
                "sensitive": t.sensitive,
                "summary": t.summary,
                "title": t.title,
            }),
        );
    }
    if ex.truncated {
        meta.insert("truncated".into(), Value::Bool(true));
    }
    if let Some(e) = ex.error.take() {
        meta.insert("extract_error".into(), Value::String(e));
    }
    if let Some(e) = triage_error {
        meta.insert("triage_error".into(), Value::String(e));
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
    if inner.cfg.keep_text && !junk {
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
    Prepared { item: new, chunks }
}

/// Extraction, PII scan and chunking on a blocking thread, bounded by the extraction timeout.
async fn extract_step(
    inner: &Arc<Inner>,
    mime: &str,
    content: Content,
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
            Content::File(p) => nugget_extract::Source::File(p),
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
            }
        }
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

/// Last path-ish segment of a key, as a title of last resort.
fn fallback_title(key: &ItemKey) -> String {
    let k = key.as_str();
    k.rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or(k)
        .to_owned()
}

/// Write an item crash-safely: content first, version last.
fn persist(store: &Store, mut p: Prepared) -> Result<()> {
    // Never mix embedding dimensions within a database.
    if let Some(dim) = store.embedding_dim()?
        && let Some(first) = p.chunks.iter().find_map(|c| c.embedding.as_ref())
        && first.len() != dim
    {
        let got = first.len();
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
    let version = p.item.version.take();
    store.upsert_item(&p.item)?;
    store.replace_chunks(&p.item.item, &p.chunks)?;
    if version.is_some() {
        p.item.version = version;
        store.upsert_item(&p.item)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_titles() {
        assert_eq!(fallback_title(&ItemKey::new("notes/a/b.md")), "b.md");
        assert_eq!(fallback_title(&ItemKey::new("dir/")), "dir");
        assert_eq!(fallback_title(&ItemKey::new("msg-1")), "msg-1");
    }
}
