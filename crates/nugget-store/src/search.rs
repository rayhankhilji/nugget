//! Full-text (FTS5 bm25), vector (`sqlite-vec` cosine kNN) and hybrid (RRF) search.

use std::collections::{HashMap, HashSet};

use nugget_core::Result;
use nugget_core::ids::ItemRef;
use rusqlite::{Connection, OptionalExtension, params};

use crate::Store;
use crate::error::DbExt;
use crate::fts;
use crate::items::item_ref;
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
pub(crate) const MAX_KNN: usize = 4096;
/// Snippet length for vector hits (characters).
const SNIPPET_CHARS: usize = 200;

pub(crate) fn excerpt(text: &str) -> String {
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
        let Some(parts) = fts::match_parts(query) else {
            return Ok(Vec::new());
        };
        if limit == 0 {
            return Ok(Vec::new());
        }
        let needles = fts::needles(query);
        self.with_read_conn(|c| text_hits(c, &parts, &needles, limit))
    }

    /// Which of `among` match the full-text `query` (in a chunk's text or the item's title, as
    /// [`Store::search_text`] matches), provided the query is rare: `None` when more than
    /// `at_most` chunks and title-only items match it.
    ///
    /// For "does this question's key word occur in these few results" checks. Unlike
    /// `search_text(query, at_most + 1)` it neither ranks the matches nor loads or snippets
    /// them, so its cost is the index lookup alone (~1% of a 500k-item store matched by a word
    /// costs ~40 µs per match through `search_text`).
    pub fn text_matches_among(
        &self,
        query: &str,
        at_most: usize,
        among: &[ItemRef],
    ) -> Result<Option<Vec<bool>>> {
        let Some(q) = fts::match_expr(query) else {
            return Ok(Some(vec![false; among.len()]));
        };
        let lim = at_most.saturating_add(1).min(i64::MAX as usize) as i64;
        self.with_read_conn(|c| {
            let mut matched: HashSet<i64> = HashSet::new();
            let mut n = 0usize;
            {
                let mut stmt = c
                    .prepare_cached(MATCHING_CHUNK_ITEMS_SQL)
                    .db("prepare matching chunks")?;
                let mut rows = stmt.query(params![q, lim]).db("matching chunks")?;
                while let Some(r) = rows.next().db("matching chunks")? {
                    n += 1;
                    matched.insert(r.get(0).db("matching chunks")?);
                }
            }
            if n > at_most {
                return Ok(None);
            }
            {
                let mut stmt = c
                    .prepare_cached(MATCHING_TITLES_SQL)
                    .db("prepare matching titles")?;
                let mut rows = stmt.query(params![q, lim]).db("matching titles")?;
                while let Some(r) = rows.next().db("matching titles")? {
                    if matched.insert(r.get(0).db("matching titles")?) {
                        n += 1;
                        if n > at_most {
                            return Ok(None);
                        }
                    }
                }
            }
            let mut id_of = c
                .prepare_cached("SELECT id FROM items WHERE source = ?1 AND key = ?2")
                .db("prepare item id")?;
            among
                .iter()
                .map(|r| {
                    let id: Option<i64> = id_of
                        .query_row(params![r.source.as_str(), r.key.as_str()], |row| row.get(0))
                        .optional()
                        .db("item id")?;
                    Ok(id.is_some_and(|id| matched.contains(&id)))
                })
                .collect::<Result<Vec<bool>>>()
                .map(Some)
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
///
/// Ranking happens on the FTS table alone and only the top `?2` rows are joined, so the cost of
/// the join (and of the snippet, taken separately by [`SNIPPET_SQL`]) does not grow with the
/// number of matching chunks. Computing `snippet()` in the ranked query made SQLite build a
/// snippet for every match before sorting: 415 ms p50 per query at 100k chunks.
pub(crate) const TEXT_SEARCH_SQL: &str = "
    WITH top AS (
        SELECT rowid AS id, bm25(chunks_fts) AS rank
        FROM chunks_fts
        WHERE chunks_fts MATCH ?1
        ORDER BY bm25(chunks_fts), rowid
        LIMIT ?2
    )
    SELECT ch.id, ch.item_id, i.source, i.key, i.title, top.rank
    FROM top
    JOIN chunks ch ON ch.id = top.id
    JOIN items i ON i.id = ch.item_id
    ORDER BY top.rank, ch.id";

/// Items of the chunks matching an FTS5 query `?1`, unranked, at most `?2` rows (one per chunk).
pub(crate) const MATCHING_CHUNK_ITEMS_SQL: &str = "
    SELECT ch.item_id
    FROM (SELECT rowid AS id FROM chunks_fts WHERE chunks_fts MATCH ?1 LIMIT ?2) m
    JOIN chunks ch ON ch.id = m.id";

/// Items whose title matches an FTS5 query `?1`, unranked, at most `?2`.
pub(crate) const MATCHING_TITLES_SQL: &str = "
    SELECT rowid FROM items_fts WHERE items_fts MATCH ?1 LIMIT ?2";

/// Snippet and text of one matching chunk: `?1` the FTS5 query, `?2` the chunk id.
pub(crate) const SNIPPET_SQL: &str = "
    SELECT snippet(chunks_fts, 0, '', '', '…', 24), text
    FROM chunks_fts
    WHERE chunks_fts MATCH ?1 AND rowid = ?2";

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

/// Characters of context kept before the match in [`segmented_snippet`].
const SNIPPET_LEAD_CHARS: usize = 40;

/// Snippet of a chunk holding CJK or Thai text. Such chunks are indexed segmented (see
/// `fts::segment`), so FTS5 token positions do not line up with the stored text and
/// `snippet()` would cut the wrong window: take the text around the first query word instead,
/// or its opening when none occurs verbatim.
fn segmented_snippet(text: &str, needles: &[String]) -> String {
    let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
    let chars: Vec<char> = text.chars().collect();
    let hay: Vec<char> = chars.iter().copied().map(fold).collect();
    let first = needles
        .iter()
        .filter_map(|n| {
            let n: Vec<char> = n.chars().map(fold).collect();
            if n.is_empty() {
                return None;
            }
            hay.windows(n.len()).position(|w| w == n.as_slice())
        })
        .min();
    let Some(pos) = first else {
        return excerpt(text);
    };
    let start = pos.saturating_sub(SNIPPET_LEAD_CHARS);
    let end = (start + SNIPPET_CHARS).min(chars.len());
    let body: String = chars[start..end].iter().collect();
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.push_str(body.trim());
    if end < chars.len() {
        out.push('…');
    }
    out
}

/// FTS5's bm25 `k1` (`b` = 0.75 does not enter the bound below).
const BM25_K1: f64 = 1.2;
/// A query term matching at least 1/this of all chunks is *common* (see [`pruned_ranks`]).
const COMMON_TERM_DIVISOR: i64 = 32;
/// Below this many rows every query ranks every match: it is fast anyway.
const PRUNE_MIN_ROWS: i64 = 20_000;
/// Times [`pruned_ranks`] moves a common term to the rare side before giving up.
const PRUNE_RETRIES: usize = 2;
/// Rows matching the rare terms, at most, for the pruned plan to be worth it.
const PRUNE_MAX_RARE_ROWS: i64 = 200_000;

/// A chunk's item, for a ranked chunk id.
const CHUNK_ITEM_SQL: &str = "
    SELECT ch.item_id, i.source, i.key, i.title
    FROM chunks ch JOIN items i ON i.id = ch.item_id
    WHERE ch.id = ?1";

/// The exact top `limit` rows of `index` for the OR-ed query `parts` by bm25 (`(rowid, rank)`,
/// best first, ties by rowid: what [`TEXT_SEARCH_SQL`] and [`TITLE_SEARCH_SQL`] rank), computed
/// without ranking every row that matches a very common term; or `None` when that cannot be
/// shown exact, or would not be cheaper (the caller then runs the plain query).
///
/// An OR query makes FTS5 score every row matching any term: a word in most chunks of a large
/// store means ranking most of the store (LL1: ~300 ms at 600k chunks), though such a word adds
/// almost nothing to bm25. So the terms are split by how many chunks match them into *rare*
/// `R` and *common* `C` ones (at least 1/[`COMMON_TERM_DIVISOR`] of all chunks), and:
///
/// - bm25 is a sum over the query's phrases, each with its own idf (from the whole table) and
///   frequency in the row. So for every row matching a rare term, the query `R` gives its
///   rare part `r` and the query `(R) AND (R OR C)` gives `2r + c` (the rare phrases appear
///   twice): its full score is the difference. FTS5 evaluates the `AND` by seeking in the
///   common terms' lists, not walking them.
/// - A row matching only common terms scores at most `Σ_C idf · (k1 + 1)` (bm25's limit as a
///   term's frequency grows). If the `limit`-th score found beats that bound, no such row can
///   be in the top `limit`: the answer is exact. Otherwise (or with fewer than `limit` rows
///   matching rare terms) the least common of the common terms moves to the rare side and
///   the check runs again, up to [`PRUNE_RETRIES`] times; then `None`.
fn pruned_ranks(
    c: &Connection,
    index: FtsIndex,
    parts: &fts::MatchParts,
    limit: usize,
) -> Result<Option<Vec<(i64, f64)>>> {
    pruned_ranks_from(c, index, parts, limit, PRUNE_MIN_ROWS)
}

/// [`pruned_ranks`] on an index of at least `min_rows` rows.
fn pruned_ranks_from(
    c: &Connection,
    index: FtsIndex,
    parts: &fts::MatchParts,
    limit: usize,
    min_rows: i64,
) -> Result<Option<Vec<(i64, f64)>>> {
    if parts.positive.len() < 2 || limit == 0 {
        return Ok(None);
    }
    let n: i64 = c
        .query_row(index.rows_sql(), [], |r| r.get(0))
        .db("count rows")?;
    if n < min_rows.max(1) {
        return Ok(None);
    }
    let common_at = n / COMMON_TERM_DIVISOR;
    let mut rare: Vec<&str> = Vec::new();
    let mut rare_rows = 0i64;
    // (term, rows matching it, its bound contribution)
    let mut common: Vec<(&str, i64, f64)> = Vec::new();
    {
        let mut count = c
            .prepare_cached(index.capped_count_sql())
            .db("prepare term count")?;
        for t in &parts.positive {
            let m: i64 = count
                .query_row(params![t, common_at], |r| r.get(0))
                .db("term count")?;
            if m < common_at {
                rare.push(t);
                rare_rows += m;
                continue;
            }
            // The exact count for the idf, which FTS5 floors at 1e-6: past half the rows the
            // count no longer matters.
            let m: i64 = count
                .query_row(params![t, n / 2 + 1], |r| r.get(0))
                .db("term count")?;
            let idf = (((n - m) as f64 + 0.5) / (m as f64 + 0.5)).ln().max(1e-6);
            common.push((t, m, idf * (BM25_K1 + 1.0)));
        }
    }
    // The least common first: those are moved to the rare side when the bound is not met.
    common.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(b.0)));
    let all: Vec<&str> = parts.positive.iter().map(String::as_str).collect();
    let mut stmt = c.prepare_cached(index.ranks_sql()).db("prepare ranks")?;
    for _ in 0..=PRUNE_RETRIES {
        if rare.is_empty() || common.is_empty() || rare_rows > PRUNE_MAX_RARE_ROWS {
            return Ok(None);
        }
        let bound: f64 = common.iter().map(|c| c.2).sum();
        let rare_rank: HashMap<i64, f64> = stmt
            .query_map(params![parts.expr_of(&rare)], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .db("rare ranks")?
            .collect::<rusqlite::Result<_>>()
            .db("rare ranks")?;
        let both = parts.with_exclusions(format!(
            "({}) AND ({})",
            rare.join(" OR "),
            all.join(" OR ")
        ));
        let mut ranked: Vec<(i64, f64)> = stmt
            .query_map(params![both], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
            })
            .db("full ranks")?
            .filter_map(|row| match row {
                // bm25() is negative: rank(full) = rank(2r + c) - rank(r).
                Ok((id, both)) => rare_rank.get(&id).map(|r| Ok((id, both - r))),
                Err(e) => Some(Err(e)),
            })
            .collect::<rusqlite::Result<_>>()
            .db("full ranks")?;
        if ranked.len() >= limit {
            ranked.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
            ranked.truncate(limit);
            if -ranked[limit - 1].1 >= bound {
                return Ok(Some(ranked));
            }
        }
        // Not provably exact: rank the least common of the common terms in full too.
        let (t, m, _) = common.remove(0);
        rare.push(t);
        rare_rows += m;
    }
    Ok(None)
}

/// The two full-text indexes [`pruned_ranks`] works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FtsIndex {
    /// `chunks_fts`: one row per chunk.
    Chunks,
    /// `items_fts`: one row per item (its title).
    Titles,
}

