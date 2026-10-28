//! Public result types.

use nugget_core::ids::{ItemRef, ProviderId};
use nugget_core::ipc::{ContextResponse, SearchHit};
use nugget_store::ChunkId;
use serde::{Deserialize, Serialize};

pub use crate::rank::ChunkMatch;

/// Which retrieval signals answered a query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RetrievalMode {
    /// Full-text and vector search fused with reciprocal-rank fusion.
    Hybrid { provider: ProviderId },
    /// Vector search only (explicitly requested with [`Strategy::VectorOnly`]).
    VectorOnly { provider: ProviderId },
    /// Full-text (bm25) only.
    TextOnly { reason: TextOnlyReason },
}

impl RetrievalMode {
    pub fn is_text_only(&self) -> bool {
        matches!(self, Self::TextOnly { .. })
    }

    /// The embedding provider used for the query, if any.
    pub fn provider(&self) -> Option<&ProviderId> {
        match self {
            Self::Hybrid { provider } | Self::VectorOnly { provider } => Some(provider),
            Self::TextOnly { .. } => None,
        }
    }
}

/// Why a query was answered by full-text search alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", content = "detail", rename_all = "snake_case")]
pub enum TextOnlyReason {
    /// The caller asked for text-only search.
    Requested,
    /// No router, or no provider in the `Embedding` tier.
    NoEmbedder,
    /// The only embedding providers are cloud providers and cloud embedding is not allowed.
    CloudNotAllowed,
    /// The store holds no vectors yet, so embedding the query would not help.
    NoVectorsIndexed,
    /// Every eligible embedding provider failed (last error).
    EmbeddingFailed(String),
    /// The query vector's dimension differs from the store's.
    DimensionMismatch(String),
}

/// How MMR compared candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Similarity {
    /// Cosine similarity of passage embeddings.
    Vector,
    /// Token-set Jaccard similarity.
    Lexical,
    /// MMR was off (or there was nothing to diversify).
    None,
}

/// Search strategy for [`Retriever::search_with`](crate::Retriever::search_with).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    /// Hybrid when the query can be embedded, otherwise text-only.
    #[default]
    Auto,
    /// Full-text only; never calls an embedding provider.
    TextOnly,
    /// Vector only; `Error::Unsupported` if the query cannot be embedded.
    VectorOnly,
}

/// Per-call overrides.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchOptions {
    pub strategy: Strategy,
    /// Override [`RetrieveConfig::mmr`](crate::RetrieveConfig::mmr).
    pub mmr: Option<bool>,
}

/// One retrieved item: its best passage plus the other chunks that matched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievedItem {
    pub item: ItemRef,
    pub title: Option<String>,
    /// From the item's `url` metadata, when present.
    pub url: Option<String>,
    /// The best-matching chunk.
    pub chunk_id: ChunkId,
    pub snippet: String,
    /// Relevance in `[0, 1]` relative to the best candidate of this query (after any recency
    /// boost). With MMR on, results are in MMR order, which need not be score order.
    pub score: f32,
    /// Other matching chunks of this item, best first.
    pub matches: Vec<ChunkMatch>,
}

impl RetrievedItem {
    /// Every matching chunk id, best first.
    pub fn chunk_ids(&self) -> Vec<ChunkId> {
        std::iter::once(self.chunk_id)
            .chain(self.matches.iter().map(|m| m.chunk_id))
            .collect()
    }
}

impl From<&RetrievedItem> for SearchHit {
    fn from(r: &RetrievedItem) -> Self {
        SearchHit {
            item: r.item.clone(),
            chunk_id: Some(r.chunk_id),
            title: r.title.clone(),
            snippet: r.snippet.clone(),
            score: r.score,
            url: r.url.clone(),
        }
    }
}

impl From<RetrievedItem> for SearchHit {
    fn from(r: RetrievedItem) -> Self {
        SearchHit {
            item: r.item,
            chunk_id: Some(r.chunk_id),
            title: r.title,
            snippet: r.snippet,
            score: r.score,
            url: r.url,
        }
    }
}

/// Result of a search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResults {
    pub mode: RetrievalMode,
    pub similarity: Similarity,
    pub items: Vec<RetrievedItem>,
}

impl SearchResults {
    /// The results as IPC search hits.
    pub fn hits(&self) -> Vec<SearchHit> {
        self.items.iter().map(SearchHit::from).collect()
    }
}

/// One packed passage block of a context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackedPassage {
    /// Citation number.
    pub n: u32,
    pub item: ItemRef,
    /// Chunks included, in document order.
    pub chunk_ids: Vec<ChunkId>,
    /// The primary passage was cut to fit the budget.
    pub truncated: bool,
}

/// A packed context with diagnostics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackedContext {
    pub response: ContextResponse,
    pub mode: RetrievalMode,
    pub similarity: Similarity,
    /// `nugget_extract::estimate_tokens(&response.text)`, never above the budget.
    pub tokens: usize,
    pub passages: Vec<PackedPassage>,
}
