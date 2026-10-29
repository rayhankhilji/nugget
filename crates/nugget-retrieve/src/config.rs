//! Retrieval configuration.

use serde::{Deserialize, Serialize};

/// Tuning knobs for [`Retriever`](crate::Retriever). `RetrieveConfig::default()` is the
/// recommended setting; every field has a serde default so partial TOML/JSON works.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RetrieveConfig {
    /// Send queries (and, for vector MMR, candidate passages stored without a vector) to a
    /// `Cloud`-locality embedding provider. Off by default: embeddings stay on this device or the
    /// local network, and a cloud-only embedding tier means text-only search.
    pub allow_cloud_embedding: bool,
    /// Candidate chunks fetched per requested result before grouping by item.
    pub overfetch: usize,
    /// ...but at least this many candidate chunks.
    pub min_candidates: usize,
    /// ...and at most this many (bounded by the store's per-call limits).
    pub max_candidates: usize,
    /// Hybrid search fuses the top `max(limit × fusion_depth_factor, min_fusion_depth)` hits of
    /// each list (full-text and vector), where `limit` is the number of results asked for, not
    /// the over-fetched candidate count. Deeper lists let chunks that are mediocre in both lists
    /// outrank a strong match from one (reciprocal-rank fusion), which cost about 8 points of
    /// known-item recall@10 on the benchmark corpus at 32× depth.
    pub fusion_depth_factor: usize,
    /// ...but at least this many hits per list.
    pub min_fusion_depth: usize,
    /// Re-rank with maximal marginal relevance (MMR) for diversity.
    pub mmr: bool,
    /// MMR trade-off: 1.0 is pure relevance, 0.0 pure novelty.
    ///
    /// 0.7, kept after the sweep in `tests/eval.rs` (`mmr_sweep`: λ 0.5–1.0 × pool factor 2–5
    /// on the 57-doc / 42-query fixture set; diversity as ILS@5, the mean pairwise token Jaccard
    /// of the top 5, lower is more diverse). At 0.7: vector MMR R@1 0.583, R@5 0.881, R@10
    /// 0.976, MRR@10 0.777, nDCG@10 0.822, ILS@5 0.021; lexical MMR 0.500 / 0.798 / 0.881 /
    /// 0.677 / 0.724, ILS@5 0.024. No λ wins clearly: 0.5 is best for vector MMR (R@5 0.917,
    /// MRR@10 0.789, nDCG@10 0.831) and 0.9 for lexical (R@5 0.833, MRR@10 0.679, nDCG@10
    /// 0.727), each about one query of 42 and none moving R@1, R@10 or ILS@5. 0.9 is slightly
    /// ahead in both, but it stops demoting exact duplicates below a clearly relevant distinct
    /// passage (`mmr_demotes_near_duplicates_lexically`), which binary-labelled fixtures with
    /// few multi-document queries cannot reward. So the gain is noise-sized and the cost real.
    pub mmr_lambda: f32,
    /// How many top items MMR chooses from, as a multiple of the requested result count.
    /// The sweep showed no effect between 2 and 5 on the fixture set (identical metrics), so the
    /// default stays at 3.
    pub mmr_pool_factor: usize,
    /// Upper bound on the MMR pool size (items).
    pub mmr_pool_max: usize,
    /// When the query was embedded, MMR compares the pool's passages by vector: their stored
    /// chunk embeddings (read back from the store, never re-embedded), with any passage stored
    /// without a vector embedded by the query's provider. Otherwise, or if that fails, MMR uses
    /// token-set Jaccard.
    pub mmr_vectors: bool,
    /// Characters of each candidate passage sent for MMR embedding (only passages stored without
    /// a vector are embedded).
    pub mmr_embed_chars: usize,
    /// Optional boost for recently modified items. Off by default.
    pub recency: Option<RecencyBoost>,
    /// Other matching chunks remembered per item (beyond the best one).
    pub max_matches_per_item: usize,
    /// Most items (citations) packed into a context.
    pub context_max_items: usize,
    /// Add the chunks before and after each packed passage while the budget allows.
    pub expand_neighbours: bool,
    /// Also pack an item's other matching chunks while the budget allows.
    pub include_secondary_matches: bool,
    /// A passage that does not fit whole is cut to fit, but only if at least this many tokens
    /// of it remain; otherwise it is skipped.
    pub min_truncated_tokens: usize,
    /// With a reranker ([`Retriever::with_reranker`](crate::Retriever::with_reranker)), how many
    /// of the top grouped candidates it scores, before MMR. Candidates below the pool keep their
    /// order after the reranked ones.
    pub rerank_pool: usize,
    /// How much the reranker decides: an item's relevance becomes
    /// `w × rerank + (1 − w) × first_stage` (both scaled so the best is 1). `1.0` replaces the
    /// first-stage score; the default keeps a little of it so a weak cross-encoder cannot bury
    /// an exact keyword match on its own.
    pub rerank_weight: f32,
    /// Give up on the reranker after this long and keep the first-stage order.
    pub rerank_timeout_ms: u64,
    /// Parse date phrases ("last week", "in march") out of queries and keep results modified in
    /// that range (see [`crate::dates`]).
    pub dates: bool,
    /// UTC offset, in minutes, at which days start for date phrases. `0` (UTC) by default: the
    /// daemon has no reliable notion of the user's time zone. Clamped to ±18 hours.
    pub date_utc_offset_minutes: i32,
    /// Rewrite follow-up queries ("what about the second one?") into standalone ones in
    /// [`Retriever::search_in_conversation`](crate::Retriever::search_in_conversation).
    pub rewrite_followups: bool,
    /// Give up on the rewriting model after this long and search the original query.
    pub rewrite_timeout_ms: u64,
    /// Longest accepted rewritten query (characters); longer output is cut at a word boundary.
    pub rewrite_max_chars: usize,
    /// Most recent conversation turns shown to the rewriting model.
    pub rewrite_history_turns: usize,
}

impl Default for RetrieveConfig {
    fn default() -> Self {
        Self {
            allow_cloud_embedding: false,
            overfetch: 8,
            min_candidates: 64,
            max_candidates: 512,
            fusion_depth_factor: 4,
            min_fusion_depth: 40,
            mmr: true,
            mmr_lambda: 0.7,
            mmr_pool_factor: 3,
            mmr_pool_max: 64,
            mmr_vectors: true,
            mmr_embed_chars: 2000,
            recency: None,
            max_matches_per_item: 8,
            context_max_items: 10,
            expand_neighbours: true,
            include_secondary_matches: true,
            min_truncated_tokens: 24,
            rerank_pool: 30,
            rerank_weight: 0.7,
            rerank_timeout_ms: 2_500,
            dates: true,
            date_utc_offset_minutes: 0,
            rewrite_followups: true,
            rewrite_timeout_ms: 3_000,
            rewrite_max_chars: 200,
            rewrite_history_turns: 6,
        }
    }
}

/// Multiplies an item's relevance by `1 + weight * 0.5^(age_days / half_life_days)`, where the
/// age comes from the source's modification time (or, failing that, the store's update time).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RecencyBoost {
    pub half_life_days: f32,
    pub weight: f32,
}

impl Default for RecencyBoost {
    fn default() -> Self {
        Self {
            half_life_days: 30.0,
            weight: 0.2,
        }
    }
}
