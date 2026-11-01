//! Re-indexing under another embedding model, with an atomic switch.
//!
//! [`Store::begin_reindex`] records the target model and opens a staging table, `chunk_vec_next`
//! (its rows go with their chunk). The caller fills it chunk by chunk ([`Store::reindex_pending`],
//! [`Store::put_reindexed`]), across restarts if need be, while searches keep using `chunk_vec`.
//! [`Store::finish_reindex`] then swaps in one transaction: the new vectors replace the old ones
//! and the index records the new model and prefixes, or nothing changes.

use std::sync::PoisonError;

use nugget_core::{Error, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::Store;
use crate::error::DbExt;
use crate::items::{META_EMBEDDING_DIM, f32_blob, validate_embedding};
use crate::store::{META_EMBEDDING_MODEL, META_EMBEDDING_PREFIXES};
use crate::types::ChunkId;
use crate::vector::{META_ANN_CENTER, META_ANN_READY};

/// `meta` key: the re-index in progress ([`ReindexTarget`] as JSON).
const META_REINDEX: &str = "reindex_target";

/// The embedding model a re-index is moving to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReindexTarget {
    pub model: String,
    /// Task prefixes `(query, document)` of the new model.
    pub query_prefix: String,
    pub document_prefix: String,
}

fn target(conn: &Connection) -> Result<Option<ReindexTarget>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [META_REINDEX],
            |r| r.get(0),
        )
        .optional()
        .db("read re-index target")?;
    raw.map(|s| {
        serde_json::from_str(&s).map_err(|e| Error::Storage(format!("bad re-index target: {e}")))
    })
    .transpose()
}

fn missing(conn: &Connection) -> Result<u64> {
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM chunks c
             WHERE NOT EXISTS (SELECT 1 FROM chunk_vec_next n WHERE n.chunk_id = c.id)",
            [],
            |r| r.get(0),
        )
        .db("count chunks to re-index")?;
    Ok(n as u64)
}

