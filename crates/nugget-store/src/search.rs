//! Full-text (FTS5 bm25), vector (`sqlite-vec` cosine kNN) and hybrid (RRF) search.

use std::collections::{HashMap, HashSet};

use nugget_core::Result;
use nugget_core::ids::ItemRef;
use rusqlite::{Connection, OptionalExtension, params};
use unicode_normalization::char::{decompose_compatible, is_combining_mark};

use crate::Store;
use crate::error::{ConnExt, DbExt};
use crate::fts;
use crate::items::item_ref;
use crate::types::{ChunkId, Hit};

/// Weight of an item's title bm25 score, added to the bm25 score of each of its matching chunks in
/// text search.
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
/// Text search also matches query words as parts of longer words (see `fts::fallback_exprs`)
/// when its chunk leg finds fewer hits than this (or than the limit)...
const FALLBACK_BELOW: usize = 3;
/// ...and weights the bm25 score of such matches by this.
const FALLBACK_WEIGHT: f64 = 0.5;

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
    pub fn search_text(&self, query: &str, limit: usize) -> Result<Vec<Hit>> {
        let Some(parts) = fts::match_parts(query) else {
            return Ok(Vec::new());
        };
        if limit == 0 {
            return Ok(Vec::new());
        }
        let needles = fts::needles(query);
        let fallback = fts::fallback_exprs(query);
        self.with_read_conn(|c| text_hits(c, &parts, fallback.as_ref(), &needles, limit))
    }

    /// Which of `among` match the full-text `query` (in a chunk's text or the item's title, as
    /// [`Store::search_text`] matches), provided the query is rare: `None` when more than `at_most`
    /// chunks and title-only items match it.
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
    /// reciprocal-rank fusion, `score = Σ 1 / (60 + rank)`.
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

/// Word-part fallback of text search, words starting with a query word: `?1` an FTS5 query
/// (`fts::Fallback::prefix`), `?2` the limit. Returns [`TEXT_SEARCH_SQL`]'s columns and the text.
pub(crate) const PREFIX_SEARCH_SQL: &str = "
    WITH top AS (
        SELECT rowid AS id, bm25(chunks_fts) AS rank
        FROM chunks_fts
        WHERE chunks_fts MATCH ?1
        ORDER BY bm25(chunks_fts), rowid
        LIMIT ?2
    )
    SELECT ch.id, ch.item_id, i.source, i.key, i.title, top.rank, ch.text
    FROM top
    JOIN chunks ch ON ch.id = top.id
    JOIN items i ON i.id = ch.item_id
    ORDER BY top.rank, ch.id";

/// [`PREFIX_SEARCH_SQL`] for words ending with a query word, in `chunks_rev`
/// (`fts::Fallback::suffix`).
pub(crate) const SUFFIX_SEARCH_SQL: &str = "
    WITH top AS (
        SELECT rowid AS id, bm25(chunks_rev) AS rank
        FROM chunks_rev
        WHERE chunks_rev MATCH ?1
        ORDER BY bm25(chunks_rev), rowid
        LIMIT ?2
    )
    SELECT ch.id, ch.item_id, i.source, i.key, i.title, top.rank, ch.text
    FROM top
    JOIN chunks ch ON ch.id = top.id
    JOIN items i ON i.id = ch.item_id
    ORDER BY top.rank, ch.id";

/// Whether chunk `?2` matches the FTS5 query `?1` (a fallback hit's exclusions).
pub(crate) const CHUNK_MATCHES_SQL: &str = "
    SELECT EXISTS (SELECT 1 FROM chunks_fts WHERE chunks_fts MATCH ?1 AND rowid = ?2)";

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

/// Characters of context kept before the match in [`located_snippet`].
const SNIPPET_LEAD_CHARS: usize = 40;

/// Append `c` as [`located_snippet`] compares it: compatibility-decomposed without marks,
/// lowercased, with the Arabic and German spellings `fts::fold` unifies.
fn loose_chars(c: char, out: &mut impl FnMut(char)) {
    decompose_compatible(c, |d| {
        if is_combining_mark(d) || d == '\u{0640}' {
            return;
        }
        for l in d.to_lowercase() {
            match l {
                'ة' => out('ه'),
                'ى' => out('ي'),
                'ß' => {
                    out('s');
                    out('s');
                }
                l => out(l),
            }
        }
    });
}

