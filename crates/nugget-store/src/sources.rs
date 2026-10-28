//! Per-source operations used by the ingest pipeline and retrieval.

use std::collections::HashMap;

use nugget_core::Result;
use nugget_core::ids::{ItemKey, SourceId};
use rusqlite::{TransactionBehavior, params_from_iter};

use crate::Store;
use crate::error::DbExt;
use crate::items::item_ref;
use crate::types::{Chunk, ChunkId, SourceCounts};

/// Upper bound on ids per `get_chunks` call (keeps the SQL parameter list bounded).
pub const MAX_CHUNK_IDS: usize = 512;

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
            "SELECT c.id, i.source, i.key, c.ord, c.text, i.title
             FROM chunks c JOIN items i ON i.id = c.item_id
             WHERE c.id IN ({placeholders})"
        );
        let mut found: HashMap<ChunkId, Chunk> = self.with_conn(|conn| {
            let mut stmt = conn.prepare(&sql).db("prepare get_chunks")?;
            let rows = stmt
                .query_map(params_from_iter(ids.iter()), |r| {
                    let id: ChunkId = r.get(0)?;
                    Ok((
                        id,
                        Chunk {
                            id,
                            item: item_ref(r.get(1)?, r.get(2)?),
                            ord: r.get::<_, i64>(3)? as u32,
                            text: r.get(4)?,
                            title: r.get(5)?,
                        },
                    ))
                })
                .db("query get_chunks")?;
            rows.collect::<rusqlite::Result<_>>().db("read get_chunks")
        })?;
        Ok(ids.iter().filter_map(|id| found.remove(id)).collect())
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