impl Store {
    /// Start (or keep) a re-index to `target`. A re-index to another model is replaced.
    pub fn begin_reindex(&self, t: &ReindexTarget) -> Result<()> {
        let json = serde_json::to_string(t).map_err(|e| Error::Storage(e.to_string()))?;
        self.with_conn(|c| {
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .db("begin re-index")?;
            if target(&tx)?.is_some_and(|old| old.model != t.model) {
                tx.execute("DROP TABLE IF EXISTS chunk_vec_next", [])
                    .db("drop staged vectors")?;
            }
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS chunk_vec_next (
                     chunk_id  INTEGER PRIMARY KEY REFERENCES chunks (id) ON DELETE CASCADE,
                     embedding BLOB NOT NULL
                 );",
            )
            .db("create staged vectors")?;
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                params![META_REINDEX, json],
            )
            .db("record re-index target")?;
            tx.commit().db("commit re-index start")
        })
    }

    /// The re-index in progress, if any.
    pub fn reindex_target(&self) -> Result<Option<ReindexTarget>> {
        self.with_read_conn(target)
    }

    /// Up to `limit` chunks `(id, text)` without a staged vector, by id, after chunk `after`.
    pub fn reindex_pending(&self, after: ChunkId, limit: usize) -> Result<Vec<(ChunkId, String)>> {
        self.with_read_conn(|c| {
            let mut st = c
                .prepare_cached(
                    "SELECT c.id, c.text FROM chunks c
                     WHERE c.id > ?1
                       AND NOT EXISTS (SELECT 1 FROM chunk_vec_next n WHERE n.chunk_id = c.id)
                     ORDER BY c.id LIMIT ?2",
                )
                .db("prepare re-index page")?;
            let rows = st
                .query_map(params![after, limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))
                .db("read re-index page")?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .db("read re-index page")
        })
    }

    /// Stage new vectors. Chunks deleted meanwhile are skipped (their replacements are pending).
    pub fn put_reindexed(&self, vectors: &[(ChunkId, Vec<f32>)]) -> Result<()> {
        for (_, v) in vectors {
            validate_embedding(v).map_err(Error::Invalid)?;
        }
        self.with_conn(|c| {
            let tx = c.transaction().db("begin staging")?;
            {
                let mut st = tx
                    .prepare_cached(
                        "INSERT OR REPLACE INTO chunk_vec_next (chunk_id, embedding)
                         SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM chunks WHERE id = ?1)",
                    )
                    .db("prepare staging")?;
                for (id, v) in vectors {
                    st.execute(params![id, f32_blob(v)]).db("stage vector")?;
                }
            }
            tx.commit().db("commit staging")
        })
    }

    /// Chunks `(staged, total)` of the re-index in progress.
    pub fn reindex_progress(&self) -> Result<(u64, u64)> {
        self.with_read_conn(|c| {
            let total: i64 = c
                .query_row("SELECT count(*) FROM chunks", [], |r| r.get(0))
                .db("count chunks")?;
            let total = total as u64;
            if target(c)?.is_none() {
                return Ok((0, total));
            }
            Ok((total - missing(c)?.min(total), total))
        })
    }

    /// Switch to the staged vectors if every chunk has one: in one transaction the vector table
    /// is rebuilt from them and the index records the target model and prefixes. Returns the
    /// target switched to, or `None` (nothing changed) while chunks are still pending.
    pub fn finish_reindex(&self) -> Result<Option<ReindexTarget>> {
        // No approximate-index build may run while its tables are dropped.
        let build = self.db().ann_build.clone();
        let _build = build.lock().unwrap_or_else(PoisonError::into_inner);
        self.with_conn(|c| {
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .db("begin re-index switch")?;
            let Some(t) = target(&tx)? else {
                return Ok(None);
            };
            if missing(&tx)? > 0 {
                return Ok(None);
            }
            let dim: Option<i64> = tx
                .query_row(
                    "SELECT length(embedding) / 4 FROM chunk_vec_next LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()
                .db("read staged dimension")?;
            tx.execute_batch(
                "DROP TRIGGER IF EXISTS chunks_ann_ad;
                 DROP TABLE IF EXISTS ann_bits;
                 DROP TABLE IF EXISTS ann_vectors;
                 DROP TRIGGER IF EXISTS chunks_vec_ad;
                 DROP TABLE IF EXISTS chunk_vec;",
            )
            .db("drop old vectors")?;
            tx.execute(
                "DELETE FROM meta WHERE key IN (?1, ?2, ?3, ?4)",
                params![
                    META_ANN_READY,
                    META_ANN_CENTER,
                    META_EMBEDDING_DIM,
                    META_REINDEX
                ],
            )
            .db("forget old vectors")?;
            if let Some(dim) = dim {
                tx.execute_batch(&format!(
                    "CREATE VIRTUAL TABLE chunk_vec
                         USING vec0 (embedding float[{dim}] distance_metric=cosine);
                     CREATE TRIGGER chunks_vec_ad AFTER DELETE ON chunks BEGIN
                         DELETE FROM chunk_vec WHERE rowid = old.id;
                     END;
                     INSERT INTO chunk_vec (rowid, embedding)
                         SELECT chunk_id, embedding FROM chunk_vec_next;"
                ))
                .db("build new vectors")?;
                tx.execute(
                    "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                    params![META_EMBEDDING_DIM, dim.to_string()],
                )
                .db("record embedding dimension")?;
            }
            let prefixes =
                serde_json::json!({"query": t.query_prefix, "document": t.document_prefix});
            tx.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2), (?3, ?4)",
                params![
                    META_EMBEDDING_MODEL,
                    t.model,
                    META_EMBEDDING_PREFIXES,
                    prefixes.to_string()
                ],
            )
            .db("record new embedding model")?;
            tx.execute("DROP TABLE chunk_vec_next", [])
                .db("drop staged vectors")?;
            tx.commit().db("commit re-index switch")?;
            Ok(Some(t))
        })
    }

    /// Abandon the re-index in progress, keeping the current vectors.
    pub fn cancel_reindex(&self) -> Result<()> {
        self.with_conn(|c| {
            let tx = c.transaction().db("begin re-index cancel")?;
            tx.execute("DROP TABLE IF EXISTS chunk_vec_next", [])
                .db("drop staged vectors")?;
            tx.execute("DELETE FROM meta WHERE key = ?1", [META_REINDEX])
                .db("forget re-index target")?;
            tx.commit().db("commit re-index cancel")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{NewChunk, NewItem};

    fn add(s: &Store, key: &str, v: Vec<f32>) {
        s.adopt_embedding_model("old").unwrap();
        s.upsert_item_with_chunks(
            &NewItem::new("notes", key, key),
            &[NewChunk::embedded(format!("text of {key}"), v)],
        )
        .unwrap();
    }

    #[test]
    fn searches_use_the_old_vectors_until_every_chunk_is_staged_then_switch_at_once() {
        let s = Store::open_in_memory().unwrap();
        add(&s, "a", vec![1.0, 0.0, 0.0]);
        add(&s, "b", vec![0.0, 1.0, 0.0]);
        let t = ReindexTarget {
            model: "new".into(),
            query_prefix: "q: ".into(),
            document_prefix: "d: ".into(),
        };
        s.begin_reindex(&t).unwrap();
        let pending = s.reindex_pending(0, 10).unwrap();
        assert_eq!(pending.len(), 2);
        s.put_reindexed(&[(pending[0].0, vec![0.0, 0.0, 0.0, 1.0])])
            .unwrap();
        assert_eq!(s.reindex_progress().unwrap(), (1, 2));
        // Not complete: nothing changes.
        assert_eq!(s.finish_reindex().unwrap(), None);
        assert_eq!(s.embedding_model().unwrap().as_deref(), Some("old"));
        assert_eq!(s.embedding_dim().unwrap(), Some(3));
        assert_eq!(s.search_vector(&[1.0, 0.0, 0.0], 1).unwrap().len(), 1);

        // Resumes after the staged chunk; then the switch is complete.
        let rest = s.reindex_pending(0, 10).unwrap();
        assert_eq!(rest, pending[1..]);
        s.put_reindexed(&[(rest[0].0, vec![0.0, 0.0, 1.0, 0.0])])
            .unwrap();
        assert_eq!(s.finish_reindex().unwrap(), Some(t));
        assert_eq!(s.embedding_model().unwrap().as_deref(), Some("new"));
        assert_eq!(s.embedding_dim().unwrap(), Some(4));
        assert_eq!(
            s.embedding_prefixes().unwrap(),
            Some(("q: ".into(), "d: ".into()))
        );
        assert_eq!(s.reindex_target().unwrap(), None);
        let hits = s.search_vector(&[0.0, 0.0, 1.0, 0.0], 1).unwrap();
        assert_eq!(hits[0].item.key.as_str(), "b");
        // Cancelling afterwards is harmless.
        s.cancel_reindex().unwrap();
    }
}
