//! Items and chunks: upsert with content-hash dedup, delete cascade, chunk replacement.
//!
//! Item text is stored once: when an item's chunks cover its text, `items.text` is NULL and the
//! text is kept as a layout over the chunks in `item_text` (see [`text_layout`]). Every path
//! that rewrites an item's chunks re-plans that layout in the same transaction, without touching
//! the `items` row (so `updated_at` and the item change feed are unaffected).

mod text_layout;

use std::collections::HashMap;

use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_core::{Error, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use time::OffsetDateTime;

use crate::Store;
use crate::error::{DbExt, format_time, parse_time};
use crate::graph::{ItemScope, Touched, retract_tx};
use crate::types::{ChunkId, Item, NewChunk, NewItem, UpsertOutcome};
use crate::vector::AnnWriter;

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
    /// The `item_text` layout, when the text is stored there (then `text` is NULL).
    layout: Option<Vec<u8>>,
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
        layout: r.get(10)?,
    })
}

fn find_item(conn: &Connection, item: &ItemRef) -> Result<Option<ItemRow>> {
    conn.query_row(
        &format!(
            "SELECT {ITEM_COLUMNS}, (SELECT layout FROM item_text WHERE item_id = items.id)
             FROM items WHERE source = ?1 AND key = ?2"
        ),
        params![item.source.as_str(), item.key.as_str()],
        read_item_row,
    )
    .optional()
    .db("read item")
}

/// Items looked up per query by the batched reads ([`Store::get_items`],
/// [`Store::item_dates`]). The refs travel as one JSON parameter, so this bounds the work (and
/// the time a read connection is held) per query, not a bound-parameter limit.
pub(crate) const LOOKUP_BATCH: usize = 500;

/// `FROM` clause joining the refs in `?1` (a [`refs_json`] array) to their items: `want.key` is
/// a ref's index in the array, `i` the item. `CROSS JOIN` keeps the refs as the outer loop, so
/// each one is a lookup on the `(source, key)` index.
pub(crate) const WANTED_ITEMS_JOIN: &str = "FROM json_each(?1) AS want
     CROSS JOIN items i ON i.source = json_extract(want.value, '$[0]')
                       AND i.key = json_extract(want.value, '$[1]')";

/// Chunk texts of the items whose ids are in `?1` (a JSON array). `CROSS JOIN` keeps the ids as
/// the outer loop (an `IN (SELECT ...)` made SQLite scan `chunks` with a bloom filter).
const LAYOUT_CHUNKS_SQL: &str = "SELECT c.item_id, c.ord, c.text
     FROM json_each(?1) AS want CROSS JOIN chunks c ON c.item_id = want.value";

/// `refs` as the JSON array of `[source, key]` pairs that [`WANTED_ITEMS_JOIN`] reads.
pub(crate) fn refs_json(refs: &[ItemRef]) -> Result<String> {
    let pairs: Vec<[&str; 2]> = refs
        .iter()
        .map(|r| [r.source.as_str(), r.key.as_str()])
        .collect();
    Ok(serde_json::to_string(&pairs)?)
}

