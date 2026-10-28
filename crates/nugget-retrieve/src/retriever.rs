//! The [`Retriever`]: query embedding, over-fetched search, grouping, MMR and packing.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use nugget_core::ipc::ContextResponse;
use nugget_core::{Error, Result};
use nugget_infer::{Provider, Router};
use nugget_store::{Chunk, ChunkId, Item, Store};
use time::OffsetDateTime;

use crate::config::RetrieveConfig;
use crate::embed::{embed_first, embedding_candidates, validate_vectors};
use crate::pack::{PackItem, PackOptions, pack};
use crate::rank::{
    Candidate, cosine, group_by_item, jaccard, mmr_select, recency_factor, text_query, token_set,
};
use crate::types::{
    PackedContext, RetrievalMode, RetrievedItem, SearchOptions, SearchResults, Similarity,
    Strategy, TextOnlyReason,
};

/// Longest accepted `url` metadata value.
const MAX_URL_LEN: usize = 4096;
/// Store limit on ids per `get_chunks` / `get_embeddings` call.
const GET_CHUNKS_BATCH: usize = nugget_store::MAX_CHUNK_IDS;

/// Retrieval over a [`Store`], optionally with query embeddings from a [`Router`].
///
/// Store calls are synchronous SQLite queries made from the async methods; async callers that
/// care about blocking a runtime thread should run searches on a blocking-friendly runtime or
/// thread (they are typically a few milliseconds).
#[derive(Clone)]
pub struct Retriever {
    store: Store,
    router: Option<Arc<Router>>,
    config: RetrieveConfig,
    now: Option<OffsetDateTime>,
}

impl std::fmt::Debug for Retriever {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Retriever")
            .field("has_router", &self.router.is_some())
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// A ranked item with what packing and result mapping need.
struct Ranked {
    cand: Candidate,
    url: Option<String>,
}

struct Ranking {
    mode: RetrievalMode,
    similarity: Similarity,
    items: Vec<Ranked>,
}

impl Retriever {
    pub fn new(store: Store, router: Option<Arc<Router>>, config: RetrieveConfig) -> Self {
        Self {
            store,
            router,
            config,
            now: None,
        }
    }

    /// Fix "now" for the recency boost (tests, evaluation). Defaults to the system clock.
    pub fn with_reference_time(mut self, now: OffsetDateTime) -> Self {
        self.now = Some(now);
        self
    }

