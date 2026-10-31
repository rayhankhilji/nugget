//! The [`Retriever`]: query embedding, over-fetched search, grouping, reranking, MMR and
//! packing.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use nugget_core::capability::{Capabilities, Locality, Tier};
use nugget_core::ipc::{ChatMessage, ContextResponse};
use nugget_core::{Error, Result};
use nugget_infer::{ChatRequest, ChatResponse, Message, Provider, Router};
use nugget_store::{Chunk, ChunkId, Item, Store};
use time::{OffsetDateTime, UtcOffset};

use crate::config::{Fusion, RetrieveConfig};
use crate::dates::{DateRange, parse_date_query};
use crate::embed::{embed_first, embedding_candidates, validate_vectors};
use crate::pack::{PackItem, PackOptions, pack};
use crate::rank::{
    Candidate, convex_fuse, cosine, group_by_item, jaccard, mmr_select, recency_factor, text_query,
    token_set,
};
use crate::rerank::{RerankGovernor, RerankStatus, Reranker, normalise_scores};
use crate::rewrite::{
    FEATURE_REWRITE, QueryRewrite, RewriteMethod, clean_reply, heuristic_rewrite,
    looks_like_followup, system_prompt, user_prompt,
};
use crate::types::{
    ContextOptions, DateFilter, PackedContext, RetrievalMode, RetrievedItem, SearchOptions,
    SearchResults, Similarity, Strategy, TextOnlyReason,
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
    reranker: Option<Arc<dyn Reranker>>,
    /// Keeps reranking within its budget; shared by clones.
    rerank_gov: Arc<RerankGovernor>,
}

impl std::fmt::Debug for Retriever {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Retriever")
            .field("has_router", &self.router.is_some())
            .field(
                "reranker",
                &self.reranker.as_ref().map(|r| r.name().to_owned()),
            )
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// A ranked item with what packing and result mapping need.
struct Ranked {
    cand: Candidate,
    url: Option<String>,
    /// See [`is_sensitive`].
    sensitive: bool,
    /// `YYYY-MM-DD` of the source modification time, else the update time.
    date: Option<String>,
}

struct Ranking {
    mode: RetrievalMode,
    similarity: Similarity,
    items: Vec<Ranked>,
    reranked: bool,
    date: Option<DateFilter>,
}

/// Terms of the last user turn appended by the heuristic rewrite.
const HEURISTIC_TERMS: usize = 6;
/// Output cap for the rewriting model (a query is short).
const REWRITE_MAX_TOKENS: u32 = 96;

impl Retriever {
    pub fn new(store: Store, router: Option<Arc<Router>>, config: RetrieveConfig) -> Self {
        Self {
            store,
            router,
            config,
            now: None,
            reranker: None,
            rerank_gov: Arc::default(),
        }
    }

    /// The adaptive reranker's state (see [`RetrieveConfig::rerank_adaptive`]); `None` without a
    /// reranker.
    pub fn rerank_status(&self) -> Option<RerankStatus> {
        self.reranker
            .as_ref()
            .map(|_| self.rerank_gov.status(&self.config))
    }

    /// Rerank the top candidates with `reranker` (see [`crate::rerank`]).
    pub fn with_reranker(mut self, reranker: Arc<dyn Reranker>) -> Self {
        self.reranker = Some(reranker);
        self
    }

