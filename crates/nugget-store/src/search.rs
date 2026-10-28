//! Full-text (FTS5 bm25), vector (`sqlite-vec` cosine kNN) and hybrid (RRF) search.

use std::collections::HashMap;

use nugget_core::{Error, Result};
use rusqlite::{OptionalExtension, params};

use crate::Store;
use crate::error::DbExt;
use crate::items::{embedding_dim, f32_blob, item_ref, validate_embedding};
use crate::types::{ChunkId, Hit};

/// RRF damping constant (Cormack et al. 2009).
const RRF_K: f64 = 60.0;
/// Hybrid search pulls this many candidates per list for each requested hit...
const HYBRID_OVERFETCH: usize = 4;
/// ...but at least this many.
const HYBRID_MIN_CANDIDATES: usize = 50;
/// `sqlite-vec` caps `k` for kNN queries.
const MAX_KNN: usize = 4096;
/// Most query terms used for full-text search.
const MAX_QUERY_TERMS: usize = 32;
/// Snippet length for vector hits (characters).
const SNIPPET_CHARS: usize = 200;

/// Turn free text into a safe FTS5 query: alphanumeric terms, each quoted (so FTS5 syntax in user
/// input is inert), OR-ed so bm25 ranks documents matching more terms higher.
fn fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(MAX_QUERY_TERMS)
        .map(|t| format!("\"{t}\""))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

fn excerpt(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(SNIPPET_CHARS) {
        None => text.to_owned(),
        Some((cut, _)) => {
            // Prefer to cut at a word boundary.
            let head = &text[..cut];
            let head = head
                .rsplit_once(char::is_whitespace)
                .map_or(head, |(h, _)| h);
            format!("{}…", head.trim_end())
        }
    }
}

impl Store {
    /// Full-text search over chunks, ranked by bm25 (best first).
    ///
    /// The query is treated as plain words: punctuation and FTS5 operators are ignored. Words are
    /// stemmed (Porter) and diacritics folded.
    pub fn search_text(&self, query: &str, limit: usize) -> Result<Vec<Hit>> {
        let Some(q) = fts_query(query) else {
            return Ok(Vec::new());
        };
        if limit == 0 {
            return Ok(Vec::new());
        }
        self.with_conn(|c| {
            let mut stmt = c
                .prepare_cached(
                    "SELECT ch.id, i.source, i.key, i.title,
                            snippet(chunks_fts, 0, '', '', '…', 24), bm25(chunks_fts)
                     FROM chunks_fts
                     JOIN chunks ch ON ch.id = chunks_fts.rowid
                     JOIN items i ON i.id = ch.item_id
                     WHERE chunks_fts MATCH ?1
                     ORDER BY bm25(chunks_fts), ch.id
                     LIMIT ?2",
                )
                .db("prepare text search")?;
            let rows = stmt
                .query_map(params![q, limit as i64], |r| {
                    let rank: f64 = r.get(5)?;
                    Ok(Hit {
                        chunk_id: r.get(0)?,
                        item: item_ref(r.get(1)?, r.get(2)?),
                        title: r.get(3)?,
                        snippet: r.get(4)?,
                        score: -rank as f32,
                    })
                })
                .db("text search")?;
            rows.collect::<rusqlite::Result<_>>().db("text search")
        })
    }

