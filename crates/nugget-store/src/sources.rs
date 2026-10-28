//! Per-source operations used by the ingest pipeline and retrieval.

use std::collections::HashMap;

use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_core::{Error, Result};
use rusqlite::{OptionalExtension, TransactionBehavior, params, params_from_iter};

use crate::Store;
use crate::error::DbExt;
use crate::items::{embedding_dim, f32_from_blob, item_ref};
use crate::types::{Chunk, ChunkId, SourceCounts};

/// Upper bound on ids per `get_chunks`, `get_chunks_by_ord` and `get_embeddings` call (keeps
/// SQL parameter lists and result sizes bounded).
pub const MAX_CHUNK_IDS: usize = 512;
/// Largest radius accepted by [`Store::neighbours`] (the window is at most `2 * radius + 1`).
pub const MAX_NEIGHBOUR_RADIUS: u32 = 256;

const CHUNK_COLUMNS: &str = "c.id, i.source, i.key, c.ord, c.text, i.title";

fn read_chunk(r: &rusqlite::Row<'_>) -> rusqlite::Result<Chunk> {
    Ok(Chunk {
        id: r.get(0)?,
        item: item_ref(r.get(1)?, r.get(2)?),
        ord: r.get::<_, i64>(3)? as u32,
        text: r.get(4)?,
        title: r.get(5)?,
    })
}

