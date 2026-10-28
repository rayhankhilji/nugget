//! `nugget-retrieve`: precise, cited retrieval over the Nugget store.
//!
//! Instead of pasting a whole knowledge base into a prompt (and silently truncating it), Nugget
//! retrieves a few relevant, diverse passages, packs them into an explicit token budget and
//! numbers them so answers can cite `[n]`.
//!
//! Pipeline of [`Retriever::search`]:
//! 1. **Query embedding** through the router's `Embedding` tier, when a provider is available.
//!    Embedding is local-only by default: a `Cloud`-locality provider is used only with
//!    [`RetrieveConfig::allow_cloud_embedding`]. With no usable embedder (or no vectors in the
//!    store, or a failure) search falls back to full-text and says why ([`RetrievalMode`]).
//! 2. **Over-fetched hybrid search**: `limit × overfetch` candidate chunks from the store's
//!    reciprocal-rank fusion of bm25 and vector kNN.
//! 3. **Grouping by item**: the best chunk per item, other matching chunks recorded.
//! 4. Optional **recency boost** (off by default).
//! 5. **MMR diversity** over the top items: cosine similarity of the passages' stored embeddings
//!    (read back from the store, not re-embedded) when the query was embedded, token-set Jaccard
//!    otherwise.
//! 6. Mapping to [`nugget_core::ipc::SearchHit`] (with `url` from item metadata) via
//!    [`SearchResults::hits`].
//!
//! [`Retriever::build_context`] ranks the same way, then packs passages (each widened by its
//! neighbouring chunks while the budget allows, adjacent chunks merged) under `[n] Title (source)`
//! headers, never exceeding the budget as measured by `nugget_extract::estimate_tokens`.
//!
//! [`eval`] is a retrieval-quality harness on a labelled fixture corpus.

mod config;
mod embed;
pub mod eval;
mod pack;
pub mod rank;
mod retriever;
mod types;

pub use config::{RecencyBoost, RetrieveConfig};
pub use retriever::Retriever;
pub use types::{
    ChunkMatch, PackedContext, PackedPassage, RetrievalMode, RetrievedItem, SearchOptions,
    SearchResults, Similarity, Strategy, TextOnlyReason,
};
