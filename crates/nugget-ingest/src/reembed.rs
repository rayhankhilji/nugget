//! The re-embed job: gives vectors to items that were indexed text-only (no embedder was routable,
//! or embedding failed), and runs a re-index to another embedding model when one was started
//! ([`Reindex`]).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nugget_core::ids::{ItemRef, ProviderId};
use nugget_core::{Error, Result};
use nugget_store::{Item, NewChunk, NewItem, UpsertOutcome};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ai;
use crate::ingest::{Inner, lock};
use crate::known::join_err;
use crate::util::cap_error;

/// Items read from the store per page.
const PAGE: usize = 32;
/// Chunk positions read per lookup.
const ORD_PAGE: u32 = 64;
/// Failures of one item (same content) before it is skipped.
const MAX_ITEM_FAILURES: u32 = 3;
/// Consecutive failures that end a run (the provider is probably down).
const MAX_CONSECUTIVE_FAILURES: u32 = 3;

/// What the re-embed job has done since start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReembedStats {
    /// Runs that found a routable embedder.
    pub runs: u64,
    /// Items given vectors.
    pub embedded: u64,
    /// Items left text-only (no allowed provider, changed meanwhile, dimension mismatch).
    pub skipped: u64,
    /// Items whose embedding failed.
    pub failed: u64,
    /// The last failure, capped.
    pub last_error: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct ReembedState {
    pub stats: ReembedStats,
    /// Items not to retry until their content hash changes: item → (content hash, failures).
    pub skip: HashMap<ItemRef, (String, u32)>,
}

/// Called with the target once the index switched to the re-indexed vectors.
pub type OnReindexed = Arc<dyn Fn(nugget_store::ReindexTarget) + Send + Sync>;

/// A re-index in progress: the new model's embedder and what to do once switched.
pub struct Reindex {
    provider: Arc<dyn nugget_infer::Provider>,
    on_done: OnReindexed,
    /// The last failure (the pass then waits for the next run).
    pub error: Option<String>,
}

impl Reindex {
    pub(crate) fn new(provider: Arc<dyn nugget_infer::Provider>, on_done: OnReindexed) -> Self {
        Self {
            provider,
            on_done,
            error: None,
        }
    }
}

/// The job loop; ends on shutdown.
pub(crate) async fn run(inner: Arc<Inner>) {
    loop {
        tokio::select! {
            biased;
            _ = inner.shutdown.cancelled() => return,
            _ = async { run_once(&inner).await; reindex_once(&inner).await } => {}
        }
        let interval = inner.cfg.reembed_interval_ms;
        tokio::select! {
            _ = inner.shutdown.cancelled() => return,
            _ = inner.reembed.notified() => {}
            _ = sleep_or_forever(interval) => {}
        }
    }
}

async fn sleep_or_forever(ms: u64) {
    if ms == 0 {
        std::future::pending::<()>().await;
    } else {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
}

enum Outcome {
    Embedded,
    Skipped,
    Failed(String),
}

async fn run_once(inner: &Arc<Inner>) {
    if !inner.cfg.embeddings {
        return;
    }
    let Some(router) = inner.router.clone() else {
        return;
    };
    // The most permissive provider set an item could get; none at all means nothing to do.
    if ai::embedding_providers(&router, false, inner.cfg.allow_cloud_embeddings).is_empty() {
        return;
    }
    if inner.power.constrained() {
        tracing::debug!("re-embedding deferred: low power");
        return;
    }
    lock(&inner.reembed_state).stats.runs += 1;

    let (mut embedded, mut consecutive_failures) = (0u64, 0u32);
    let mut after: Option<ItemRef> = None;
    'pages: loop {
        let page = {
            let store = inner.store.clone();
            let after = after.clone();
            match tokio::task::spawn_blocking(move || {
                store.items_missing_embeddings(after.as_ref(), PAGE)
            })
            .await
            .map_err(join_err)
            .and_then(|r| r)
            {
                Ok(p) => p,
                Err(e) => {
                    record_failure(inner, None, format!("list items: {e}"));
                    return;
                }
            }
        };
        let Some(last) = page.last() else { break };
        after = Some(last.clone());
        for item in page {
            match reembed_item(inner, &router, &item).await {
                Outcome::Embedded => {
                    embedded += 1;
                    consecutive_failures = 0;
                    let mut st = lock(&inner.reembed_state);
                    st.stats.embedded += 1;
                    st.skip.remove(&item);
                }
                Outcome::Skipped => {
                    lock(&inner.reembed_state).stats.skipped += 1;
                }
                Outcome::Failed(msg) => {
                    consecutive_failures += 1;
                    tracing::warn!(item = %item.key, source = %item.source, error = %msg, "re-embedding failed");
                    record_failure(inner, Some(&item), msg);
                    if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                        tracing::warn!("re-embedding paused after repeated failures");
                        break 'pages;
                    }
                }
            }
        }
    }
    if embedded > 0 {
        tracing::info!(
            items = embedded,
            "re-embedded items indexed without vectors"
        );
    }
}

