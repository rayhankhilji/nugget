//! Full-text (FTS5 bm25), vector (`sqlite-vec` cosine kNN) and hybrid (RRF) search.

use std::collections::{HashMap, HashSet};

use nugget_core::{Error, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Store;
use crate::error::DbExt;
use crate::fts;
use crate::items::{embedding_dim, f32_blob, item_ref, validate_embedding};
use crate::types::{ChunkId, Hit};

/// Weight of an item's title bm25 score, added to the bm25 score of each of its matching chunks
/// in text search. Kept low on purpose: titles are often uninformative (file names, "Re: quick
/// question"). Measured (M1-J): on the `nugget-retrieve` eval fixtures (informative titles) 0.25
/// lifts every metric over title-less search (hybrid MRR@10 0.758 → 0.780) and higher weights
/// help further (1.0: 0.812); on the synthetic benchmark corpus (titles of random topic words,
/// queries from bodies) hybrid recall@10 falls from 0.936 (0) to 0.930 (0.25), 0.920 (0.5) and
/// 0.896 (1.0). 0.5 and 0.75 also cost text-only recall@5 on the fixtures.
pub const TITLE_WEIGHT: f64 = 0.25;
/// RRF damping constant (Cormack et al. 2009).
const RRF_K: f64 = 60.0;
/// Hybrid search pulls this many candidates per list for each requested hit...
const HYBRID_OVERFETCH: usize = 4;
/// ...but at least this many.
const HYBRID_MIN_CANDIDATES: usize = 50;
/// `sqlite-vec` caps `k` for kNN queries.
const MAX_KNN: usize = 4096;
/// Snippet length for vector hits (characters).
const SNIPPET_CHARS: usize = 200;

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
    /// Full-text search over chunks **and item titles**, ranked by bm25 (best first).
    ///
    /// The query is plain words (OR-ed, so chunks matching more of them rank higher) plus a small
    /// syntax: `"quoted phrases"`, `word*` prefixes (2+ characters) and `-word` / `-"phrase"`
    /// exclusions (only alongside a positive term). Anything else, FTS5 operators included, is
    /// treated as plain text; see the `fts` module for the exact rules. At most 32 words are
    /// used. Words are stemmed (Porter) and diacritics folded.
    ///
    /// Titles live in their own FTS5 index. A title match adds [`TITLE_WEIGHT`] × its bm25 score
    /// to every matching chunk of that item, and an item whose title matches but none of whose
    /// chunks do is represented by its first chunk (snippet: the chunk's opening text). Items
    /// without chunks cannot be returned (a hit is a chunk).
    pub fn search_text(&self, query: &str, limit: usize) -> Result<Vec<Hit>> {
        let Some(q) = fts::match_expr(query) else {
            return Ok(Vec::new());
        };
        if limit == 0 {
            return Ok(Vec::new());
        }
        self.with_conn(|c| text_hits(c, &q, limit))
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
    ///
    /// Each list contributes its top `max(4 × limit, 50)` hits; see
    /// [`Store::search_hybrid_with_depth`] to choose that depth.
    pub fn search_hybrid(
        &self,
        query: &str,
        embedding: Option<&[f32]>,
        limit: usize,
    ) -> Result<Vec<Hit>> {
        let depth = limit
            .saturating_mul(HYBRID_OVERFETCH)
            .max(HYBRID_MIN_CANDIDATES);
        self.search_hybrid_with_depth(query, embedding, limit, depth)
    }

    /// [`Store::search_hybrid`] with an explicit fusion depth: each list (full-text, and vector
    /// when `embedding` is given) contributes its top `depth` hits (the vector list at most
    /// 4096), and the best `limit` fused hits are returned.
    ///
    /// Depth matters for quality, not just cost: with RRF, a chunk ranked moderately in *both*
    /// lists outscores one ranked first in only one of them, so the deeper the lists, the more
    /// such middling chunks can push a strong single-list match down. Callers that over-fetch
    /// (to group chunks by item, say) should keep `depth` tied to the number of results they
    /// will show and pass the larger number as `limit`.
    pub fn search_hybrid_with_depth(
        &self,
        query: &str,
        embedding: Option<&[f32]>,
        limit: usize,
        depth: usize,
    ) -> Result<Vec<Hit>> {
        if limit == 0 || depth == 0 {
            return Ok(Vec::new());
        }
        let text = self.search_text(query, depth)?;
        let vector = match embedding {
            Some(e) => self.search_vector(e, depth.min(MAX_KNN))?,
            None => Vec::new(),
        };
        Ok(rrf_fuse([text, vector], limit))
    }
}

/// Chunk leg of text search: `?1` an FTS5 query, `?2` the limit.
pub(crate) const TEXT_SEARCH_SQL: &str = "
    SELECT ch.id, ch.item_id, i.source, i.key, i.title,
           snippet(chunks_fts, 0, '', '', '…', 24), bm25(chunks_fts)
    FROM chunks_fts
    JOIN chunks ch ON ch.id = chunks_fts.rowid
    JOIN items i ON i.id = ch.item_id
    WHERE chunks_fts MATCH ?1
    ORDER BY bm25(chunks_fts), ch.id
    LIMIT ?2";

/// Title leg of text search: `?1` an FTS5 query, `?2` the limit.
pub(crate) const TITLE_SEARCH_SQL: &str = "
    SELECT rowid, bm25(items_fts) FROM items_fts
    WHERE items_fts MATCH ?1
    ORDER BY bm25(items_fts), rowid
    LIMIT ?2";

/// An item's first chunk (`?1` the item id), standing in for an item matched by title only.
pub(crate) const FIRST_CHUNK_SQL: &str = "
    SELECT ch.id, i.source, i.key, i.title, ch.text
    FROM chunks ch JOIN items i ON i.id = ch.item_id
    WHERE ch.item_id = ?1
    ORDER BY ch.ord
    LIMIT 1";

/// Chunk and title full-text legs, fused (see [`Store::search_text`]). `q` is an FTS5 query
/// from [`fts::match_expr`].
fn text_hits(c: &Connection, q: &str, limit: usize) -> Result<Vec<Hit>> {
    let lim = limit.min(i64::MAX as usize) as i64;
    let mut hits: Vec<(i64, Hit)> = {
        let mut stmt = c
            .prepare_cached(TEXT_SEARCH_SQL)
            .db("prepare text search")?;
        let rows = stmt
            .query_map(params![q, lim], |r| {
                let rank: f64 = r.get(6)?;
                Ok((
                    r.get::<_, i64>(1)?,
                    Hit {
                        chunk_id: r.get(0)?,
                        item: item_ref(r.get(2)?, r.get(3)?),
                        title: r.get(4)?,
                        snippet: r.get(5)?,
                        score: -rank as f32,
                    },
                ))
            })
            .db("text search")?;
        rows.collect::<rusqlite::Result<_>>().db("text search")?
    };

    // Title leg: item id → weighted title score.
    let titles: Vec<(i64, f64)> = {
        let mut stmt = c
            .prepare_cached(TITLE_SEARCH_SQL)
            .db("prepare title search")?;
        let rows = stmt
            .query_map(params![q, lim], |r| {
                Ok((r.get::<_, i64>(0)?, -r.get::<_, f64>(1)? * TITLE_WEIGHT))
            })
            .db("title search")?;
        rows.collect::<rusqlite::Result<_>>().db("title search")?
    };
    if titles.is_empty() {
        return Ok(hits.into_iter().map(|(_, h)| h).collect());
    }
    let title_score: HashMap<i64, f64> = titles.iter().copied().collect();
    let mut with_chunk_hits: HashSet<i64> = HashSet::new();
    for (item_id, hit) in &mut hits {
        if let Some(t) = title_score.get(item_id) {
            hit.score = (f64::from(hit.score) + t) as f32;
            with_chunk_hits.insert(*item_id);
        }
    }
    let mut first_chunk = c
        .prepare_cached(FIRST_CHUNK_SQL)
        .db("prepare first chunk lookup")?;
    for (item_id, t) in titles {
        if with_chunk_hits.contains(&item_id) {
            continue;
        }
        let hit = first_chunk
            .query_row([item_id], |r| {
                let text: String = r.get(4)?;
                Ok(Hit {
                    chunk_id: r.get(0)?,
                    item: item_ref(r.get(1)?, r.get(2)?),
                    title: r.get(3)?,
                    snippet: excerpt(&text),
                    score: t as f32,
                })
            })
            .optional()
            .db("first chunk lookup")?;
        hits.extend(hit.map(|h| (item_id, h)));
    }
    let mut out: Vec<Hit> = hits.into_iter().map(|(_, h)| h).collect();
    out.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.chunk_id.cmp(&b.chunk_id))
    });
    out.truncate(limit);
    Ok(out)
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
