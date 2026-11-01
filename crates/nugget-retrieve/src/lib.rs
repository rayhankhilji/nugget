//! `nugget-retrieve`: precise, cited retrieval over the Nugget store.

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

pub use config::{Fusion, RecencyBoost, RetrieveConfig};
pub use dates::{DateQuery, DateRange, parse_date_query};
pub use rerank::{HttpReranker, HttpRerankerConfig, RerankStatus, Reranker};
pub use retriever::{Retriever, is_confirmed_sensitive, is_sensitive};
pub use rewrite::{QueryRewrite, RewriteMethod};
pub use types::{
    ChunkMatch, ContextOptions, DateFilter, PackedContext, PackedPassage, RetrievalMode,
    RetrievedItem, SearchOptions, SearchResults, Similarity, Strategy, TextOnlyReason,
};