/// Chunks re-indexed per batch.
const REINDEX_BATCH: usize = 16;

/// One re-index pass, at the lowest priority: off on low power, waiting out interactive model
/// use before each batch, stopping when the re-index is stopped. Switches when nothing is pending.
async fn reindex_once(inner: &Arc<Inner>) {
    let current = || {
        lock(&inner.reindex)
            .as_ref()
            .map(|r| (r.provider.clone(), r.on_done.clone()))
    };
    let Some((provider, on_done)) = current() else {
        return;
    };
    let same =
        |p: &Arc<dyn nugget_infer::Provider>| current().is_some_and(|(c, _)| Arc::ptr_eq(&c, p));
    let fail = |msg: String| {
        tracing::warn!(error = %msg, "re-indexing paused");
        if let Some(r) = lock(&inner.reindex).as_mut() {
            r.error = Some(cap_error(msg));
        }
    };
    let mut after = 0;
    loop {
        if inner.power.constrained() {
            tracing::debug!("re-indexing deferred: low power");
            return;
        }
        if inner.foreground.busy() {
            inner.foreground.idle().await;
        }
        if !same(&provider) {
            return;
        }
        let store = inner.store.clone();
        let page = tokio::task::spawn_blocking(move || -> Result<_> {
            let target = store.reindex_target()?;
            Ok((target, store.reindex_pending(after, REINDEX_BATCH)?))
        })
        .await
        .map_err(join_err)
        .and_then(|r| r);
        let (target, page) = match page {
            Ok((Some(t), p)) => (t, p),
            Ok((None, _)) => return,
            Err(e) => return fail(format!("read chunks: {e}")),
        };
        let Some(&(last, _)) = page.last() else {
            // Nothing pending past `after`; a full sweep from the start decides.
            if after > 0 {
                after = 0;
                continue;
            }
            let switched = {
                // Under the write lock: writes check the index's model before they store vectors.
                let me = inner.clone();
                tokio::task::spawn_blocking(move || {
                    let _w = lock(&me.write_lock);
                    me.store.finish_reindex()
                })
                .await
                .map_err(join_err)
                .and_then(|r| r)
            };
            match switched {
                Ok(Some(t)) if same(&provider) => {
                    tracing::info!(model = %t.model, "search index switched to the new embedding model");
                    lock(&inner.reindex).take();
                    on_done(t);
                }
                Ok(_) => {}
                Err(e) => fail(format!("switch: {e}")),
            }
            return;
        };
        after = last;
        let texts: Vec<String> = page.iter().map(|(_, t)| t.clone()).collect();
        let texts = if target.document_prefix.is_empty() {
            texts
        } else {
            nugget_infer::with_prefix(&target.document_prefix, &texts)
        };
        let vectors =
            match tokio::time::timeout(inner.cfg.embed_timeout(), provider.embed(&texts)).await {
                Ok(Ok(v)) if v.len() == page.len() => v,
                Ok(Ok(v)) => return fail(format!("{} vectors for {} inputs", v.len(), page.len())),
                Ok(Err(e)) => return fail(e.to_string()),
                Err(_) => return fail("embedding timed out".into()),
            };
        let staged: Vec<_> = page.iter().map(|(id, _)| *id).zip(vectors).collect();
        let store = inner.store.clone();
        let put = tokio::task::spawn_blocking(move || store.put_reindexed(&staged)).await;
        if let Err(e) = put.map_err(join_err).and_then(|r| r) {
            return fail(format!("store: {e}"));
        }
        if let Some(r) = lock(&inner.reindex).as_mut() {
            r.error = None;
        }
    }
}

fn record_failure(inner: &Inner, item: Option<&ItemRef>, msg: String) {
    let mut st = lock(&inner.reembed_state);
    st.stats.failed += 1;
    st.stats.last_error = Some(cap_error(msg));
    if let Some(item) = item
        && let Some((_, n)) = st.skip.get_mut(item)
    {
        *n = n.saturating_add(1);
    }
}

/// Whether the item must stay away from cloud providers (fails closed, like retrieval).
fn is_sensitive(item: &Item) -> bool {
    item.metadata.get("sensitive") != Some(&Value::Bool(false))
}

/// All chunk texts of an item, in order (positions are contiguous from 0).
pub(crate) fn chunk_texts(store: &nugget_store::Store, item: &ItemRef) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut from = 0u32;
    loop {
        let ords: Vec<u32> = (from..from + ORD_PAGE).collect();
        let page = store.get_chunks_by_ord(item, &ords)?;
        let n = page.len();
        out.extend(page.into_iter().map(|c| c.text));
        if n < ORD_PAGE as usize {
            return Ok(out);
        }
        from += ORD_PAGE;
    }
}