impl Store {
    /// Delete every item of a source (with chunks, full-text entries and vectors) and its cursor.
    /// Returns the number of items removed.
    pub fn delete_source(&self, source: &SourceId) -> Result<u64> {
        self.with_conn(|conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .db("begin delete_source")?;
            let n = tx
                .execute("DELETE FROM items WHERE source = ?1", [source.as_str()])
                .db("delete source items")?;
            tx.execute("DELETE FROM cursors WHERE source = ?1", [source.as_str()])
                .db("delete source cursor")?;
            tx.commit().db("commit delete_source")?;
            Ok(n as u64)
        })
    }

    /// Every item key of a source with its connector-reported version, for reconciling a
    /// connector's view (e.g. a directory scan) against what is stored.
    pub fn item_versions(&self, source: &SourceId) -> Result<HashMap<ItemKey, Option<String>>> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT key, version FROM items WHERE source = ?1")
                .db("prepare item_versions")?;
            let rows = stmt
                .query_map([source.as_str()], |r| {
                    Ok((ItemKey::new(r.get::<_, String>(0)?), r.get(1)?))
                })
                .db("query item_versions")?;
            rows.collect::<rusqlite::Result<_>>()
                .db("read item_versions")
        })
    }

    /// Fetch chunks by id, in the order given. Unknown ids are skipped. At most
    /// [`MAX_CHUNK_IDS`] ids per call (`Error::Invalid` otherwise).
    pub fn get_chunks(&self, ids: &[ChunkId]) -> Result<Vec<Chunk>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        if ids.len() > MAX_CHUNK_IDS {
            return Err(nugget_core::Error::Invalid(format!(
                "get_chunks: at most {MAX_CHUNK_IDS} ids per call"
            )));
        }
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT {CHUNK_COLUMNS}
             FROM chunks c JOIN items i ON i.id = c.item_id
             WHERE c.id IN ({placeholders})"
        );
        let mut found: HashMap<ChunkId, Chunk> = self.with_conn(|conn| {
            let mut stmt = conn.prepare(&sql).db("prepare get_chunks")?;
            let rows = stmt
                .query_map(params_from_iter(ids.iter()), |r| {
                    read_chunk(r).map(|c| (c.id, c))
                })
                .db("query get_chunks")?;
            rows.collect::<rusqlite::Result<_>>().db("read get_chunks")
        })?;
        Ok(ids.iter().filter_map(|id| found.remove(id)).collect())
    }

    /// Fetch an item's chunks by position (`ord`, from 0), in the order given. Positions the item
    /// does not have (and unknown items) are skipped. At most [`MAX_CHUNK_IDS`] positions per
    /// call (`Error::Invalid` otherwise).
    pub fn get_chunks_by_ord(&self, item: &ItemRef, ords: &[u32]) -> Result<Vec<Chunk>> {
        if ords.is_empty() {
            return Ok(Vec::new());
        }
        if ords.len() > MAX_CHUNK_IDS {
            return Err(Error::Invalid(format!(
                "get_chunks_by_ord: at most {MAX_CHUNK_IDS} positions per call"
            )));
        }
        let found: HashMap<u32, Chunk> = self.with_conn(|conn| {
            // One indexed point lookup per position, through the UNIQUE (item_id, ord) index.
            let mut stmt = conn
                .prepare_cached(&format!(
                    "SELECT {CHUNK_COLUMNS}
                     FROM items i JOIN chunks c ON c.item_id = i.id
                     WHERE i.source = ?1 AND i.key = ?2 AND c.ord = ?3"
                ))
                .db("prepare get_chunks_by_ord")?;
            let mut out = HashMap::with_capacity(ords.len());
            for &ord in ords {
                if out.contains_key(&ord) {
                    continue;
                }
                let chunk = stmt
                    .query_row(
                        params![item.source.as_str(), item.key.as_str(), i64::from(ord)],
                        read_chunk,
                    )
                    .optional()
                    .db("get_chunks_by_ord")?;
                if let Some(c) = chunk {
                    out.insert(ord, c);
                }
            }
            Ok(out)
        })?;
        Ok(ords.iter().filter_map(|o| found.get(o).cloned()).collect())
    }

    /// The chunks of `chunk`'s item whose position is within `radius` of it (`ord - radius ..=
    /// ord + radius`, clamped to the item), in document order, **including `chunk` itself**.
    /// Empty if the chunk does not exist. `radius` is capped at [`MAX_NEIGHBOUR_RADIUS`].
    ///
    /// Positions come from the stored `ord`, never from chunk ids, so this stays correct however
    /// ids were assigned.
    pub fn neighbours(&self, chunk: ChunkId, radius: u32) -> Result<Vec<Chunk>> {
        let radius = i64::from(radius.min(MAX_NEIGHBOUR_RADIUS));
        self.with_conn(|conn| {
            let Some((item_id, ord)) = conn
                .query_row(
                    "SELECT item_id, ord FROM chunks WHERE id = ?1",
                    [chunk],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
                )
                .optional()
                .db("find chunk")?
            else {
                return Ok(Vec::new());
            };
            let mut stmt = conn
                .prepare_cached(&format!(
                    "SELECT {CHUNK_COLUMNS}
                     FROM chunks c JOIN items i ON i.id = c.item_id
                     WHERE c.item_id = ?1 AND c.ord BETWEEN ?2 AND ?3
                     ORDER BY c.ord"
                ))
                .db("prepare neighbours")?;
            let rows = stmt
                .query_map(params![item_id, ord - radius, ord + radius], read_chunk)
                .db("query neighbours")?;
            rows.collect::<rusqlite::Result<_>>().db("read neighbours")
        })
    }

    /// Stored embeddings of the given chunks, read back from the vector index. Chunks without a
    /// vector (stored text-only) and unknown ids are absent from the map; while the store holds
    /// no vectors at all the map is empty. At most [`MAX_CHUNK_IDS`] ids per call
    /// (`Error::Invalid` otherwise).
    pub fn get_embeddings(&self, ids: &[ChunkId]) -> Result<HashMap<ChunkId, Vec<f32>>> {
        if ids.len() > MAX_CHUNK_IDS {
            return Err(Error::Invalid(format!(
                "get_embeddings: at most {MAX_CHUNK_IDS} ids per call"
            )));
        }
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        self.with_conn(|conn| {
            let Some(dim) = embedding_dim(conn)? else {
                return Ok(HashMap::new());
            };
            // `vec0` answers `rowid = ?` with a point lookup (no kNN scan).
            let mut stmt = conn
                .prepare_cached("SELECT embedding FROM chunk_vec WHERE rowid = ?1")
                .db("prepare get_embeddings")?;
            let mut out = HashMap::with_capacity(ids.len());
            for &id in ids {
                if out.contains_key(&id) {
                    continue;
                }
                let blob: Option<Vec<u8>> = stmt
                    .query_row([id], |r| r.get(0))
                    .optional()
                    .db("get_embeddings")?;
                if let Some(blob) = blob {
                    let v = f32_from_blob(&blob)
                        .filter(|v| v.len() == dim)
                        .ok_or_else(|| {
                            Error::Storage(format!("malformed stored vector for chunk {id}"))
                        })?;
                    out.insert(id, v);
                }
            }
            Ok(out)
        })
    }

    /// Items with at least one chunk stored without an embedding (indexed text-only, e.g.
    /// while no embedder was available), ordered by `(source, key)`, at most `limit`, starting
    /// after `after` (keyset pagination: pass the last item of the previous page). Junk and
    /// title-less items without chunks are never listed.
    pub fn items_missing_embeddings(
        &self,
        after: Option<&ItemRef>,
        limit: usize,
    ) -> Result<Vec<ItemRef>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let (src, key) = after.map_or(("", ""), |a| (a.source.as_str(), a.key.as_str()));
        self.with_conn(|conn| {
            // Without a vector table no chunk has a vector yet.
            let missing = if embedding_dim(conn)?.is_some() {
                "NOT EXISTS (SELECT 1 FROM chunk_vec v WHERE v.rowid = c.id)"
            } else {
                "1"
            };
            let sql = format!(
                "SELECT i.source, i.key FROM items i
                 WHERE (i.source > ?1 OR (i.source = ?1 AND i.key > ?2))
                   AND EXISTS (SELECT 1 FROM chunks c WHERE c.item_id = i.id AND {missing})
                 ORDER BY i.source, i.key
                 LIMIT ?3"
            );
            let mut stmt = conn.prepare(&sql).db("prepare items_missing_embeddings")?;
            let rows = stmt
                .query_map(params![src, key, limit], |r| {
                    Ok(item_ref(r.get(0)?, r.get(1)?))
                })
                .db("query items_missing_embeddings")?;
            rows.collect::<rusqlite::Result<_>>()
                .db("read items_missing_embeddings")
        })
    }

    /// Item and chunk counts per source, sorted by source id.
    pub fn source_counts(&self) -> Result<Vec<SourceCounts>> {
        self.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT i.source, COUNT(DISTINCT i.id), COUNT(c.id)
                     FROM items i LEFT JOIN chunks c ON c.item_id = i.id
                     GROUP BY i.source ORDER BY i.source",
                )
                .db("prepare source_counts")?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(SourceCounts {
                        source: SourceId::new(r.get::<_, String>(0)?),
                        items: r.get::<_, i64>(1)? as u64,
                        chunks: r.get::<_, i64>(2)? as u64,
                    })
                })
                .db("query source_counts")?;
            rows.collect::<rusqlite::Result<_>>()
                .db("read source_counts")
        })
    }
}
