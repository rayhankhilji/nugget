//! Items and chunks: upsert with content-hash dedup, delete cascade, chunk replacement.

use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_core::{Error, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use time::OffsetDateTime;

use crate::Store;
use crate::error::{DbExt, format_time, parse_time};
use crate::graph::{ItemScope, Touched, retract_tx};
use crate::types::{ChunkId, Item, NewChunk, NewItem, UpsertOutcome};

/// Upper bound accepted by `sqlite-vec`'s `vec0` tables.
pub(crate) const MAX_EMBEDDING_DIM: usize = 8192;

const META_EMBEDDING_DIM: &str = "embedding_dim";

/// Row shape of `items` as stored (strings not yet parsed).
struct ItemRow {
    id: i64,
    title: Option<String>,
    content_hash: String,
    mime: Option<String>,
    text: Option<String>,
    metadata: String,
    source_modified_at: Option<String>,
    created_at: String,
    updated_at: String,
    version: Option<String>,
}

const ITEM_COLUMNS: &str = "id, title, content_hash, mime, text, metadata, source_modified_at, \
                            created_at, updated_at, version";

fn read_item_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ItemRow> {
    Ok(ItemRow {
        id: r.get(0)?,
        title: r.get(1)?,
        content_hash: r.get(2)?,
        mime: r.get(3)?,
        text: r.get(4)?,
        metadata: r.get(5)?,
        source_modified_at: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
        version: r.get(9)?,
    })
}

fn find_item(conn: &Connection, item: &ItemRef) -> Result<Option<ItemRow>> {
    conn.query_row(
        &format!("SELECT {ITEM_COLUMNS} FROM items WHERE source = ?1 AND key = ?2"),
        params![item.source.as_str(), item.key.as_str()],
        read_item_row,
    )
    .optional()
    .db("read item")
}

fn validate_ref(item: &ItemRef) -> Result<()> {
    if item.source.as_str().is_empty() || item.key.as_str().is_empty() {
        return Err(Error::Invalid(
            "item source and key must be non-empty".into(),
        ));
    }
    Ok(())
}

/// An item's fields prepared for writing (serialised once, outside the lock).
struct PreparedItem<'a> {
    new: &'a NewItem,
    metadata: String,
    modified: Option<String>,
    now: String,
}

impl<'a> PreparedItem<'a> {
    fn new(new: &'a NewItem) -> Result<Self> {
        validate_ref(&new.item)?;
        if new.content_hash.trim().is_empty() {
            return Err(Error::Invalid("content_hash must be non-empty".into()));
        }
        Ok(Self {
            new,
            metadata: serde_json::to_string(&new.metadata)?,
            modified: new.source_modified_at.map(format_time).transpose()?,
            now: format_time(OffsetDateTime::now_utc())?,
        })
    }
}

/// What [`upsert_row`] did, with the row id.
struct RowUpsert {
    outcome: UpsertOutcome,
    item_id: i64,
}