    /// k-nearest-neighbour search by cosine distance (best first; `score` is cosine similarity).
    ///
    /// Returns `Error::Invalid` if the query's dimension differs from the store's. An empty result
    /// is returned while no embeddings have been stored.
    pub fn search_vector(&self, embedding: &[f32], limit: usize) -> Result<Vec<Hit>> {
        validate_embedding(embedding).map_err(Error::Invalid)?;
        self.with_conn(|c| {
            let Some(dim) = embedding_dim(c)? else {
                return Ok(Vec::new());
            };
            if dim != embedding.len() {
                return Err(Error::Invalid(format!(
                    "query embedding dimension {} does not match this store's dimension {dim}",
                    embedding.len()
                )));
            }
            let k = limit.min(MAX_KNN);
            if k == 0 {
                return Ok(Vec::new());
            }
            // vec0 kNN queries must stand alone (no joins or extra ORDER BY), so fetch the
            // neighbours first, then their chunk and item rows.
            let neighbours: Vec<(ChunkId, f64)> = {
                let mut knn = c
                    .prepare_cached(
                        "SELECT rowid, distance FROM chunk_vec
                         WHERE embedding MATCH ?1 AND k = ?2
                         ORDER BY distance",
                    )
                    .db("prepare vector search")?;
                let rows = knn
                    .query_map(params![f32_blob(embedding), k as i64], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })
                    .db("vector search")?;
                rows.collect::<rusqlite::Result<_>>().db("vector search")?
            };
            let mut details = c
                .prepare_cached(
                    "SELECT i.source, i.key, i.title, ch.text
                     FROM chunks ch JOIN items i ON i.id = ch.item_id
                     WHERE ch.id = ?1",
                )
                .db("prepare chunk lookup")?;
            let mut hits = Vec::with_capacity(neighbours.len());
            for (chunk_id, distance) in neighbours {
                let hit = details
                    .query_row([chunk_id], |r| {
                        let text: String = r.get(3)?;
                        Ok(Hit {
                            chunk_id,
                            item: item_ref(r.get(0)?, r.get(1)?),
                            title: r.get(2)?,
                            snippet: excerpt(&text),
                            score: (1.0 - distance) as f32,
                        })
                    })
                    .optional()
                    .db("chunk lookup")?;
                // A vector without a chunk cannot happen (the delete trigger removes both), but
                // skipping it is the safe reading if it ever does.
                hits.extend(hit);
            }
            // Equal distances: order by chunk id for determinism.
            hits.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then(a.chunk_id.cmp(&b.chunk_id))
            });
            Ok(hits)
        })
    }

    /// Hybrid search: full-text and (if an embedding is given) vector results fused with
    /// reciprocal-rank fusion, `score = Σ 1 / (60 + rank)`. Chunks found by both lists rise to
    /// the top. Snippets come from the full-text hit when there is one.
    pub fn search_hybrid(
        &self,
        query: &str,
        embedding: Option<&[f32]>,
        limit: usize,
    ) -> Result<Vec<Hit>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let candidates = limit
            .saturating_mul(HYBRID_OVERFETCH)
            .max(HYBRID_MIN_CANDIDATES);
        let text = self.search_text(query, candidates)?;
        let vector = match embedding {
            Some(e) => self.search_vector(e, candidates.min(MAX_KNN))?,
            None => Vec::new(),
        };
        Ok(rrf_fuse([text, vector], limit))
    }
}

/// Reciprocal-rank fusion of ranked lists (each best-first). The first list's hit wins for the
/// snippet; ties break on chunk id for determinism.
fn rrf_fuse<const N: usize>(lists: [Vec<Hit>; N], limit: usize) -> Vec<Hit> {
    let mut fused: HashMap<ChunkId, (f64, Hit)> = HashMap::new();
    for list in lists {
        for (rank0, hit) in list.into_iter().enumerate() {
            let contribution = 1.0 / (RRF_K + rank0 as f64 + 1.0);
            fused
                .entry(hit.chunk_id)
                .and_modify(|(score, _)| *score += contribution)
                .or_insert((contribution, hit));
        }
    }
    let mut out: Vec<(f64, Hit)> = fused.into_values().collect();
    out.sort_by(|(sa, a), (sb, b)| sb.total_cmp(sa).then(a.chunk_id.cmp(&b.chunk_id)));
    out.into_iter()
        .take(limit)
        .map(|(score, hit)| Hit {
            score: score as f32,
            ..hit
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_query_neutralises_syntax() {
        assert_eq!(fts_query("  "), None);
        assert_eq!(fts_query("\"(*)-^"), None);
        assert_eq!(
            fts_query("rust AND \"tokio\" NEAR(x"),
            Some("\"rust\" OR \"AND\" OR \"tokio\" OR \"NEAR\" OR \"x\"".into())
        );
        assert_eq!(fts_query("café über"), Some("\"café\" OR \"über\"".into()));
    }

    #[test]
    fn excerpt_truncates_on_char_and_word_boundaries() {
        assert_eq!(excerpt(" short "), "short");
        let long = "é".repeat(150) + " " + &"word ".repeat(40);
        let e = excerpt(&long);
        assert!(e.ends_with('…'));
        assert!(e.chars().count() <= SNIPPET_CHARS + 1);
    }

    fn hit(id: ChunkId) -> Hit {
        Hit {
            item: item_ref("s".into(), id.to_string()),
            chunk_id: id,
            title: None,
            snippet: format!("snippet {id}"),
            score: 0.0,
        }
    }

    #[test]
    fn rrf_rewards_agreement() {
        let fused = rrf_fuse([vec![hit(1), hit(2), hit(3)], vec![hit(4), hit(2)]], 10);
        let ids: Vec<_> = fused.iter().map(|h| h.chunk_id).collect();
        assert_eq!(ids[0], 2, "found by both lists");
        assert_eq!(ids.len(), 4);
        // 1 and 4 are both rank 1 in one list: equal score, tie broken by id.
        assert_eq!(&ids[1..3], &[1, 4]);
        assert!((fused[0].score - (1.0 / 62.0 + 1.0 / 62.0) as f32).abs() < 1e-6);
        assert_eq!(rrf_fuse([vec![hit(1), hit(2)]], 1).len(), 1);
    }
}