    /// Fix "now" for the recency boost and date phrases (tests, evaluation). Defaults to the
    /// system clock.
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
            reranked: ranking.reranked,
            date: ranking.date,
            rewrite: None,
        })
    }

    /// [`search`](Self::search) for the latest message of a conversation: a follow-up such as
    /// "what about the second one?" is first rewritten into a standalone query with
    /// [`rewrite_query`](Self::rewrite_query). [`SearchResults::rewrite`] says what was searched.
    pub async fn search_in_conversation(
        &self,
        history: &[ChatMessage],
        query: &str,
        limit: usize,
    ) -> Result<SearchResults> {
        let rewrite = self.rewrite_query(history, query).await;
        let mut res = self.search(&rewrite.query, limit).await?;
        res.rewrite = Some(rewrite);
        Ok(res)
    }

    /// The standalone form of `query` given the conversation so far (see [`crate::rewrite`]).
    /// Never fails: anything that goes wrong keeps the original query.
    pub async fn rewrite_query(&self, history: &[ChatMessage], query: &str) -> QueryRewrite {
        if !self.config.rewrite_followups
            || !history
                .iter()
                .any(|m| m.role != nugget_core::ipc::Role::System)
            || !looks_like_followup(query)
        {
            return QueryRewrite::unchanged(query);
        }
        let providers = self.rewrite_candidates();
        let Some(p) = providers.first() else {
            return match heuristic_rewrite(history, query, HEURISTIC_TERMS) {
                Some(q) => QueryRewrite {
                    query: q,
                    method: RewriteMethod::Heuristic,
                },
                None => QueryRewrite::unchanged(query),
            };
        };
        let max_chars = self.config.rewrite_max_chars.max(16);
        let mut req = ChatRequest::new(vec![Message::user_text(user_prompt(
            history,
            query,
            self.config.rewrite_history_turns,
        ))])
        .with_system(system_prompt(max_chars))
        .with_max_tokens(REWRITE_MAX_TOKENS)
        .with_feature(FEATURE_REWRITE);
        req.temperature = Some(0.0);
        let timeout = Duration::from_millis(self.config.rewrite_timeout_ms.max(1));
        let reply = tokio::time::timeout(timeout, async {
            ChatResponse::collect(p.chat(req).await?).await
        })
        .await;
        let router = self.router.as_ref();
        match reply {
            Ok(Ok(r)) => {
                if let Some(router) = router {
                    router.report_success(p.id());
                }
                match clean_reply(&r.text(), max_chars) {
                    Some(q) => QueryRewrite {
                        query: q,
                        method: RewriteMethod::Model {
                            provider: p.id().clone(),
                        },
                    },
                    None => {
                        tracing::warn!(provider = %p.id(), "unusable query rewrite; searching the original query");
                        QueryRewrite::unchanged(query)
                    }
                }
            }
            Ok(Err(e)) => {
                tracing::warn!(provider = %p.id(), error = %e, "query rewrite failed; searching the original query");
                if let Some(router) = router
                    && !matches!(e, Error::Invalid(_) | Error::Cancelled)
                {
                    router.report_failure(p.id());
                }
                QueryRewrite::unchanged(query)
            }
            Err(_) => {
                tracing::warn!(provider = %p.id(), "query rewrite timed out; searching the original query");
                QueryRewrite::unchanged(query)
            }
        }
    }

    /// Local chat providers for rewriting, best first: the `Light`, then `Triage`, then
    /// `Synthesis` tier, without `Cloud`-locality providers.
    fn rewrite_candidates(&self) -> Vec<Arc<dyn Provider>> {
        let Some(router) = &self.router else {
            return Vec::new();
        };
        let mut out: Vec<Arc<dyn Provider>> = Vec::new();
        for tier in [Tier::Light, Tier::Triage, Tier::Synthesis] {
            for p in router.candidates(tier, &Capabilities::default()) {
                if p.meta().locality != Locality::Cloud && !out.iter().any(|q| q.id() == p.id()) {
                    out.push(p);
                }
            }
        }
        out
    }

    /// Pack the most relevant, diverse passages for `query` into at most `budget_tokens`
    /// (estimated with `nugget_extract::estimate_tokens`), each prefixed `[n] Title (source)`.
    pub async fn build_context(&self, query: &str, budget_tokens: u32) -> Result<ContextResponse> {
        self.build_context_with(query, budget_tokens, ContextOptions::default())
            .await
    }

    /// [`build_context`](Self::build_context) with options, e.g. leaving sensitive items out
    /// of a context meant for a cloud model.
    pub async fn build_context_with(
        &self,
        query: &str,
        budget_tokens: u32,
        opts: ContextOptions,
    ) -> Result<ContextResponse> {
        Ok(self
            .pack_context_with(query, budget_tokens as usize, opts)
            .await?
            .response)
    }

    /// [`build_context`](Self::build_context) with diagnostics (mode, tokens used, chunks per
    /// passage).
    pub async fn pack_context(&self, query: &str, budget_tokens: usize) -> Result<PackedContext> {
        self.pack_context_with(query, budget_tokens, ContextOptions::default())
            .await
    }

    /// [`pack_context`](Self::pack_context) with options.
    pub async fn pack_context_with(
        &self,
        query: &str,
        budget_tokens: usize,
        opts: ContextOptions,
    ) -> Result<PackedContext> {
        let max_items = self.config.context_max_items;
        // When sensitive items are dropped, rank a deeper list so the context can be refilled.
        let depth = if opts.exclude_sensitive {
            max_items.saturating_mul(2)
        } else {
            max_items
        };
        let mut ranking = self.rank(query, depth, SearchOptions::default()).await?;
        let mut excluded_sensitive: u32 = 0;
        if opts.exclude_sensitive {
            let mut kept = Vec::with_capacity(max_items);
            for r in ranking.items {
                if kept.len() >= max_items {
                    break;
                }
                if r.sensitive {
                    excluded_sensitive = excluded_sensitive.saturating_add(1);
                } else {
                    kept.push(r);
                }
            }
            ranking.items = kept;
        }
        let items = if budget_tokens == 0 {
            Vec::new()
        } else {
            self.pack_items(&ranking.items)?
        };
        let (mut response, passages) = pack(
            &items,
            PackOptions {
                budget: budget_tokens,
                expand_neighbours: self.config.expand_neighbours,
                include_secondary: self.config.include_secondary_matches,
                min_truncated_tokens: self.config.min_truncated_tokens,
            },
        );
        response.excluded_sensitive = excluded_sensitive;
        let tokens = nugget_extract::estimate_tokens(&response.text);
        Ok(PackedContext {
            response,
            mode: ranking.mode,
            similarity: ranking.similarity,
            tokens,
            passages,
            reranked: ranking.reranked,
            date: ranking.date,
        })
    }

    // ---- pipeline ----

    fn now(&self) -> OffsetDateTime {
        self.now.unwrap_or_else(OffsetDateTime::now_utc)
    }

    /// Rank with any date phrase handled: search the query without the phrase and keep items
    /// modified in its range; if none match, search the original query unfiltered.
    async fn rank(&self, query: &str, limit: usize, opts: SearchOptions) -> Result<Ranking> {
        let parsed = if self.config.dates && limit > 0 {
            let minutes = self.config.date_utc_offset_minutes.clamp(-18 * 60, 18 * 60);
            let offset = UtcOffset::from_whole_seconds(minutes * 60).unwrap_or(UtcOffset::UTC);
            parse_date_query(query, self.now(), offset)
        } else {
            None
        };
        let Some(dq) = parsed else {
            return self.rank_in(query, limit, opts, None).await;
        };
        // A query that is only a date has nothing else to search for.
        let searched = if dq.cleaned.chars().any(char::is_alphanumeric) {
            dq.cleaned.as_str()
        } else {
            query
        };
        let mut ranking = self.rank_in(searched, limit, opts, Some(dq.range)).await?;
        let applied = !ranking.items.is_empty();
        if !applied {
            ranking = self.rank_in(query, limit, opts, None).await?;
        }
        ranking.date = Some(DateFilter {
            phrase: dq.phrase,
            range: dq.range,
            applied,
        });
        Ok(ranking)
    }

    async fn rank_in(
        &self,
        query: &str,
        limit: usize,
        opts: SearchOptions,
        range: Option<DateRange>,
    ) -> Result<Ranking> {
        let mmr_on = opts.mmr.unwrap_or(self.config.mmr);
        if limit == 0 || !query.chars().any(char::is_alphanumeric) {
            return Ok(Ranking {
                mode: RetrievalMode::TextOnly {
                    reason: TextOnlyReason::Requested,
                },
                similarity: Similarity::None,
                items: Vec::new(),
                reranked: false,
                date: None,
            });
        }

        // 1. Query embedding (or why not).
        let embedded = match opts.strategy {
            Strategy::TextOnly => Err(TextOnlyReason::Requested),
            Strategy::Auto | Strategy::VectorOnly => self.embed_query(query).await,
        };

        // 2. Over-fetched candidate chunks.
        let mut n = limit
            .saturating_mul(self.config.overfetch.max(1))
            .max(self.config.min_candidates)
            .min(self.config.max_candidates.max(1))
            .max(limit);
        if range.is_some() {
            // The date filter drops candidates: start from as many as allowed.
            n = n.max(self.config.max_candidates);
        }
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
                let fused = match self.config.fusion {
                    Fusion::Rrf => {
                        self.store
                            .search_hybrid_with_depth(&fts, Some(&v), n, fusion_depth)
                    }
                    Fusion::Convex => self.store.search_text(&fts, fusion_depth).and_then(|t| {
                        // sqlite-vec caps kNN at 4096.
                        let v = self.store.search_vector(&v, fusion_depth.min(4096))?;
                        Ok(convex_fuse(t, v, self.config.fusion_text_weight, n))
                    }),
                };
                match fused {
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

        // Voice samples (the texts the user sent with Double Tap) are never knowledge.
        let hits: Vec<_> = hits
            .into_iter()
            .filter(|h| !nugget_core::ids::is_private_source(h.item.source.as_str()))
            .collect();

        // 3. Group by item.
        let mut cands = group_by_item(hits, self.config.max_matches_per_item);

        // 4. The pool MMR chooses from (just the top `limit` without MMR), and the reranker's.
        let pool_len = if mmr_on {
            limit
                .saturating_mul(self.config.mmr_pool_factor.max(1))
                .min(self.config.mmr_pool_max.max(limit))
        } else {
            limit
        };
        let rerank_len = if self.reranker.is_some() {
            self.config.rerank_pool
        } else {
            0
        };
        let keep = pool_len.max(rerank_len);
        let (mut cands, items_meta) = match range {
            // Date filter: by source modification time, else the store's update time. Filters on
            // the dates alone (one batched lookup), then loads only the items that are kept.
            Some(range) => {
                let refs: Vec<_> = cands.iter().map(|c| c.item.clone()).collect();
                let dates = self.store.item_dates(&refs)?;
                let kept: Vec<Candidate> = cands
                    .into_iter()
                    .filter(|c| {
                        dates.get(&c.item).is_some_and(|d| {
                            range.contains(d.source_modified_at.unwrap_or(d.updated_at))
                        })
                    })
                    .take(keep)
                    .collect();
                let metas = self.load_items(&kept)?;
                (kept, metas)
            }
            None => {
                cands.truncate(keep);
                let metas = self.load_items(&cands)?;
                (cands, metas)
            }
        };
        if range.is_some() {
            // Relevance is relative to the best candidate that is left.
            crate::rank::normalise(&mut cands);
        }

        // 5. Recency boost.
        if let Some(boost) = self.config.recency {
            let now = self.now();
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
                // An item deleted since the search cannot be checked: treat it as sensitive.
                sensitive: meta.as_ref().is_none_or(|i| is_sensitive(&i.metadata)),
                date: meta
                    .as_ref()
                    .map(|i| iso_date(i.source_modified_at.unwrap_or(i.updated_at))),
                cand,
            })
            .collect();
        if self.config.recency.is_some() {
            // Stable: equal relevance keeps search order.
            pool.sort_by(|a, b| b.cand.rel.total_cmp(&a.cand.rel));
        }

        // 6. Rerank the head of the list.
        let plan = match &self.reranker {
            Some(_) if rerank_len > 0 && !pool.is_empty() => self.rerank_gov.plan(&self.config),
            _ => None,
        };
        let reranked = match (&self.reranker, plan) {
            (Some(reranker), Some(n)) if n > 0 => {
                self.rerank(reranker.as_ref(), query, &mut pool, n).await?
            }
            _ => false,
        };
        pool.truncate(pool_len);

        // 7. Diversify.
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
            reranked,
            date: None,
        })
    }

    /// Score the first `n` of `pool` with `reranker` and blend the scores into their relevance
    /// ([`RetrieveConfig::rerank_weight`]); the rest count as scored 0, so they stay behind the
    /// reranked candidates that the first stage also preferred. Re-sorts `pool` (stable) and
    /// returns whether reranking was applied. Any reranker error, a bad response or a timeout
    /// leaves `pool` as it was.
    async fn rerank(
        &self,
        reranker: &dyn Reranker,
        query: &str,
        pool: &mut [Ranked],
        n: usize,
    ) -> Result<bool> {
        let n = n.min(pool.len());
        // Sensitive items never go to a cloud reranker (as for cloud embeddings and contexts):
        // only the others are sent, and a withheld item keeps its first-stage relevance.
        let cloud = reranker.locality() == Locality::Cloud;
        let sent: Vec<usize> = (0..n).filter(|&i| !(cloud && pool[i].sensitive)).collect();
        if sent.is_empty() {
            return Ok(false);
        }
        let ids: Vec<ChunkId> = sent.iter().map(|&i| pool[i].cand.chunk_id).collect();
        let chunks = self.chunks_by_id(&ids)?;
        let passages: Vec<String> = sent
            .iter()
            .map(|&i| &pool[i])
            .map(|r| {
                let text = chunks
                    .get(&r.cand.chunk_id)
                    .map_or(r.cand.snippet.as_str(), |c| c.text.as_str());
                match r.cand.title.as_deref().map(str::trim) {
                    Some(t) if !t.is_empty() => format!("{t}\n\n{text}"),
                    _ => text.to_owned(),
                }
            })
            .collect();
        let items: Vec<_> = sent.iter().map(|&i| pool[i].cand.item.clone()).collect();
        let timeout = Duration::from_millis(self.config.rerank_timeout_ms.max(1));
        let call = reranker.rerank_items(query, &passages, &items);
        let started = tokio::time::Instant::now();
        let scores = match tokio::time::timeout(timeout, call).await {
            Ok(Ok(s)) if s.len() == sent.len() && s.iter().all(|x| x.is_finite()) => {
                self.rerank_gov
                    .completed(&self.config, sent.len(), started.elapsed());
                s
            }
            Ok(Ok(s)) => {
                tracing::warn!(
                    reranker = reranker.name(),
                    got = s.len(),
                    expected = sent.len(),
                    "bad rerank scores; keeping first-stage order"
                );
                return Ok(false);
            }
            Ok(Err(e)) => {
                tracing::warn!(reranker = reranker.name(), error = %e, "reranking failed; keeping first-stage order");
                return Ok(false);
            }
            Err(_) => {
                tracing::warn!(
                    reranker = reranker.name(),
                    "reranking timed out; keeping first-stage order"
                );
                self.rerank_gov.timed_out(&self.config, sent.len());
                return Ok(false);
            }
        };
        let w = if self.config.rerank_weight.is_finite() {
            self.config.rerank_weight.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let scores = normalise_scores(&scores);
        let mut by_pos: Vec<Option<f32>> = vec![None; pool.len()];
        for (&i, &s) in sent.iter().zip(&scores) {
            by_pos[i] = Some(s);
        }
        for (i, r) in pool.iter_mut().enumerate() {
            let s = match by_pos[i] {
                Some(s) => s,
                // Withheld from a cloud reranker: neither promoted nor demoted.
                None if i < n => r.cand.rel,
                None => 0.0,
            };
            r.cand.rel = w * s + (1.0 - w) * r.cand.rel;
        }
        pool.sort_by(|a, b| b.cand.rel.total_cmp(&a.cand.rel));
        let max = pool.first().map_or(0.0, |r| r.cand.rel);
        if max > 0.0 {
            for r in pool.iter_mut() {
                r.cand.rel /= max;
            }
        }
        Ok(true)
    }

    /// Embed the query with an eligible `Embedding`-tier provider, with the query task prefix of
    /// the index's scheme ([`Store::embedding_prefixes`]): none for an index built without
    /// prefixes, whatever the provider's model would take.
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
        let text = match self.store.embedding_prefixes() {
            Ok(Some((prefix, _))) if !prefix.is_empty() => {
                nugget_infer::with_prefix(&prefix, &[query.to_owned()])
            }
            Ok(_) => vec![query.to_owned()],
            Err(e) => return Err(TextOnlyReason::EmbeddingFailed(e.to_string())),
        };
        let (p, mut vectors) = embed_first(router, &providers, &text)
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
            && let Some(vectors) = self.pool_vectors(pool, &ids, &texts, p).await?
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
    ///
    /// Sensitive passages (see [`is_sensitive`]; an item that cannot be checked counts as
    /// sensitive) are never sent to a `Cloud` embedder, the same rule as ingest's and the
    /// reranker's: if one of them has no stored vector (ingest leaves sensitive items without one
    /// when only a cloud embedder is allowed), MMR compares by words instead.
    async fn pool_vectors(
        &self,
        pool: &[Ranked],
        ids: &[ChunkId],
        texts: &[String],
        p: &Arc<dyn Provider>,
    ) -> Result<Option<Vec<Vec<f32>>>> {
        let mut stored = self.embeddings_by_id(ids)?;
        let mut vectors: Vec<Option<Vec<f32>>> = ids.iter().map(|id| stored.remove(id)).collect();
        let missing: Vec<usize> = (0..ids.len()).filter(|&i| vectors[i].is_none()).collect();
        if p.meta().locality == Locality::Cloud && missing.iter().any(|&i| pool[i].sensitive) {
            tracing::debug!(provider = %p.id(), "a sensitive passage has no stored vector; lexical MMR");
            return Ok(None);
        }
        if !missing.is_empty() {
            // Documents of this index, so its document prefix (if any).
            let prefix = self
                .store
                .embedding_prefixes()?
                .map(|(_, d)| d)
                .unwrap_or_default();
            let inputs: Vec<String> = missing
                .iter()
                .map(|&i| {
                    texts[i]
                        .chars()
                        .take(self.config.mmr_embed_chars.max(1))
                        .collect()
                })
                .collect();
            let inputs = nugget_infer::with_prefix(&prefix, &inputs);
            let items: Vec<_> = missing.iter().map(|&i| pool[i].cand.item.clone()).collect();
            match p.embed_items(&inputs, &items).await {
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
        let refs: Vec<_> = cands.iter().map(|c| c.item.clone()).collect();
        self.store.get_items(&refs)
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
                sensitive: r.sensitive,
                date: r.date.clone(),
                primary,
                neighbours,
                secondary,
            });
        }
        Ok(out)
    }
}

/// `YYYY-MM-DD` (UTC) of a timestamp.
fn iso_date(at: OffsetDateTime) -> String {
    let d = at.to_offset(UtcOffset::UTC).date();
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

/// Whether an item must stay away from cloud models: its `sensitive` metadata (set by ingest
/// from PII findings and triage) is anything but an explicit `false`. Items without the flag
/// (not written by ingest) are treated as sensitive, so the check fails closed.
pub fn is_sensitive(metadata: &serde_json::Value) -> bool {
    metadata.get("sensitive") != Some(&serde_json::Value::Bool(false))
}

/// The item's `url` metadata (set by ingest), if it is a sane single-line string.
fn item_url(item: &Item) -> Option<String> {
    let url = item.metadata.get("url")?.as_str()?.trim();
    let ok = !url.is_empty() && url.len() <= MAX_URL_LEN && !url.chars().any(char::is_control);
    ok.then(|| url.to_owned())
}