/// Insert or update the item row inside `tx` (see [`Store::upsert_item`] for the rules).
fn upsert_row(tx: &Connection, p: &PreparedItem<'_>) -> Result<RowUpsert> {
    let new = p.new;
    match find_item(tx, &new.item)? {
        None => {
            tx.execute(
                "INSERT INTO items (source, key, title, content_hash, mime, text, metadata,
                                    source_modified_at, created_at, updated_at, version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10)",
                params![
                    new.item.source.as_str(),
                    new.item.key.as_str(),
                    new.title,
                    new.content_hash,
                    new.mime,
                    new.text,
                    p.metadata,
                    p.modified,
                    p.now,
                    new.version,
                ],
            )
            .db("insert item")?;
            Ok(RowUpsert {
                outcome: UpsertOutcome::Inserted,
                item_id: tx.last_insert_rowid(),
            })
        }
        Some(old) => {
            let content_changed = old.content_hash != new.content_hash;
            let modified_unchanged = match (&old.source_modified_at, &new.source_modified_at) {
                (None, None) => true,
                (Some(o), Some(n)) => parse_time(o)? == *n,
                _ => false,
            };
            let unchanged = !content_changed
                && old.title == new.title
                && old.mime == new.mime
                && old.text == new.text
                && old.metadata == p.metadata
                && old.version == new.version
                && modified_unchanged;
            if unchanged {
                return Ok(RowUpsert {
                    outcome: UpsertOutcome::Unchanged,
                    item_id: old.id,
                });
            }
            tx.execute(
                "UPDATE items SET title = ?2, content_hash = ?3, mime = ?4, text = ?5,
                                  metadata = ?6, source_modified_at = ?7,
                                  updated_at = ?8, version = ?9
                 WHERE id = ?1",
                params![
                    old.id,
                    new.title,
                    new.content_hash,
                    new.mime,
                    new.text,
                    p.metadata,
                    p.modified,
                    p.now,
                    new.version,
                ],
            )
            .db("update item")?;
            if content_changed {
                tx.execute("DELETE FROM chunks WHERE item_id = ?1", [old.id])
                    .db("drop stale chunks")?;
            }
            Ok(RowUpsert {
                outcome: UpsertOutcome::Updated,
                item_id: old.id,
            })
        }
    }
}

/// Validate a chunk batch: finite, non-empty embeddings of one dimension. Returns that
/// dimension (`None` when no chunk carries an embedding).
fn validate_chunks(chunks: &[NewChunk]) -> Result<Option<usize>> {
    let mut dim = None;
    for (i, ch) in chunks.iter().enumerate() {
        if let Some(e) = &ch.embedding {
            validate_embedding(e).map_err(|m| Error::Invalid(format!("chunk {i}: {m}")))?;
            match dim {
                None => dim = Some(e.len()),
                Some(d) if d != e.len() => {
                    return Err(Error::Invalid(format!(
                        "chunk {i}: embedding dimension {} differs from {d} in the same batch",
                        e.len()
                    )));
                }
                Some(_) => {}
            }
        }
    }
    Ok(dim)
}

/// Replace every chunk of `item_id` inside `tx`. `dim` comes from [`validate_chunks`].
fn write_chunks(
    tx: &Connection,
    item_id: i64,
    chunks: &[NewChunk],
    dim: Option<usize>,
) -> Result<Vec<ChunkId>> {
    if let Some(d) = dim {
        ensure_vec_table(tx, d)?;
    }
    tx.execute("DELETE FROM chunks WHERE item_id = ?1", [item_id])
        .db("delete old chunks")?;
    let mut ids = Vec::with_capacity(chunks.len());
    let mut ins = tx
        .prepare_cached("INSERT INTO chunks (item_id, ord, text) VALUES (?1, ?2, ?3)")
        .db("prepare chunk insert")?;
    let mut ins_vec = match dim {
        Some(_) => Some(
            tx.prepare_cached("INSERT INTO chunk_vec (rowid, embedding) VALUES (?1, ?2)")
                .db("prepare vector insert")?,
        ),
        None => None,
    };
    for (ord, ch) in chunks.iter().enumerate() {
        ins.execute(params![item_id, ord as i64, ch.text])
            .db("insert chunk")?;
        let id = tx.last_insert_rowid();
        if let (Some(e), Some(stmt)) = (&ch.embedding, ins_vec.as_mut()) {
            stmt.execute(params![id, f32_blob(e)]).db("insert vector")?;
        }
        ids.push(id);
    }
    Ok(ids)
}

