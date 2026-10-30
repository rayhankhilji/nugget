//! The re-embed job: gives vectors to items that were indexed text-only.
//!
//! Items are stored without embeddings when no embedding provider was routable at the time
//! (typically: the local embedding server was not installed or not started yet), or when
//! embedding failed. Without this job they would never become reachable by vector search.
//!
//! The job runs at start, whenever [`Ingest::reembed_now`](crate::Ingest::reembed_now) is
//! called (nuggetd calls it when a local runtime becomes ready) and every
//! [`IngestConfig::reembed_interval_ms`](crate::IngestConfig::reembed_interval_ms). Each run:
//! 1. does nothing unless embeddings are on, an `Embedding`-tier provider is routable and the
//!    power policy allows heavy work;
//! 2. pages through [`Store::items_missing_embeddings`](nugget_store::Store) and, per item,
//!    embeds every chunk with the providers the pipeline would use (the same privacy rules:
//!    local providers, `Cloud` ones only when allowed **and** the item is not sensitive; an
//!    item without a `sensitive: false` flag counts as sensitive);
//! 3. writes the vectors only if the item is unchanged since it was read, under the ingest
//!    write lock, atomically with its metadata (`embedding_provider` set, `embed_error`
//!    cleared). A concurrent pipeline write or delete therefore always wins.
//!
//! Items that cannot be embedded (dimension mismatch with the index, or repeated failures) are
//! remembered by content hash and skipped until their content changes. A run stops after a few
//! consecutive provider failures and is retried on the next trigger.

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
    /// `u32::MAX` failures means permanently skipped.
    pub skip: HashMap<ItemRef, (String, u32)>,
}

/// The job loop; ends on shutdown.
pub(crate) async fn run(inner: Arc<Inner>) {
    loop {
        tokio::select! {
            biased;
            _ = inner.shutdown.cancelled() => return,
            _ = run_once(&inner) => {}
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
fn chunk_texts(store: &nugget_store::Store, item: &ItemRef) -> Result<Vec<String>> {
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
    let (provider, vectors) = match ai::embed(
        &inner.store,
        router,
        &providers,
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
        write_back(&me, &item_ref, &stored, &texts, &provider, vectors)
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

/// Store `vectors` for the item read as `stored` with chunk texts `texts`, under the write
/// lock. Returns `false` (nothing written) if the item changed or disappeared meanwhile.
fn write_back(
    inner: &Inner,
    item: &ItemRef,
    stored: &Item,
    texts: &[String],
    provider: &ProviderId,
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