    pub fn config(&self) -> &RetrieveConfig {
        &self.config
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Search with the configured defaults: hybrid when the query can be embedded, grouped by
    /// item, diversified with MMR. At most `limit` items.
    pub async fn search(&self, query: &str, limit: usize) -> Result<SearchResults> {
        self.search_with(query, limit, SearchOptions::default())
            .await
    }

    /// [`search`](Self::search) with per-call overrides.
    pub async fn search_with(
        &self,
        query: &str,
        limit: usize,
        opts: SearchOptions,
    ) -> Result<SearchResults> {
        let ranking = self.rank(query, limit, opts).await?;
        let items = ranking
            .items
            .into_iter()
            .map(|r| RetrievedItem {
                item: r.cand.item,
                title: r.cand.title,
                url: r.url,
                chunk_id: r.cand.chunk_id,
                snippet: r.cand.snippet,
                score: r.cand.rel,
                matches: r.cand.matches,
            })
            .collect();
        Ok(SearchResults {
            mode: ranking.mode,
            similarity: ranking.similarity,
            items,
        })
    }

    /// Pack the most relevant, diverse passages for `query` into at most `budget_tokens`
    /// (estimated with `nugget_extract::estimate_tokens`), each prefixed `[n] Title (source)`.
    pub async fn build_context(&self, query: &str, budget_tokens: u32) -> Result<ContextResponse> {
        Ok(self
            .pack_context(query, budget_tokens as usize)
            .await?
            .response)
    }

    /// [`build_context`](Self::build_context) with diagnostics (mode, tokens used, chunks per
    /// passage).
    pub async fn pack_context(&self, query: &str, budget_tokens: usize) -> Result<PackedContext> {
        let ranking = self
            .rank(
                query,
                self.config.context_max_items,
                SearchOptions::default(),
            )
            .await?;
        let items = if budget_tokens == 0 {
            Vec::new()
        } else {
            self.pack_items(&ranking.items)?
        };
        let (response, passages) = pack(
            &items,
            PackOptions {
                budget: budget_tokens,
                expand_neighbours: self.config.expand_neighbours,
                include_secondary: self.config.include_secondary_matches,
                min_truncated_tokens: self.config.min_truncated_tokens,
            },
        );
        let tokens = nugget_extract::estimate_tokens(&response.text);
        Ok(PackedContext {
            response,
            mode: ranking.mode,
            similarity: ranking.similarity,
            tokens,
            passages,
        })
    }

    // ---- pipeline ----

    async fn rank(&self, query: &str, limit: usize, opts: SearchOptions) -> Result<Ranking> {
        let mmr_on = opts.mmr.unwrap_or(self.config.mmr);
        if limit == 0 || !query.chars().any(char::is_alphanumeric) {
            return Ok(Ranking {
                mode: RetrievalMode::TextOnly {
                    reason: TextOnlyReason::Requested,
                },
                similarity: Similarity::None,
                items: Vec::new(),
            });
        }

        // 1. Query embedding (or why not).
        let embedded = match opts.strategy {
            Strategy::TextOnly => Err(TextOnlyReason::Requested),
            Strategy::Auto | Strategy::VectorOnly => self.embed_query(query).await,
        };

        // 2. Over-fetched candidate chunks.
        let n = limit
            .saturating_mul(self.config.overfetch.max(1))
            .max(self.config.min_candidates)
            .min(self.config.max_candidates.max(1))
            .max(limit);
        // Each list's contribution to rank fusion is tied to what was asked for (see
        // `RetrieveConfig::fusion_depth_factor`), not to the over-fetched candidate count.
        let fusion_depth = limit
            .saturating_mul(self.config.fusion_depth_factor.max(1))
            .max(self.config.min_fusion_depth)
            .max(limit);
        // Function words would match nearly every chunk in the OR-ed full-text query and, through
        // rank fusion, crowd out passages only the vector leg found.
        let fts = text_query(query);
        let (mode, provider, hits) = match (opts.strategy, embedded) {
            (Strategy::VectorOnly, Err(reason)) => {
                return Err(Error::Unsupported(format!(
                    "vector-only search needs a query embedding: {reason:?}"
                )));
            }
            (Strategy::VectorOnly, Ok((p, v))) => {
                let hits = self.store.search_vector(&v, n)?;
                (
                    RetrievalMode::VectorOnly {
                        provider: p.id().clone(),
                    },
                    Some(p),
                    hits,
                )
            }
            (_, Ok((p, v))) => {
                match self
                    .store
                    .search_hybrid_with_depth(&fts, Some(&v), n, fusion_depth)
                {
                    Ok(hits) => (
                        RetrievalMode::Hybrid {
                            provider: p.id().clone(),
                        },
                        Some(p),
                        hits,
                    ),
                    // The dimension was checked, but the store may have changed since.
                    Err(Error::Invalid(msg)) => (
                        RetrievalMode::TextOnly {
                            reason: TextOnlyReason::DimensionMismatch(msg),
                        },
                        None,
                        self.store.search_text(&fts, n)?,
                    ),
                    Err(e) => return Err(e),
                }
            }
            (_, Err(reason)) => (
                RetrievalMode::TextOnly { reason },
                None,
                self.store.search_text(&fts, n)?,
            ),
        };

        // 3. Group by item.
        let mut cands = group_by_item(hits, self.config.max_matches_per_item);

        // 4. The pool MMR chooses from (just the top `limit` without MMR).
        let pool_len = if mmr_on {
            limit
                .saturating_mul(self.config.mmr_pool_factor.max(1))
                .min(self.config.mmr_pool_max.max(limit))
        } else {
            limit
        };
        cands.truncate(pool_len);
        let items_meta = self.load_items(&cands)?;

        // 5. Recency boost.
        if let Some(boost) = self.config.recency {
            let now = self.now.unwrap_or_else(OffsetDateTime::now_utc);
            for (c, meta) in cands.iter_mut().zip(&items_meta) {
                if let Some(item) = meta {
                    let at = item.source_modified_at.unwrap_or(item.updated_at);
                    let age_days = (now - at).as_seconds_f64() / 86_400.0;
                    c.rel *= recency_factor(age_days, &boost);
                }
            }
            let max = cands.iter().map(|c| c.rel).fold(0.0f32, f32::max);
            if max > 0.0 {
                for c in &mut cands {
                    c.rel /= max;
                }
            }
        }
        let mut pool: Vec<Ranked> = cands
            .into_iter()
            .zip(items_meta)
            .map(|(cand, meta)| Ranked {
                url: meta.as_ref().and_then(item_url),
                cand,
            })
            .collect();
        if self.config.recency.is_some() {
            // Stable: equal relevance keeps search order.
            pool.sort_by(|a, b| b.cand.rel.total_cmp(&a.cand.rel));
        }

        // 6. Diversify.
        let (order, similarity) = if mmr_on && pool.len() > 1 {
            self.diversify(&pool, limit, provider.as_ref()).await?
        } else {
            ((0..pool.len().min(limit)).collect(), Similarity::None)
        };
        let mut slots: Vec<Option<Ranked>> = pool.into_iter().map(Some).collect();
        let items = order.into_iter().filter_map(|i| slots[i].take()).collect();
        Ok(Ranking {
            mode,
            similarity,
            items,
        })
    }

    /// Embed the query with an eligible `Embedding`-tier provider.
    async fn embed_query(
        &self,
        query: &str,
    ) -> std::result::Result<(Arc<dyn Provider>, Vec<f32>), TextOnlyReason> {
        let Some(router) = &self.router else {
            return Err(TextOnlyReason::NoEmbedder);
        };
        let (providers, cloud_skipped) =
            embedding_candidates(router, self.config.allow_cloud_embedding);
        if providers.is_empty() {
            return Err(if cloud_skipped {
                TextOnlyReason::CloudNotAllowed
            } else {
                TextOnlyReason::NoEmbedder
            });
        }
        // Nothing to compare against: don't send the query anywhere.
        let dim = match self.store.embedding_dim() {
            Ok(Some(d)) => d,
            Ok(None) => return Err(TextOnlyReason::NoVectorsIndexed),
            Err(e) => return Err(TextOnlyReason::EmbeddingFailed(e.to_string())),
        };
        let (p, mut vectors) = embed_first(router, &providers, &[query.to_owned()])
            .await
            .map_err(TextOnlyReason::EmbeddingFailed)?;
        let v = vectors.pop().unwrap_or_default();
        if v.len() != dim {
            return Err(TextOnlyReason::DimensionMismatch(format!(
                "provider {} returned dimension {}, the store holds {dim}",
                p.id(),
                v.len()
            )));
        }
        Ok((p, v))
    }

    /// MMR order over `pool` (indices) and the similarity used.
    ///
    /// When the query was embedded (`provider` is set) and [`RetrieveConfig::mmr_vectors`] is on,
    /// passages are compared by their **stored** chunk embeddings, read back from the store, so
    /// nothing is re-embedded. Only passages stored without a vector (e.g. indexed while no
    /// embedder was available) are embedded, in one batch with the query's provider. Otherwise,
    /// or if that fails, MMR uses token-set Jaccard.
    async fn diversify(
        &self,
        pool: &[Ranked],
        limit: usize,
        provider: Option<&Arc<dyn Provider>>,
    ) -> Result<(Vec<usize>, Similarity)> {
        let ids: Vec<ChunkId> = pool.iter().map(|r| r.cand.chunk_id).collect();
        let chunks = self.chunks_by_id(&ids)?;
        let texts: Vec<String> = pool
            .iter()
            .map(|r| {
                chunks
                    .get(&r.cand.chunk_id)
                    .map_or_else(|| r.cand.snippet.clone(), |c| c.text.clone())
            })
            .collect();
        let rel: Vec<f32> = pool.iter().map(|r| r.cand.rel).collect();
        let lambda = self.config.mmr_lambda;

        if let (Some(p), true) = (provider, self.config.mmr_vectors)
            && let Some(vectors) = self.pool_vectors(&ids, &texts, p).await?
        {
            let sim = |i: usize, j: usize| cosine(&vectors[i], &vectors[j]);
            return Ok((mmr_select(&rel, limit, lambda, &sim), Similarity::Vector));
        }
        let sets: Vec<BTreeSet<String>> = texts.iter().map(|t| token_set(t)).collect();
        let sim = |i: usize, j: usize| jaccard(&sets[i], &sets[j]);
        Ok((mmr_select(&rel, limit, lambda, &sim), Similarity::Lexical))
    }

    /// One vector per pool passage: stored embeddings first, the rest embedded with `p`.
    /// `None` (lexical MMR) if the missing ones cannot be embedded or the vectors disagree in
    /// dimension.
    async fn pool_vectors(
        &self,
        ids: &[ChunkId],
        texts: &[String],
        p: &Arc<dyn Provider>,
    ) -> Result<Option<Vec<Vec<f32>>>> {
        let mut stored = self.embeddings_by_id(ids)?;
        let mut vectors: Vec<Option<Vec<f32>>> = ids.iter().map(|id| stored.remove(id)).collect();
        let missing: Vec<usize> = (0..ids.len()).filter(|&i| vectors[i].is_none()).collect();
        if !missing.is_empty() {
            let inputs: Vec<String> = missing
                .iter()
                .map(|&i| {
                    texts[i]
                        .chars()
                        .take(self.config.mmr_embed_chars.max(1))
                        .collect()
                })
                .collect();
            match p.embed(&inputs).await {
                Ok(fresh) if validate_vectors(&fresh, inputs.len()).is_ok() => {
                    for (i, v) in missing.into_iter().zip(fresh) {
                        vectors[i] = Some(v);
                    }
                }
                Ok(_) => {
                    tracing::warn!(provider = %p.id(), "bad passage embeddings; lexical MMR");
                    return Ok(None);
                }
                Err(e) => {
                    tracing::warn!(provider = %p.id(), error = %e, "passage embedding failed; lexical MMR");
                    return Ok(None);
                }
            }
        }
        let vectors: Vec<Vec<f32>> = vectors.into_iter().flatten().collect();
        let dim = vectors.first().map_or(0, Vec::len);
        if vectors.len() != ids.len() || vectors.iter().any(|v| v.len() != dim) {
            tracing::warn!(provider = %p.id(), "passage vectors disagree in dimension; lexical MMR");
            return Ok(None);
        }
        Ok(Some(vectors))
    }

    fn embeddings_by_id(&self, ids: &[ChunkId]) -> Result<HashMap<ChunkId, Vec<f32>>> {
        let mut out = HashMap::with_capacity(ids.len());
        for batch in ids.chunks(GET_CHUNKS_BATCH) {
            out.extend(self.store.get_embeddings(batch)?);
        }
        Ok(out)
    }

    fn load_items(&self, cands: &[Candidate]) -> Result<Vec<Option<Item>>> {
        cands.iter().map(|c| self.store.get_item(&c.item)).collect()
    }

    fn chunks_by_id(&self, ids: &[ChunkId]) -> Result<HashMap<ChunkId, Chunk>> {
        let mut out = HashMap::with_capacity(ids.len());
        for batch in ids.chunks(GET_CHUNKS_BATCH) {
            for c in self.store.get_chunks(batch)? {
                out.insert(c.id, c);
            }
        }
        Ok(out)
    }

    /// Fetch primary, neighbouring and secondary chunks for packing.
    ///
    /// Neighbours are the chunks at `ord ± 1` of the same item, looked up by position with
    /// [`Store::neighbours`].
    fn pack_items(&self, ranked: &[Ranked]) -> Result<Vec<PackItem>> {
        let mut ids: Vec<ChunkId> = Vec::new();
        for r in ranked {
            ids.push(r.cand.chunk_id);
            if self.config.include_secondary_matches {
                ids.extend(r.cand.matches.iter().map(|m| m.chunk_id));
            }
        }
        ids.sort_unstable();
        ids.dedup();
        let chunks = self.chunks_by_id(&ids)?;

        let mut out = Vec::with_capacity(ranked.len());
        for r in ranked {
            // Deleted since the search: skip it.
            let Some(primary) = chunks.get(&r.cand.chunk_id).cloned() else {
                continue;
            };
            let neighbours = if self.config.expand_neighbours {
                self.store
                    .neighbours(primary.id, 1)?
                    .into_iter()
                    .filter(|c| c.item == primary.item && c.ord != primary.ord)
                    .collect()
            } else {
                Vec::new()
            };
            let secondary = if self.config.include_secondary_matches {
                r.cand
                    .matches
                    .iter()
                    .filter_map(|m| chunks.get(&m.chunk_id))
                    .filter(|c| c.item == primary.item)
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };
            out.push(PackItem {
                item: r.cand.item.clone(),
                title: r.cand.title.clone(),
                url: r.url.clone(),
                primary,
                neighbours,
                secondary,
            });
        }
        Ok(out)
    }
}

/// The item's `url` metadata (set by ingest), if it is a sane single-line string.
fn item_url(item: &Item) -> Option<String> {
    let url = item.metadata.get("url")?.as_str()?.trim();
    let ok = !url.is_empty() && url.len() <= MAX_URL_LEN && !url.chars().any(char::is_control);
    ok.then(|| url.to_owned())
}