impl Store {
    /// Insert or update an item, keyed by `(source, key)`.
    ///
    /// Re-upserting identical content and fields returns `Unchanged` without writing. When the
    /// content hash changes, the item's chunks (derived from the old content) are deleted so stale
    /// text never stays searchable; re-chunk with [`Store::replace_chunks`], or write item and
    /// chunks together with [`Store::upsert_item_with_chunks`].
    pub fn upsert_item(&self, new: &NewItem) -> Result<UpsertOutcome> {
        let p = PreparedItem::new(new)?;
        self.with_conn(|conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .db("begin upsert")?;
            let row = upsert_row(&tx, &p)?;
            tx.commit().db("commit upsert")?;
            Ok(row.outcome)
        })
    }

    /// Write an item and its chunks in **one transaction**: the item row (including its
    /// `version`), its chunks, their full-text entries and vectors become visible together, or
    /// (on any error) nothing changes. A crash can therefore never leave an item whose version
    /// says "indexed" without its chunks.
    ///
    /// - `Inserted` / `Updated`: the item's chunks are replaced by `chunks` (as with
    ///   [`Store::replace_chunks`]; an empty slice leaves the item without chunks).
    /// - `Unchanged` (same hash and fields): nothing is written, with one exception: if the stored
    ///   item has **no** chunks but `chunks` is non-empty (an earlier non-atomic write was
    ///   interrupted between the item and its chunks), the chunks are written and the outcome
    ///   is `Updated`.
    ///
    /// Embeddings follow the [`Store::replace_chunks`] rules (one dimension per database).
    pub fn upsert_item_with_chunks(
        &self,
        new: &NewItem,
        chunks: &[NewChunk],
    ) -> Result<UpsertOutcome> {
        let p = PreparedItem::new(new)?;
        let dim = validate_chunks(chunks)?;
        self.with_conn(|conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .db("begin upsert_item_with_chunks")?;
            let row = upsert_row(&tx, &p)?;
            let outcome = match row.outcome {
                UpsertOutcome::Unchanged => {
                    let has_chunks: bool = tx
                        .query_row(
                            "SELECT EXISTS (SELECT 1 FROM chunks WHERE item_id = ?1)",
                            [row.item_id],
                            |r| r.get(0),
                        )
                        .db("count chunks")?;
                    if has_chunks || chunks.is_empty() {
                        UpsertOutcome::Unchanged
                    } else {
                        write_chunks(&tx, row.item_id, chunks, dim)?;
                        UpsertOutcome::Updated
                    }
                }
                outcome => {
                    write_chunks(&tx, row.item_id, chunks, dim)?;
                    outcome
                }
            };
            tx.commit().db("commit upsert_item_with_chunks")?;
            Ok(outcome)
        })
    }

    /// Fetch an item by reference.
    pub fn get_item(&self, item: &ItemRef) -> Result<Option<Item>> {
        let Some(row) = self.with_read_conn(|c| find_item(c, item))? else {
            return Ok(None);
        };
        Ok(Some(Item {
            item: item.clone(),
            title: row.title,
            content_hash: row.content_hash,
            mime: row.mime,
            text: row.text,
            metadata: serde_json::from_str(&row.metadata)?,
            source_modified_at: row
                .source_modified_at
                .as_deref()
                .map(parse_time)
                .transpose()?,
            version: row.version,
            created_at: parse_time(&row.created_at)?,
            updated_at: parse_time(&row.updated_at)?,
        }))
    }

    /// Delete an item with its chunks, full-text entries and vectors. Returns whether it existed.
    ///
    /// In the same transaction, the item's knowledge-graph provenance is dropped as by
    /// [`GraphStore::retract_item`](nugget_graph::GraphStore::retract_item): its evidence and
    /// mentions are removed, claims left without evidence are retracted, and the affected
    /// entities enter the graph change feed. This happens even if the item row is already gone,
    /// so a retry cleans up after an earlier partial failure elsewhere.
    pub fn delete_item(&self, item: &ItemRef) -> Result<bool> {
        let now = format_time(OffsetDateTime::now_utc())?;
        self.with_conn(|conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .db("begin delete_item")?;
            // `ON DELETE CASCADE` removes chunks; chunk triggers clean FTS and vectors, and the
            // item trigger cleans the title index.
            let n = tx
                .execute(
                    "DELETE FROM items WHERE source = ?1 AND key = ?2",
                    params![item.source.as_str(), item.key.as_str()],
                )
                .db("delete item")?;
            let mut touched = Touched::new(now);
            retract_tx(&tx, ItemScope::Item(item), &mut touched)?;
            touched.flush(&tx)?;
            tx.commit().db("commit delete_item")?;
            Ok(n > 0)
        })
    }

    /// Replace all chunks of an item (in order). Returns the new chunk ids.
    ///
    /// The first embedding stored fixes the database's embedding dimension; later embeddings of
    /// another dimension are rejected with `Error::Invalid`. Fails with `Error::NotFound` if the
    /// item does not exist. Atomic: on error nothing changes.
    pub fn replace_chunks(&self, item: &ItemRef, chunks: &[NewChunk]) -> Result<Vec<ChunkId>> {
        let dim = validate_chunks(chunks)?;
        self.with_conn(|conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .db("begin replace_chunks")?;
            let item_id: i64 = tx
                .query_row(
                    "SELECT id FROM items WHERE source = ?1 AND key = ?2",
                    params![item.source.as_str(), item.key.as_str()],
                    |r| r.get(0),
                )
                .optional()
                .db("find item")?
                .ok_or_else(|| Error::NotFound(format!("item {}:{}", item.source, item.key)))?;
            let ids = write_chunks(&tx, item_id, chunks, dim)?;
            tx.commit().db("commit replace_chunks")?;
            Ok(ids)
        })
    }
}