impl FtsIndex {
    /// Rows in the index (an exact count kept by triggers).
    fn rows_sql(self) -> &'static str {
        match self {
            FtsIndex::Chunks => "SELECT coalesce(sum(chunks), 0) FROM source_stats",
            FtsIndex::Titles => "SELECT coalesce(sum(items), 0) FROM source_stats",
        }
    }

    /// Rows matching `?1`, at most `?2`.
    fn capped_count_sql(self) -> &'static str {
        match self {
            FtsIndex::Chunks => {
                "SELECT count(*) FROM (SELECT 1 FROM chunks_fts WHERE chunks_fts MATCH ?1 LIMIT ?2)"
            }
            FtsIndex::Titles => {
                "SELECT count(*) FROM (SELECT 1 FROM items_fts WHERE items_fts MATCH ?1 LIMIT ?2)"
            }
        }
    }

    /// Every row matching `?1` with its bm25 rank, unordered.
    fn ranks_sql(self) -> &'static str {
        match self {
            FtsIndex::Chunks => {
                "SELECT rowid, bm25(chunks_fts) FROM chunks_fts WHERE chunks_fts MATCH ?1"
            }
            FtsIndex::Titles => {
                "SELECT rowid, bm25(items_fts) FROM items_fts WHERE items_fts MATCH ?1"
            }
        }
    }
}

