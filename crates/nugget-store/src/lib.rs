//! Encrypted local store for Nugget.
//!
//! One SQLCipher-encrypted SQLite database holds:
//! - **items** (one per source document, deduplicated by content hash) and their **chunks**,
//! - an **FTS5** index (bm25) and a **`sqlite-vec`** index (cosine kNN) over chunks, fused with
//!   reciprocal-rank fusion for hybrid search,
//! - per-source **cursors**, and an **item change feed** with deletion tombstones
//!   ([`Store::items_changed_since`]),
//! - the **egress ledger** (`impl EgressSink for Store`),
//! - a hash-chained, append-only **audit log**,
//! - the **knowledge graph** (entities, claims with evidence, mentions, a change feed):
//!   [`SqliteGraph`], obtained with [`Store::graph`], implements [`nugget_graph::GraphStore`].
//!
//! The database key is a random 256-bit key held by the OS keychain, or in a `0600` key file
//! when no keychain is usable (see [`StoreKey::load_or_create`]).
//!
//! The API is synchronous. [`Store`] is cheap to clone and safe to share between threads; async
//! callers should use `spawn_blocking` for heavy calls.

mod audit;
mod changes;
mod egress;
mod error;
mod graph;
mod items;
mod key;
mod schema;
mod search;
mod sources;
mod store;
mod types;
mod vec_ext;

pub use graph::{ENTITY_ID_PREFIX, MAX_MERGE_DEPTH, MAX_NEIGHBOURHOOD_NODES, SqliteGraph};
pub use key::{KEYCHAIN_ENV, KeySource, StoreKey};
pub use schema::LATEST_VERSION as SCHEMA_VERSION;
pub use search::TITLE_WEIGHT;
pub use sources::{MAX_CHUNK_IDS, MAX_NEIGHBOUR_RADIUS};
pub use store::Store;
pub use types::{
    AuditEntry, AuditRecord, Chunk, ChunkId, Counts, Hit, Item, ItemChange, ItemDates, NewChunk,
    NewItem, SourceCounts, UpsertOutcome, content_hash,
};

pub use nugget_core::{Error, Result};