/// Reconstruct an [`ItemRef`] from stored columns.
pub(crate) fn item_ref(source: String, key: String) -> ItemRef {
    ItemRef {
        source: SourceId(source),
        key: ItemKey(key),
    }
}

pub(crate) fn validate_embedding(e: &[f32]) -> std::result::Result<(), String> {
    if e.is_empty() {
        return Err("embedding is empty".into());
    }
    if e.len() > MAX_EMBEDDING_DIM {
        return Err(format!(
            "embedding dimension {} exceeds the maximum of {MAX_EMBEDDING_DIM}",
            e.len()
        ));
    }
    if e.iter().any(|x| !x.is_finite()) {
        return Err("embedding contains NaN or infinity".into());
    }
    Ok(())
}

/// Little-endian `f32` blob, the binary vector format `sqlite-vec` accepts.
pub(crate) fn f32_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// Inverse of [`f32_blob`]; `None` if the blob is not a whole number of `f32`s.
pub(crate) fn f32_from_blob(blob: &[u8]) -> Option<Vec<f32>> {
    if !blob.len().is_multiple_of(4) {
        return None;
    }
    Some(
        blob.chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect(),
    )
}

pub(crate) fn embedding_dim(conn: &Connection) -> Result<Option<usize>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [META_EMBEDDING_DIM],
            |r| r.get(0),
        )
        .optional()
        .db("read embedding dimension")?;
    raw.map(|s| {
        s.parse::<usize>()
            .map_err(|_| Error::Storage(format!("bad embedding dimension {s:?} in meta")))
    })
    .transpose()
}

/// Check `dim` against the database's embedding dimension, fixing it (and creating the vector
/// table with its delete trigger) on first use.
fn ensure_vec_table(conn: &Connection, dim: usize) -> Result<()> {
    match embedding_dim(conn)? {
        Some(d) if d == dim => Ok(()),
        Some(d) => Err(Error::Invalid(format!(
            "embedding dimension {dim} does not match this store's dimension {d}"
        ))),
        None => {
            conn.execute_batch(&format!(
                "CREATE VIRTUAL TABLE IF NOT EXISTS chunk_vec
                     USING vec0 (embedding float[{dim}] distance_metric=cosine);
                 CREATE TRIGGER IF NOT EXISTS chunks_vec_ad AFTER DELETE ON chunks BEGIN
                     DELETE FROM chunk_vec WHERE rowid = old.id;
                 END;"
            ))
            .db("create vector table")?;
            conn.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                params![META_EMBEDDING_DIM, dim.to_string()],
            )
            .db("record embedding dimension")?;
            Ok(())
        }
    }
}
