//! Public value types of the store API.

use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

/// Row id of a chunk. Stable until the item's chunks are replaced.
pub type ChunkId = i64;

/// Lowercase hex SHA-256 of `bytes`: the canonical `content_hash` for [`NewItem`].
pub fn content_hash(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes.as_ref()))
}

/// An item to insert or update. `content_hash` identifies the content: re-upserting the same
/// item with the same hash and fields is a no-op.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewItem {
    pub item: ItemRef,
    pub title: Option<String>,
    /// Hash of the item's content, e.g. [`content_hash`] of the raw bytes. Must be non-empty.
    pub content_hash: String,
    /// MIME type of the original content, if known.
    pub mime: Option<String>,
    /// Extracted plain text of the whole item, if the caller wants it kept.
    pub text: Option<String>,
    /// Free-form, source-specific metadata (JSON object recommended).
    pub metadata: Value,
    /// Modification time reported by the source, if any.
    #[serde(with = "time::serde::rfc3339::option")]
    pub source_modified_at: Option<OffsetDateTime>,
    /// Connector-defined version token (e.g. `mtime:size`, an etag, a Gmail history id) used to
    /// skip unchanged items without re-reading their content.
    #[serde(default)]
    pub version: Option<String>,
}

impl NewItem {
    /// A new item with no optional fields and `metadata = {}`.
    pub fn new(
        source: impl Into<String>,
        key: impl Into<String>,
        content_hash: impl Into<String>,
    ) -> Self {
        Self {
            item: ItemRef {
                source: SourceId::new(source),
                key: ItemKey::new(key),
            },
            title: None,
            content_hash: content_hash.into(),
            mime: None,
            text: None,
            metadata: Value::Object(Default::default()),
            source_modified_at: None,
            version: None,
        }
    }

    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    pub fn with_modified_at(mut self, at: OffsetDateTime) -> Self {
        self.source_modified_at = Some(at);
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    pub fn with_mime(mut self, mime: impl Into<String>) -> Self {
        self.mime = Some(mime.into());
        self
    }

    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = metadata;
        self
    }
}

/// A stored item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub item: ItemRef,
    pub title: Option<String>,
    pub content_hash: String,
    pub mime: Option<String>,
    pub text: Option<String>,
    pub metadata: Value,
    #[serde(with = "time::serde::rfc3339::option")]
    pub source_modified_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// Result of [`Store::upsert_item`](crate::Store::upsert_item).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpsertOutcome {
    /// The item did not exist.
    Inserted,
    /// The item existed and something changed. If the content hash changed, its (now stale)
    /// chunks were removed and the caller should re-chunk.
    Updated,
    /// The item existed with the same content hash and fields; nothing was written.
    Unchanged,
}

/// A chunk to store for an item. Embeddings are optional, but all embeddings in one database
/// must have the same dimension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewChunk {
    pub text: String,
    pub embedding: Option<Vec<f32>>,
}

impl NewChunk {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            embedding: None,
        }
    }

    pub fn embedded(text: impl Into<String>, embedding: Vec<f32>) -> Self {
        Self {
            text: text.into(),
            embedding: Some(embedding),
        }
    }
}

/// A search result. Higher `score` is better; scores are only comparable within one call.
///
/// - text search: `-bm25` (positive, unbounded),
/// - vector search: cosine similarity (`1 - cosine distance`, in `[-1, 1]`),
/// - hybrid search: reciprocal-rank-fusion score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub item: ItemRef,
    pub chunk_id: ChunkId,
    pub title: Option<String>,
    /// Plain-text excerpt of the chunk (no markup).
    pub snippet: String,
    pub score: f32,
}

impl From<Hit> for nugget_core::ipc::SearchHit {
    fn from(h: Hit) -> Self {
        Self {
            item: h.item,
            chunk_id: Some(h.chunk_id),
            title: h.title,
            snippet: h.snippet,
            score: h.score,
            url: None,
        }
    }
}

/// A stored chunk with its owning item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    pub id: ChunkId,
    pub item: ItemRef,
    /// Position within the item, from 0.
    pub ord: u32,
    pub text: String,
    /// Title of the owning item.
    pub title: Option<String>,
}

/// Per-source row counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCounts {
    pub source: SourceId,
    pub items: u64,
    pub chunks: u64,
}

/// Row counts, e.g. for the status page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub items: u64,
    pub chunks: u64,
}

/// An audit-log entry to append.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Who acted, e.g. `user`, `agent`, `daemon`.
    pub actor: String,
    /// What was done, e.g. `item.delete`, `action.send_email`.
    pub action: String,
    /// What it was done to (an item ref, a host, ...).
    pub target: String,
    pub detail: Value,
}

/// A stored audit-log entry with its position in the hash chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// 1-based, contiguous sequence number.
    pub seq: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub detail: Value,
    /// Hex SHA-256 of the previous record (all zeros for the first).
    pub prev_hash: String,
    /// Hex SHA-256 over this record's fields and `prev_hash`.
    pub hash: String,
}