/// Run `f` in a read transaction, so its statements see one snapshot (an item row and the
/// chunks its layout points at agree).
fn read_snapshot<T>(c: &Connection, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    c.execute_batch("BEGIN DEFERRED").db("begin read")?;
    let res = f(c);
    let end = c.execute_batch("COMMIT").db("end read");
    match res {
        Ok(v) => end.map(|()| v),
        Err(e) => {
            let _ = c.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// The items for a [`refs_json`] array, with their texts, as `(index in the array, row, text)`.
fn find_items(c: &Connection, refs: &str) -> Result<Vec<(usize, ItemRow, Option<String>)>> {
    let rows: Vec<(usize, ItemRow)> = {
        let mut stmt = c
            .prepare_cached(&format!(
                "SELECT i.id, i.title, i.content_hash, i.mime, i.text, i.metadata,
                        i.source_modified_at, i.created_at, i.updated_at, i.version,
                        (SELECT layout FROM item_text WHERE item_id = i.id), want.key
                 {WANTED_ITEMS_JOIN}"
            ))
            .db("prepare get_items")?;
        let rows = stmt
            .query_map([refs], |r| {
                Ok((r.get::<_, i64>(11)? as usize, read_item_row(r)?))
            })
            .db("query get_items")?;
        rows.collect::<rusqlite::Result<_>>().db("read get_items")?
    };
    // Chunk texts for every item whose text is a layout, in one query.
    let layout_ids: Vec<i64> = rows
        .iter()
        .filter(|(_, r)| r.text.is_none() && r.layout.is_some())
        .map(|(_, r)| r.id)
        .collect();
    let mut chunks: HashMap<i64, HashMap<u64, String>> = HashMap::new();
    if !layout_ids.is_empty() {
        let ids = serde_json::to_string(&layout_ids)?;
        let mut stmt = c
            .prepare_cached(LAYOUT_CHUNKS_SQL)
            .db("prepare chunk texts")?;
        let mut q = stmt.query([&ids]).db("query chunk texts")?;
        while let Some(r) = q.next().db("read chunk texts")? {
            chunks
                .entry(r.get(0).db("read chunk texts")?)
                .or_default()
                .insert(
                    r.get::<_, i64>(1).db("read chunk texts")? as u64,
                    r.get(2).db("read chunk texts")?,
                );
        }
    }
    rows.into_iter()
        .map(|(idx, row)| {
            let text = match (&row.text, &row.layout) {
                (None, Some(l)) => {
                    let empty = HashMap::new();
                    let texts = chunks.get(&row.id).unwrap_or(&empty);
                    Some(
                        text_layout::decode(l, |ord| texts.get(&ord).map(String::as_str))
                            .map_err(|e| Error::Integrity(format!("item {}: {e}", row.id)))?,
                    )
                }
                (text, _) => text.clone(),
            };
            Ok((idx, row, text))
        })
        .collect()
}

/// Texts of an item's chunks by `ord`.
fn chunk_texts(conn: &Connection, item_id: i64) -> Result<HashMap<u64, String>> {
    let mut stmt = conn
        .prepare_cached("SELECT ord, text FROM chunks WHERE item_id = ?1")
        .db("prepare chunk texts")?;
    let rows = stmt
        .query_map([item_id], |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?)))
        .db("query chunk texts")?;
    rows.collect::<rusqlite::Result<_>>().db("read chunk texts")
}

/// Rebuild an item's text from its `item_text` layout and current chunks.
fn decode_layout(conn: &Connection, item_id: i64, layout: &[u8]) -> Result<String> {
    let chunks = chunk_texts(conn, item_id)?;
    text_layout::decode(layout, |ord| chunks.get(&ord).map(String::as_str))
        .map_err(|e| Error::Integrity(format!("item {item_id}: {e}")))
}

/// Rebuild item `item_id`'s text from its stored `item_text` layout, looking its chunk texts up
/// by `ord` (for readers outside this module that fetch the chunks themselves, e.g. export).
pub(crate) fn decode_text_layout<'a>(
    item_id: i64,
    layout: &[u8],
    chunk: impl Fn(u64) -> Option<&'a str>,
) -> Result<String> {
    text_layout::decode(layout, chunk).map_err(|e| Error::Integrity(format!("item {item_id}: {e}")))
}

impl ItemRow {
    /// The public item for this row, with its (already resolved) text.
    fn into_item(self, item: ItemRef, text: Option<String>) -> Result<Item> {
        Ok(Item {
            item,
            title: self.title,
            content_hash: self.content_hash,
            mime: self.mime,
            text,
            metadata: serde_json::from_str(&self.metadata)?,
            source_modified_at: self
                .source_modified_at
                .as_deref()
                .map(parse_time)
                .transpose()?,
            version: self.version,
            created_at: parse_time(&self.created_at)?,
            updated_at: parse_time(&self.updated_at)?,
        })
    }

    /// The item's text as the caller wrote it, wherever it is stored.
    fn text(&self, conn: &Connection) -> Result<Option<String>> {
        match (&self.text, &self.layout) {
            (None, Some(l)) => decode_layout(conn, self.id, l).map(Some),
            (text, _) => Ok(text.clone()),
        }
    }
}

/// Remove an item's `item_text` layout before its chunks are rewritten, returning the text it
/// held (`None` when the text lives in `items.text` or there is none).
fn take_text(tx: &Connection, item_id: i64) -> Result<Option<String>> {
    let layout: Option<Vec<u8>> = tx
        .query_row(
            "SELECT layout FROM item_text WHERE item_id = ?1",
            [item_id],
            |r| r.get(0),
        )
        .optional()
        .db("read text layout")?;
    let Some(layout) = layout else {
        return Ok(None);
    };
    let text = decode_layout(tx, item_id, &layout)?;
    tx.execute("DELETE FROM item_text WHERE item_id = ?1", [item_id])
        .db("drop text layout")?;
    Ok(Some(text))
}