/// Snippet of a chunk whose index entry is not its text (see `fts::is_rewritten`: CJK or Thai
/// bigrams, folded spellings, variant forms) or that matched as word parts.
fn located_snippet(text: &str, needles: &[String]) -> String {
    let chars: Vec<char> = text.chars().collect();
    // Loose characters, each with the index of the character it came from.
    let mut hay: Vec<(char, usize)> = Vec::with_capacity(chars.len());
    for (i, &c) in chars.iter().enumerate() {
        loose_chars(c, &mut |l| hay.push((l, i)));
    }
    let find = |n: &[char]| {
        hay.windows(n.len())
            .position(|w| w.iter().map(|(c, _)| c).eq(n.iter()))
            .map(|p| hay[p].1)
    };
    let first = needles
        .iter()
        .filter_map(|n| {
            let mut loose = Vec::new();
            for c in n.chars() {
                loose_chars(c, &mut |l| loose.push(l));
            }
            if loose.is_empty() {
                return None;
            }
            // German ae/oe/ue spellings of ä/ö/ü (folded to a/o/u in the text).
            let plain: String = loose.iter().collect();
            let umlauts: Vec<char> = plain
                .replace("ae", "a")
                .replace("oe", "o")
                .replace("ue", "u")
                .chars()
                .collect();
            find(&loose).or_else(|| find(&umlauts))
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

/// The exact top `limit` rows of `index` for the OR-ed query `parts` by bm25 (`(rowid, rank)`, best
/// first, ties by rowid: what [`TEXT_SEARCH_SQL`] and [`TITLE_SEARCH_SQL`] rank), computed without
/// ranking every row that matches a very common term; or `None` when that cannot be shown exact, or
/// would not be cheaper (the caller then runs the plain query).
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

/// One row of [`TEXT_SEARCH_SQL`] (or the fallback statements, whose text is column 6) as
/// `(item id, hit without snippet, bm25 rank)`.
fn text_hit_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, Hit, f64)> {
    let rank: f64 = r.get(5)?;
    Ok((
        r.get(1)?,
        Hit {
            chunk_id: r.get(0)?,
            item: item_ref(r.get(2)?, r.get(3)?),
            title: r.get(4)?,
            snippet: String::new(),
            score: -rank as f32,
        },
        rank,
    ))
}

/// Chunk and title full-text legs, fused (see [`Store::search_text`]).
fn text_hits(
    c: &Connection,
    parts: &fts::MatchParts,
    fallback: Option<&fts::Fallback>,
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
        c.rows(
            TEXT_SEARCH_SQL,
            params![q, lim],
            |r| text_hit_row(r).map(|(i, h, _)| (i, h)),
            "text search",
        )?
    };
    if !hits.is_empty() {
        let mut snip = c.prepare_cached(SNIPPET_SQL).db("prepare snippet")?;
        for (_, hit) in &mut hits {
            let row: Option<(String, String)> = snip
                .query_row(params![q, hit.chunk_id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()
                .db("snippet")?;
            hit.snippet = match row {
                Some((_, text)) if fts::is_rewritten(&text) => located_snippet(&text, needles),
                Some((snippet, _)) => snippet,
                None => String::new(),
            };
        }
    }

    if let Some(fb) = fallback
        && hits.len() < limit.min(FALLBACK_BELOW)
    {
        let mut seen: HashSet<ChunkId> = hits.iter().map(|(_, h)| h.chunk_id).collect();
        let mut excluded = c
            .prepare_cached(CHUNK_MATCHES_SQL)
            .db("prepare exclusion check")?;
        for (sql, expr, exclude) in [
            (PREFIX_SEARCH_SQL, &fb.prefix, None),
            (SUFFIX_SEARCH_SQL, &fb.suffix, fb.exclude.as_ref()),
        ] {
            let mut stmt = c.prepare_cached(sql).db("prepare word-part search")?;
            let rows: Vec<((i64, Hit, f64), String)> = stmt
                .query_map(params![expr, lim], |r| Ok((text_hit_row(r)?, r.get(6)?)))
                .db("word-part search")?
                .collect::<rusqlite::Result<_>>()
                .db("word-part search")?;
            for ((item_id, mut hit, rank), text) in rows {
                if !seen.insert(hit.chunk_id) {
                    continue;
                }
                if let Some(n) = exclude
                    && excluded
                        .query_row(params![n, hit.chunk_id], |r| r.get::<_, bool>(0))
                        .db("exclusion check")?
                {
                    continue;
                }
                hit.score = (-rank * FALLBACK_WEIGHT) as f32;
                hit.snippet = located_snippet(&text, needles);
                hits.push((item_id, hit));
            }
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
            c.rows(
                TITLE_SEARCH_SQL,
                params![q, lim],
                |r| Ok((r.get::<_, i64>(0)?, -r.get::<_, f64>(1)? * TITLE_WEIGHT)),
                "title search",
            )?
        };
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

/// Reciprocal-rank fusion of ranked lists (each best-first).
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
        store
            .with_read_conn(|c| {
                let parts = fts::match_parts("w0 w37").unwrap();
                assert!(pruned_ranks(c, FtsIndex::Chunks, &parts, 10)?.is_none());
                Ok(())
            })
            .unwrap();
        assert!(!store.search_text("w0 w37", 10).unwrap().is_empty());
    }

    #[test]
    fn rrf_rewards_agreement() {
        let fused = rrf_fuse([vec![hit(1), hit(2), hit(3)], vec![hit(4), hit(2)]], 10);
        let ids: Vec<_> = fused.iter().map(|h| h.chunk_id).collect();
        assert_eq!(ids[0], 2, "found by both lists");
        assert_eq!(ids.len(), 4);
        assert_eq!(&ids[1..3], &[1, 4]);
        assert!((fused[0].score - (1.0 / 62.0 + 1.0 / 62.0) as f32).abs() < 1e-6);
        assert_eq!(rrf_fuse([vec![hit(1), hit(2)]], 1).len(), 1);
    }
}