async fn reembed_item(
    inner: &Arc<Inner>,
    router: &Arc<nugget_infer::Router>,
    item: &ItemRef,
) -> Outcome {
    // 1. Snapshot: the item and its chunk texts.
    let snapshot = {
        let store = inner.store.clone();
        let item = item.clone();
        tokio::task::spawn_blocking(move || -> Result<Option<(Item, Vec<String>)>> {
            let Some(it) = store.get_item(&item)? else {
                return Ok(None);
            };
            let texts = chunk_texts(&store, &item)?;
            Ok(Some((it, texts)))
        })
        .await
        .map_err(join_err)
        .and_then(|r| r)
    };
    let (stored, texts) = match snapshot {
        Ok(Some(s)) if !s.1.is_empty() => s,
        Ok(_) => return Outcome::Skipped,
        Err(e) => return Outcome::Failed(format!("read item: {e}")),
    };
    {
        let mut st = lock(&inner.reembed_state);
        match st.skip.get(item) {
            Some((hash, n)) if *hash == stored.content_hash => {
                if *n >= MAX_ITEM_FAILURES {
                    return Outcome::Skipped;
                }
            }
            _ => {
                st.skip
                    .insert(item.clone(), (stored.content_hash.clone(), 0));
            }
        }
    }

    // 2. Embed with the providers the pipeline would use for this item.
    let providers = ai::embedding_providers(
        router,
        is_sensitive(&stored),
        inner.cfg.allow_cloud_embeddings,
    );
    if providers.is_empty() {
        return Outcome::Skipped;
    }
    let (provider, model, vectors) = match ai::embed(
        &inner.store,
        router,
        &providers,
        item,
        &texts,
        inner.cfg.embed_batch,
        inner.cfg.embed_timeout(),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return Outcome::Failed(e.to_string()),
    };

    // 3. Write, only if nothing changed meanwhile.
    let me = inner.clone();
    let item_ref = item.clone();
    let res = tokio::task::spawn_blocking(move || {
        write_back(
            &me,
            &item_ref,
            &stored,
            &texts,
            (&provider, &model),
            vectors,
        )
    })
    .await
    .map_err(join_err)
    .and_then(|r| r);
    match res {
        Ok(true) => Outcome::Embedded,
        Ok(false) => Outcome::Skipped,
        Err(Error::Invalid(msg)) => {
            // E.g. the index holds vectors of another dimension: never retry this content.
            if let Some(e) = lock(&inner.reembed_state).skip.get_mut(item) {
                e.1 = u32::MAX;
            }
            tracing::debug!(item = %item.key, error = %msg, "item left text-only");
            Outcome::Skipped
        }
        Err(e) => Outcome::Failed(format!("store: {e}")),
    }
}

/// Store `vectors` for the item read as `stored` with chunk texts `texts`, under the write lock.
fn write_back(
    inner: &Inner,
    item: &ItemRef,
    stored: &Item,
    texts: &[String],
    (provider, model): (&ProviderId, &str),
    vectors: Vec<Vec<f32>>,
) -> Result<bool> {
    let store = &inner.store;
    if let Some(dim) = store.embedding_dim()?
        && vectors.first().is_some_and(|v| v.len() != dim)
    {
        return Err(Error::Invalid(format!(
            "embedding dimension {} does not match the index ({dim})",
            vectors.first().map_or(0, Vec::len)
        )));
    }
    let _w = lock(&inner.write_lock);
    // The index switched embedding models meanwhile: try again with the new one later.
    if store.embedding_model()?.is_some_and(|m| m != model) {
        return Ok(false);
    }
    let Some(now) = store.get_item(item)? else {
        return Ok(false);
    };
    if now.content_hash != stored.content_hash
        || now.updated_at != stored.updated_at
        || now.version != stored.version
        || chunk_texts(store, item)? != texts
    {
        return Ok(false);
    }
    let mut metadata = now.metadata.clone();
    if let Value::Object(m) = &mut metadata {
        m.insert(
            "embedding_provider".into(),
            Value::String(provider.as_str().to_owned()),
        );
        m.remove("embed_error");
    }
    let mut new = NewItem::new(
        item.source.as_str(),
        item.key.as_str(),
        now.content_hash.clone(),
    )
    .with_metadata(metadata);
    new.title = now.title.clone();
    new.mime = now.mime.clone();
    new.text = now.text.clone();
    new.source_modified_at = now.source_modified_at;
    new.version = now.version.clone();
    let chunks: Vec<NewChunk> = texts
        .iter()
        .zip(vectors)
        .map(|(t, v)| NewChunk::embedded(t.clone(), v))
        .collect();
    if store.upsert_item_with_chunks(&new, &chunks)? == UpsertOutcome::Unchanged {
        // Same fields (the metadata already named the provider): replace the chunks alone.
        store.replace_chunks(item, &chunks)?;
    }
    Ok(true)
}