fn insert_layout(tx: &Connection, item_id: i64, layout: &[u8]) -> Result<()> {
    tx.execute(
        "INSERT INTO item_text (item_id, layout) VALUES (?1, ?2)",
        params![item_id, layout],
    )
    .db("store text layout")?;
    Ok(())
}

/// Plan a layout of `text` over `chunks` (in `ord` order), if it saves space.
fn plan_layout(text: &str, chunks: &[NewChunk]) -> Option<Vec<u8>> {
    let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
    text_layout::plan(text, &texts)
}

/// Replace an item's chunks, keeping its text intact: a text held in `item_text` is re-planned
/// over the new chunks (or kept as a literal when they no longer cover it). `planned` is a
/// layout already planned over `chunks` for an item row just written with `text = NULL`.
fn rewrite_chunks(
    tx: &Connection,
    item_id: i64,
    chunks: &[NewChunk],
    dim: Option<usize>,
    planned: Option<&[u8]>,
) -> Result<Vec<ChunkId>> {
    let carried = take_text(tx, item_id)?;
    let ids = write_chunks(tx, item_id, chunks, dim)?;
    match (planned, carried) {
        (Some(layout), _) => insert_layout(tx, item_id, layout)?,
        (None, Some(text)) => {
            let layout = plan_layout(&text, chunks).unwrap_or_else(|| text_layout::literal(&text));
            insert_layout(tx, item_id, &layout)?;
        }
        (None, None) => {}
    }
    Ok(ids)
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
    /// Layout of `new.text` over the chunks written with it (see [`rewrite_chunks`]); when set,
    /// an inserted or updated row stores `text = NULL`.
    layout: Option<Vec<u8>>,
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
            layout: None,
        })
    }

    /// Like [`PreparedItem::new`], for an item written together with `chunks`.
    fn with_chunks(new: &'a NewItem, chunks: &[NewChunk]) -> Result<Self> {
        let mut p = Self::new(new)?;
        p.layout = new.text.as_deref().and_then(|t| plan_layout(t, chunks));
        Ok(p)
    }

    /// The value for the `items.text` column.
    fn text_column(&self) -> Option<&str> {
        match self.layout {
            Some(_) => None,
            None => self.new.text.as_deref(),
        }
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
                    p.text_column(),
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
            // Decoding a stored layout reads the chunks: skip it when the content changed anyway.
            let same_text = !content_changed && old.text(tx)? == new.text;
            let unchanged = same_text
                && old.title == new.title
                && old.mime == new.mime
                && old.metadata == p.metadata
                && old.version == new.version
                && modified_unchanged;
            if unchanged {
                return Ok(RowUpsert {
                    outcome: UpsertOutcome::Unchanged,
                    item_id: old.id,
                });
            }
            // Same text over the same (kept) chunks: the stored layout stays valid. With chunks
            // to write, `rewrite_chunks` then re-plans it over the new ones.
            let keep_layout = old.layout.is_some() && same_text && p.layout.is_none();
            let text_column = if keep_layout { None } else { p.text_column() };
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
                    text_column,
                    p.metadata,
                    p.modified,
                    p.now,
                    new.version,
                ],
            )
            .db("update item")?;
            if old.layout.is_some() && !keep_layout {
                tx.execute("DELETE FROM item_text WHERE item_id = ?1", [old.id])
                    .db("drop text layout")?;
            }
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
    // The approximate-search index, once built, is maintained in the same transaction.
    let mut ann = match dim {
        Some(_) => AnnWriter::prepare(tx)?,
        None => None,
    };
    for (ord, ch) in chunks.iter().enumerate() {
        ins.execute(params![item_id, ord as i64, ch.text])
            .db("insert chunk")?;
        let id = tx.last_insert_rowid();
        if let (Some(e), Some(stmt)) = (&ch.embedding, ins_vec.as_mut()) {
            stmt.execute(params![id, f32_blob(e)]).db("insert vector")?;
            if let Some(ann) = ann.as_mut() {
                ann.insert(id, e)?;
            }
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
        let dim = validate_chunks(chunks)?;
        let p = PreparedItem::with_chunks(new, chunks)?;
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
                        rewrite_chunks(&tx, row.item_id, chunks, dim, None)?;
                        UpsertOutcome::Updated
                    }
                }
                outcome => {
                    rewrite_chunks(&tx, row.item_id, chunks, dim, p.layout.as_deref())?;
                    outcome
                }
            };
            tx.commit().db("commit upsert_item_with_chunks")?;
            Ok(outcome)
        })
    }

    /// Fetch an item by reference.
    pub fn get_item(&self, item: &ItemRef) -> Result<Option<Item>> {
        let found = self.with_read_conn(|c| match find_item(c, item)? {
            Some(row) => {
                let text = row.text(c)?;
                Ok(Some((row, text)))
            }
            None => Ok(None),
        })?;
        found
            .map(|(row, text)| row.into_item(item.clone(), text))
            .transpose()
    }

    /// Fetch many items at once: `out[i]` is `items[i]` (`None` when unknown; duplicates are
    /// fine). One query per [`LOOKUP_BATCH`] items plus one for the chunk texts their stored
    /// layouts need, each batch read in one snapshot on a read connection, instead of one or two
    /// queries per item as with [`Self::get_item`].
    pub fn get_items(&self, items: &[ItemRef]) -> Result<Vec<Option<Item>>> {
        let mut out: Vec<Option<Item>> = Vec::with_capacity(items.len());
        for batch in items.chunks(LOOKUP_BATCH) {
            let refs = refs_json(batch)?;
            let found = self.with_read_conn(|c| read_snapshot(c, |c| find_items(c, &refs)))?;
            let mut rows: Vec<Option<(ItemRow, Option<String>)>> =
                std::iter::repeat_with(|| None).take(batch.len()).collect();
            for (idx, row, text) in found {
                if let Some(slot) = rows.get_mut(idx) {
                    *slot = Some((row, text));
                }
            }
            for (item, row) in batch.iter().zip(rows) {
                out.push(
                    row.map(|(row, text)| row.into_item(item.clone(), text))
                        .transpose()?,
                );
            }
        }
        Ok(out)
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
            let ids = rewrite_chunks(&tx, item_id, chunks, dim, None)?;
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
        blob.as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Long enough that storing it as a layout over its chunk pays off.
    const BODY: &str = "a body long enough to be kept once, as a layout over its single chunk, \
                        rather than twice (in the item row and in the chunk)";

    fn plan(c: &Connection, sql: &str) -> Vec<String> {
        let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let nulls = vec![rusqlite::types::Null; stmt.parameter_count()];
        stmt.query_map(rusqlite::params_from_iter(nulls), |r| r.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// The batched lookups reach `items` and `chunks` through their indexes, once per wanted
    /// ref, never with a table scan (also after `ANALYZE`, when the planner has statistics).
    #[test]
    fn batched_lookups_use_indexes() {
        let s = Store::open_in_memory().unwrap();
        for i in 0..50 {
            let it = NewItem::new("src", format!("k{i}"), format!("h{i}")).with_text(BODY);
            s.upsert_item_with_chunks(&it, &[NewChunk::text(BODY)])
                .unwrap();
        }
        let statements = [
            format!("SELECT i.id, want.key {WANTED_ITEMS_JOIN}"),
            LAYOUT_CHUNKS_SQL.to_owned(),
        ];
        let layouts: i64 = s
            .with_conn(|c| {
                c.query_row("SELECT count(*) FROM item_text", [], |r| r.get(0))
                    .db("count layouts")
            })
            .unwrap();
        assert_eq!(layouts, 50, "texts are stored as layouts");
        let refs: Vec<ItemRef> = (0..50)
            .map(|i| ItemRef {
                source: SourceId::new("src"),
                key: ItemKey::new(format!("k{i}")),
            })
            .collect();
        let items = s.get_items(&refs).unwrap();
        assert!(
            items
                .iter()
                .all(|i| i.as_ref().unwrap().text.as_deref() == Some(BODY))
        );
        for analyze in [false, true] {
            s.with_conn(|c| {
                if analyze {
                    c.execute_batch("ANALYZE;").db("analyze")?;
                }
                for sql in &statements {
                    let steps = plan(c, sql);
                    let scans: Vec<_> = steps
                        .iter()
                        .filter(|d| d.starts_with("SCAN ") && !d.contains("VIRTUAL TABLE"))
                        .collect();
                    assert!(scans.is_empty(), "{sql}: {steps:#?}");
                }
                Ok(())
            })
            .unwrap();
        }
    }
}