/// Chunk and title full-text legs, fused (see [`Store::search_text`]). `parts` is the query
/// from [`fts::match_parts`] and `needles` its words (from [`fts::needles`]).
fn text_hits(
    c: &Connection,
    parts: &fts::MatchParts,
    needles: &[String],
    limit: usize,
) -> Result<Vec<Hit>> {
    let q = &parts.expr();
    let lim = limit.min(i64::MAX as usize) as i64;
    let pruned = pruned_ranks(c, FtsIndex::Chunks, parts, limit)?;
    let mut hits: Vec<(i64, Hit)> = if let Some(ranked) = pruned {
        let mut item_of = c.prepare_cached(CHUNK_ITEM_SQL).db("prepare chunk item")?;
        let mut out = Vec::with_capacity(ranked.len());
        for (id, rank) in ranked {
            let row = item_of
                .query_row([id], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        Hit {
                            chunk_id: id,
                            item: item_ref(r.get(1)?, r.get(2)?),
                            title: r.get(3)?,
                            snippet: String::new(),
                            score: -rank as f32,
                        },
                    ))
                })
                .optional()
                .db("chunk item")?;
            out.extend(row);
        }
        out
    } else {
        let mut stmt = c
            .prepare_cached(TEXT_SEARCH_SQL)
            .db("prepare text search")?;
        let rows = stmt
            .query_map(params![q, lim], |r| {
                let rank: f64 = r.get(5)?;
                Ok((
                    r.get::<_, i64>(1)?,
                    Hit {
                        chunk_id: r.get(0)?,
                        item: item_ref(r.get(2)?, r.get(3)?),
                        title: r.get(4)?,
                        snippet: String::new(),
                        score: -rank as f32,
                    },
                ))
            })
            .db("text search")?;
        rows.collect::<rusqlite::Result<_>>().db("text search")?
    };
    if !hits.is_empty() {
        let mut snip = c.prepare_cached(SNIPPET_SQL).db("prepare snippet")?;
        for (_, hit) in &mut hits {
            let row: Option<(String, String)> = snip
                .query_row(params![q, hit.chunk_id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()
                .db("snippet")?;
            hit.snippet = match row {
                Some((_, text)) if fts::has_segmented(&text) => segmented_snippet(&text, needles),
                Some((snippet, _)) => snippet,
                None => String::new(),
            };
        }
    }

    // Title leg: item id → weighted title score.
    let titles: Vec<(i64, f64)> =
        if let Some(ranked) = pruned_ranks(c, FtsIndex::Titles, parts, limit)? {
            ranked
                .into_iter()
                .map(|(id, rank)| (id, -rank * TITLE_WEIGHT))
                .collect()
        } else {
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

    /// The pruned plan returns exactly what the plain query ranks (on a store small enough for
    /// a test, with the size floor lifted), for chunk and title indexes, with phrases, prefixes
    /// and exclusions, whether or not it applies.
    #[test]
    fn pruned_ranking_matches_the_plain_query() {
        use crate::{NewChunk, NewItem, Store, content_hash};

        let store = Store::open_in_memory().unwrap();
        // Zipf-like words: `w0` is in nearly every chunk, `w1` in half, ...
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let mut batch = Vec::new();
        for i in 0..1500u64 {
            let words: Vec<String> = (0..(8 + next() % 40))
                .map(|_| {
                    let r = (next() % 10_000) as f64 / 10_000.0;
                    format!("w{}", (r.powi(3) * 400.0) as u64)
                })
                .collect();
            let text = words.join(" ");
            let title = format!("t{} w{}", i % 50, next() % 30);
            let item = NewItem::new("src", format!("k{i}"), content_hash(&text)).with_title(title);
            batch.push((item, vec![NewChunk::text(text)]));
            if batch.len() == 250 {
                store.upsert_items_with_chunks(&batch).unwrap();
                batch.clear();
            }
        }
        let queries = [
            "w0 w37",
            "w0 w1 w150",
            "w0 w1 w2 w3",
            "w2 w90 w200 w0",
            "w0 w5 -w7",
            "\"w0 w1\" w60",
            "w1* w250",
            "w0 w399",
            "w0 w1",
            "t3 w0 w1",
            "w0 t7",
            "w0 w300",
            "w0 w1 w350",
            "w0 w2 w380",
            "w1 w320 w330",
            "w0 w1 w2 w310 w311",
            "w0 w4 w280 -w9",
        ];
        let mut applied = 0;
        store
            .with_read_conn(|c| {
                for q in queries {
                    let parts = fts::match_parts(q).unwrap();
                    let expr = parts.expr();
                    for (index, sql) in [
                        (FtsIndex::Chunks, "SELECT rowid, bm25(chunks_fts) FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY bm25(chunks_fts), rowid LIMIT ?2"),
                        (FtsIndex::Titles, TITLE_SEARCH_SQL),
                    ] {
                        for limit in [1usize, 10, 40] {
                            let plain: Vec<(i64, f64)> = c
                                .prepare(sql)
                                .unwrap()
                                .query_map(params![expr, limit as i64], |r| {
                                    Ok((r.get(0)?, r.get(1)?))
                                })
                                .unwrap()
                                .collect::<rusqlite::Result<_>>()
                                .unwrap();
                            let Some(pruned) =
                                pruned_ranks_from(c, index, &parts, limit, 0).unwrap()
                            else {
                                continue;
                            };
                            applied += 1;
                            let ids = |v: &[(i64, f64)]| v.iter().map(|x| x.0).collect::<Vec<_>>();
                            assert_eq!(ids(&pruned), ids(&plain), "{q:?} {index:?} {limit}");
                            for (a, b) in pruned.iter().zip(&plain) {
                                assert!((a.1 - b.1).abs() < 1e-9, "{q:?}: {a:?} vs {b:?}");
                            }
                        }
                    }
                }
                Ok(())
            })
            .unwrap();
        assert!(
            applied >= 10,
            "the pruned plan applied only {applied} times"
        );
        // Below the size floor it never applies.
        store
            .with_read_conn(|c| {
                let parts = fts::match_parts("w0 w37").unwrap();
                assert!(pruned_ranks(c, FtsIndex::Chunks, &parts, 10)?.is_none());
                Ok(())
            })
            .unwrap();
        // And search_text gives the same hits as before.
        assert!(!store.search_text("w0 w37", 10).unwrap().is_empty());
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
