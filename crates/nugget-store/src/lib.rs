//! Encrypted local store for Nugget.
//!
//! One SQLCipher-encrypted SQLite database holds:
//! - **items** (one per source document, deduplicated by content hash) and their **chunks**,
//! - an **FTS5** index (bm25) and a **`sqlite-vec`** index (cosine kNN) over chunks, fused with
//!   reciprocal-rank fusion for hybrid search,
//! - per-source **cursors**,
//! - the **egress ledger** (`impl EgressSink for Store`),
//! - a hash-chained, append-only **audit log**.
//!
//! The database key is a random 256-bit key held by the OS keychain, or in a `0600` key file
//! when no keychain is usable (see [`StoreKey::load_or_create`]).
//!
//! The API is synchronous. [`Store`] is cheap to clone and safe to share between threads; async
//! callers should use `spawn_blocking` for heavy calls.

mod audit;
mod egress;
mod error;
mod items;
mod key;
mod schema;
mod search;
mod store;
mod types;
mod vec_ext;

pub use key::{KEYCHAIN_ENV, KeySource, StoreKey};
pub use schema::LATEST_VERSION as SCHEMA_VERSION;
pub use store::Store;
pub use types::{
    AuditEntry, AuditRecord, ChunkId, Counts, Hit, Item, NewChunk, NewItem, UpsertOutcome,
    content_hash,
};

pub use nugget_core::{Error, Result};
