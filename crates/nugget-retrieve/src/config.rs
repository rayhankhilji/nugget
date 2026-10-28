//! Retrieval configuration.

use serde::{Deserialize, Serialize};

/// Tuning knobs for [`Retriever`](crate::Retriever). `RetrieveConfig::default()` is the
/// recommended setting; every field has a serde default so partial TOML/JSON works.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RetrieveConfig {
    /// Send queries (and, for vector MMR, candidate passages) to a `Cloud`-locality embedding
    /// provider. Off by default: embeddings stay on this device or the local network, and a
    /// cloud-only embedding tier means text-only search.
    pub allow_cloud_embedding: bool,
    /// Candidate chunks fetched per requested result before grouping by item.
    pub overfetch: usize,
    /// ...but at least this many candidate chunks.
    pub min_candidates: usize,
    /// ...and at most this many (bounded by the store's per-call limits).
    pub max_candidates: usize,
    /// Re-rank with maximal marginal relevance (MMR) for diversity.
    pub mmr: bool,
    /// MMR trade-off: 1.0 is pure relevance, 0.0 pure novelty.
    pub mmr_lambda: f32,
    /// How many top items MMR chooses from, as a multiple of the requested result count.
    pub mmr_pool_factor: usize,
    /// Upper bound on the MMR pool size (items).
    pub mmr_pool_max: usize,
    /// When the query was embedded, also embed the MMR pool's passages (with the same provider)
    /// so MMR compares vectors. Otherwise, or if that fails, MMR uses token-set Jaccard.
    pub mmr_vectors: bool,
    /// Characters of each candidate passage sent for MMR embedding.
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
}

impl Default for RetrieveConfig {
    fn default() -> Self {
        Self {
            allow_cloud_embedding: false,
            overfetch: 8,
            min_candidates: 64,
            max_candidates: 512,
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
