//! `nugget-retrieve`: precise, cited retrieval over the Nugget store.
//!
//! Instead of pasting a whole knowledge base into a prompt (and silently truncating it), Nugget
//! retrieves a few relevant, diverse passages, packs them into an explicit token budget and
//! numbers them so answers can cite `[n]`.
//!
//! Pipeline of [`Retriever::search`]:
//! 1. **Date phrases** ([`dates`]): "last week", "in march 2025", "since monday" and similar are
//!    parsed deterministically (relative to the reference time, in UTC unless
//!    [`RetrieveConfig::date_utc_offset_minutes`] says otherwise). The rest of the query is
//!    searched and only items modified in the range are kept; when none are, the original query
//!    is searched unfiltered (the phrase may have been topical). [`SearchResults::date`] reports
//!    it.
//! 2. **Query embedding** through the router's `Embedding` tier, when a provider is available.
//!    Embedding is local-only by default: a `Cloud`-locality provider is used only with
//!    [`RetrieveConfig::allow_cloud_embedding`]. With no usable embedder (or no vectors in the
//!    store, or a failure) search falls back to full-text and says why ([`RetrievalMode`]).
//! 3. **Over-fetched hybrid search**: `limit × overfetch` candidate chunks from the store's
//!    reciprocal-rank fusion of bm25 and vector kNN (the maximum when a date filter applies).
//! 4. **Grouping by item**: the best chunk per item, other matching chunks recorded; then the
//!    date filter.
//! 5. Optional **recency boost** (off by default).
//! 6. Optional **reranking** ([`rerank`], [`Retriever::with_reranker`]): a cross-encoder (e.g. a
//!    local llama-server rerank endpoint via [`HttpReranker`]) scores the top
//!    [`RetrieveConfig::rerank_pool`] items; its scores are blended into relevance
//!    ([`RetrieveConfig::rerank_weight`]). Errors and timeouts keep the first-stage order
//!    ([`SearchResults::reranked`] is then false). The daemon builds one from
//!    [`RetrieveConfig::reranker`] (`[retrieve.reranker]`, a server you run, e.g.
//!    `llama-server --reranking`), else from its local `rerank` runtime when
//!    `[runtime] rerank_model` is set; with neither, there is no reranking.
//! 7. **MMR diversity** over the top items: cosine similarity of the passages' stored embeddings
//!    (read back from the store, not re-embedded) when the query was embedded, token-set Jaccard
//!    otherwise.
//! 8. Mapping to [`nugget_core::ipc::SearchHit`] (with `url` from item metadata) via
//!    [`SearchResults::hits`].
//!
//! [`Retriever::search_in_conversation`] first rewrites a follow-up ("what about the second
//! one?") into a standalone query with a local chat model, or a deterministic fallback
//! ([`rewrite`]); the history is treated as untrusted prompt input.
//!
//! [`Retriever::build_context`] ranks the same way, then packs passages (each widened by its
//! neighbouring chunks while the budget allows, adjacent chunks merged) under `[n] Title (source)`
//! headers, never exceeding the budget as measured by `nugget_extract::estimate_tokens`.
//!
//! [`Retriever::build_context_with`] with [`ContextOptions::exclude_sensitive`] leaves items
//! marked sensitive (see [`is_sensitive`]) out of the context, for contexts sent to `Cloud`
//! models; citations carry each item's `sensitive` flag.
//!
//! [`eval`] is a retrieval-quality harness on a labelled fixture corpus.

mod config;
pub mod dates;
mod embed;
pub mod eval;
mod pack;
pub mod rank;
pub mod rerank;
mod retriever;
pub mod rewrite;
mod types;

pub use config::{RecencyBoost, RetrieveConfig};
pub use dates::{DateQuery, DateRange, parse_date_query};
pub use rerank::{HttpReranker, HttpRerankerConfig, Reranker};
pub use retriever::{Retriever, is_sensitive};
pub use rewrite::{QueryRewrite, RewriteMethod};
pub use types::{
    ChunkMatch, ContextOptions, DateFilter, PackedContext, PackedPassage, RetrievalMode,
    RetrievedItem, SearchOptions, SearchResults, Similarity, Strategy, TextOnlyReason,
};
