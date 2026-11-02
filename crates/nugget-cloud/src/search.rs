//! Hybrid search over a user's items: Postgres full text and pgvector, fused, grouped per item,
//! recency-boosted and diversified with `nugget-retrieve`'s ranking; natural-language dates
//! ("last week") with `nugget_retrieve::parse_date_query`. Facets (source, person) and a timeline
//! count the items that match the words and filters (every filtered item when there are no
//! words); vector-only matches have no cut-off, so they rank results but are not counted.

use std::collections::HashMap;

use nugget_retrieve::rank::{
    convex_fuse, group_by_item, jaccard, mmr_select, normalise, recency_factor, token_set,
};
use nugget_retrieve::{RecencyBoost, parse_date_query};
use nugget_store::Hit;
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, UtcOffset};
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use crate::db::{Db, DbError, ItemMeta, tsquery};
use crate::gateway::{Allowance, Gateway, GatewayError};
use crate::plans::ModelTier;

const CANDIDATES: i64 = 200;
pub const MAX_LIMIT: usize = 50;
const MMR_LAMBDA: f32 = 0.7;
const SNIPPET_CHARS: usize = 280;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchRequest {
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub sources: Vec<String>,
    /// A name or address, matched against sender, recipients and attendees.
    pub person: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub from: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub to: Option<OffsetDateTime>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchResult {
    pub item_id: Uuid,
    pub source: String,
    pub kind: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub url: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub occurred_at: Option<OffsetDateTime>,
    pub snippet: String,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Count {
    pub value: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Bucket {
    #[serde(with = "time::serde::rfc3339")]
    pub start: OffsetDateTime,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Facets {
    pub sources: Vec<Count>,
    pub people: Vec<Count>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Timeline {
    /// `day` or `month`.
    pub bucket: &'static str,
    pub points: Vec<Bucket>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DateUsed {
    #[serde(with = "time::serde::rfc3339::option")]
    pub from: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub to: Option<OffsetDateTime>,
    /// The phrase read from the query ("last week"), when the range came from it.
    pub phrase: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchResponse {
    /// `hybrid`, `text` (no embeddings) or `browse` (no words: newest first).
    pub mode: &'static str,
    /// The words searched for, after any date phrase was taken out.
    pub query: String,
    pub dates: DateUsed,
    pub results: Vec<SearchResult>,
    /// Items matching the words and filters.
    pub total: i64,
    pub facets: Facets,
    pub timeline: Timeline,
}

/// The filters as SQL parameters, shared by every query (`$1` is the user).
struct Filters {
    sources: Option<Vec<String>>,
    from: Option<OffsetDateTime>,
    to: Option<OffsetDateTime>,
    person: Option<String>,
}

/// SQL over `i` (items) for the filters in `$2..$5`.
const FILTER_SQL: &str = "($2::text[] is null or i.source = any($2))
   and ($3::timestamptz is null or i.occurred_at >= $3)
   and ($4::timestamptz is null or i.occurred_at < $4)
   and ($5::text is null or i.author ilike $5 or i.metadata->>'to' ilike $5
        or i.metadata->>'cc' ilike $5 or (i.metadata->'attendees')::text ilike $5)";

fn like_pattern(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{}%", escaped.trim())
}

impl Filters {
    fn params<'a>(&'a self, user: &'a Uuid) -> Vec<&'a (dyn ToSql + Sync)> {
        vec![user, &self.sources, &self.from, &self.to, &self.person]
    }
}

fn hits(rows: &[tokio_postgres::Row]) -> Vec<Hit> {
    use nugget_core::ids::{ItemKey, ItemRef, SourceId};
    rows.iter()
        .map(|r| Hit {
            item: ItemRef {
                source: SourceId::new(r.get::<_, String>("source")),
                key: ItemKey::new(r.get::<_, Uuid>("item_id").to_string()),
            },
            chunk_id: r.get("id"),
            title: r.get("title"),
            snippet: r.get("text"),
            score: r.get::<_, f32>("score"),
        })
        .collect()
}

fn snippet(text: &str) -> String {
    let t: String = text.chars().take(SNIPPET_CHARS).collect();
    if t.len() < text.len() {
        format!("{}…", t.trim_end())
    } else {
        t
    }
}

fn meta_result(m: &ItemMeta, snippet: String, score: f32) -> SearchResult {
    SearchResult {
        item_id: m.id,
        source: m.source.clone(),
        kind: m.kind.clone(),
        title: m.title.clone(),
        author: crate::authors::display_name(m.author.as_deref()),
        url: m.url.clone(),
        occurred_at: m.occurred_at,
        snippet,
        score,
    }
}

pub async fn search(
    db: &Db,
    gateway: &Gateway,
    user: Uuid,
    req: &SearchRequest,
    allowance: Allowance,
    offset: UtcOffset,
    now: OffsetDateTime,
) -> Result<SearchResponse, GatewayError> {
    let limit = req.limit.unwrap_or(20).clamp(1, MAX_LIMIT);
    let mut words = req.q.trim().to_owned();
    let mut dates = DateUsed {
        from: req.from,
        to: req.to,
        phrase: None,
    };
    if dates.from.is_none()
        && dates.to.is_none()
        && let Some(d) = parse_date_query(&words, now, offset)
    {
        dates = DateUsed {
            from: Some(d.range.start),
            to: Some(d.range.end),
            phrase: Some(d.phrase),
        };
        words = d.cleaned;
    }
    let sources: Vec<String> = req
        .sources
        .iter()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let f = Filters {
        sources: (!sources.is_empty()).then_some(sources),
        from: dates.from,
        to: dates.to,
        person: req
            .person
            .as_deref()
            .filter(|p| !p.trim().is_empty())
            .map(like_pattern),
    };
    let tsq = tsquery(&words);

    // Lexical hits, the facet counts and the timeline in one scoped transaction.
    let day_buckets =
        matches!((dates.from, dates.to), (Some(a), Some(b)) if b - a <= time::Duration::days(62));
    let bucket = if day_buckets { "day" } else { "month" };
    let (text, total, facets, timeline) = db
        .scoped(Some(user), async |tx| {
            let mut params = f.params(&user);
            params.push(&tsq);
            // `m`: the items that match (by words when there are any, else every filtered item).
            let matched = format!(
                "select distinct i.id, i.source, i.author, i.occurred_at from public.items i
                 left join public.chunks c on c.item_id = i.id and $6::text is not null
                 where i.user_id = $1 and {FILTER_SQL}
                   and ($6::text is null or c.tsv @@ to_tsquery('simple', $6))"
            );
            let text = if tsq.is_some() {
                let sql = format!(
                    "select c.id, c.item_id, c.text, i.source, i.title, ts_rank_cd(c.tsv, to_tsquery('simple', $6))::real as score
                     from public.chunks c join public.items i on i.id = c.item_id
                     where c.user_id = $1 and c.tsv @@ to_tsquery('simple', $6) and {FILTER_SQL}
                     order by score desc, c.id limit {CANDIDATES}"
                );
                hits(&tx.query(&sql, &params).await?)
            } else {
                Vec::new()
            };
            let total: i64 = tx
                .query_one(&format!("select count(*) from ({matched}) m"), &params)
                .await?
                .get(0);
            let counts = async |col: &str, n: i64| -> Result<Vec<Count>, DbError> {
                let sql = format!(
                    "select {col} as v, count(*) from ({matched}) m where {col} is not null
                     group by v order by count(*) desc, v limit {n}"
                );
                Ok(tx
                    .query(&sql, &params)
                    .await?
                    .iter()
                    .map(|r| Count {
                        value: r.get(0),
                        count: r.get(1),
                    })
                    .collect())
            };
            let facets = Facets {
                sources: counts("m.source", 20).await?,
                people: counts("m.author", 10).await?,
            };
            let sql = format!(
                "select date_trunc('{bucket}', m.occurred_at) as b, count(*) from ({matched}) m
                 where m.occurred_at is not null group by b order by b"
            );
            let points = tx
                .query(&sql, &params)
                .await?
                .iter()
                .map(|r| Bucket {
                    start: r.get(0),
                    count: r.get(1),
                })
                .collect();
            Ok((
                text,
                total,
                facets,
                Timeline { bucket, points },
            ))
        })
        .await?;

    // Browse: no words, newest first.
    if tsq.is_none() && words.trim().is_empty() {
        let rows = db
            .scoped(Some(user), async |tx| {
                let sql = format!(
                    "select i.id, i.source, i.kind, i.title, i.author, i.url, i.occurred_at, left(i.body, 400) as body
                     from public.items i where i.user_id = $1 and {FILTER_SQL}
                     order by i.occurred_at desc nulls last, i.id limit {limit}"
                );
                Ok(tx.query(&sql, &f.params(&user)).await?)
            })
            .await?;
        let results = rows
            .iter()
            .map(|r| SearchResult {
                item_id: r.get("id"),
                source: r.get("source"),
                kind: r.get("kind"),
                title: r.get("title"),
                author: crate::authors::display_name(
                    r.get::<_, Option<String>>("author").as_deref(),
                ),
                url: r.get("url"),
                occurred_at: r.get("occurred_at"),
                snippet: snippet(&r.get::<_, String>("body")),
                score: 0.0,
            })
            .collect();
        return Ok(SearchResponse {
            mode: "browse",
            query: words,
            dates,
            results,
            total,
            facets,
            timeline,
        });
    }

    // The query embedding is metered: past the plan's hard cap, search is full text only.
    let embed_ok = match gateway
        .choose(db, user, allowance, Some(ModelTier::Fast))
        .await
    {
        Ok(_) => true,
        Err(GatewayError::QuotaExceeded) => false,
        Err(e) => return Err(e),
    };
    let embedded = if embed_ok {
        gateway.embed(db, user, &[words.clone()]).await?
    } else {
        None
    };
    let vector = match embedded {
        Some(mut v) if !v.is_empty() => {
            let emb = crate::db::vector_literal(&v.remove(0));
            db.scoped(Some(user), async |tx| {
                crate::db::widen_vector_scan(tx, CANDIDATES).await?;
                let mut params = f.params(&user);
                params.push(&emb);
                let sql = format!(
                    "select c.id, c.item_id, c.text, i.source, i.title,
                            (1 - (c.embedding operator(extensions.<=>) $6::text::extensions.vector))::real as score
                     from public.chunks c join public.items i on i.id = c.item_id
                     where c.user_id = $1 and c.embedding is not null and {FILTER_SQL}
                     order by c.embedding operator(extensions.<=>) $6::text::extensions.vector limit {CANDIDATES}"
                );
                Ok(hits(&tx.query(&sql, &params).await?))
            })
            .await?
        }
        _ => Vec::new(),
    };
    let mode = if gateway.has_embeddings() && embed_ok {
        "hybrid"
    } else {
        "text"
    };
    let weight = if vector.is_empty() { 1.0 } else { 0.5 };
    let mut cands = group_by_item(convex_fuse(text, vector, weight, CANDIDATES as usize), 2);
    normalise(&mut cands);
    let ids: Vec<Uuid> = cands
        .iter()
        .filter_map(|c| c.item.key.as_str().parse().ok())
        .collect();
    let metas: HashMap<Uuid, ItemMeta> = db.items_meta(user, &ids).await?;
    let boost = RecencyBoost::default();
    let mut ranked: Vec<(ItemMeta, String, f32)> = cands
        .into_iter()
        .filter_map(|c| {
            let m = metas.get(&c.item.key.as_str().parse().ok()?)?.clone();
            let age = m
                .occurred_at
                .map_or(f64::INFINITY, |t| (now - t).as_seconds_f64() / 86_400.0);
            let rel = c.rel * recency_factor(age, &boost);
            Some((m, c.snippet, rel))
        })
        .collect();
    ranked.sort_by(|a, b| b.2.total_cmp(&a.2));
    let tokens: Vec<_> = ranked.iter().map(|(_, s, _)| token_set(s)).collect();
    let rel: Vec<f32> = ranked.iter().map(|r| r.2).collect();
    let picked = mmr_select(&rel, limit, MMR_LAMBDA, &|i, j| {
        jaccard(&tokens[i], &tokens[j])
    });
    let results = picked
        .into_iter()
        .map(|i| {
            let (m, s, r) = &ranked[i];
            meta_result(m, snippet(s), *r)
        })
        .collect();
    Ok(SearchResponse {
        mode,
        query: words,
        dates,
        results,
        total,
        facets,
        timeline,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vlit(x0: f32, x1: f32, jitter: f32) -> String {
        let mut v = vec![0f32; 1536];
        v[0] = x0;
        v[1] = x1;
        v[2] = jitter;
        crate::db::vector_literal(&v)
    }

    /// SEC1 finding 31: the scan is widened per query, and another user's nearer chunks never
    /// crowd out this user's. (At this size the planner does not pick HNSW, so the crowding itself
    /// shows only on a large table; the settings are what change it there.)
    #[tokio::test]
    async fn hnsw_scan_still_finds_the_users_own_chunks() {
        let Some((db, pool)) = crate::testkit::test_db().await else {
            return;
        };
        let (a, b) = (
            crate::testkit::add_user(&pool).await,
            crate::testkit::add_user(&pool).await,
        );
        for (user, n, x0, x1) in [(b, 200, 1.0f32, 0.0f32), (a, 3, 0.5, 0.5)] {
            let item: Uuid = pool
                .query_one(
                    "insert into public.items (user_id, source, external_id, kind, body, content_hash)
                     values ($1, 'gmail', 'x', 'email', '', 'h') returning id",
                    &[&user],
                )
                .await
                .unwrap()
                .get(0);
            for ord in 0..n {
                let v = vlit(x0, x1, ord as f32 / 1000.0);
                pool.execute(
                    "insert into public.chunks (user_id, item_id, ord, text, embedding)
                     values ($1, $2, $3, 'c', $4::text::extensions.vector)",
                    &[&user, &item, &ord, &v],
                )
                .await
                .unwrap();
            }
        }
        let q = {
            let mut v = vec![0f32; 1536];
            v[0] = 1.0;
            v
        };
        // The scan settings apply to the query's transaction only.
        let (ef, iterative): (String, Option<String>) = db
            .scoped(Some(a), async |tx| {
                crate::db::widen_vector_scan(tx, 40).await?;
                let r = tx
                    .query_one(
                        "select current_setting('hnsw.ef_search'), current_setting('hnsw.iterative_scan', true)",
                        &[],
                    )
                    .await?;
                Ok((r.get(0), r.get(1)))
            })
            .await
            .unwrap();
        assert_eq!(ef, "400");
        let pgvector: String = pool
            .query_one(
                "select extversion from pg_extension where extname = 'vector'",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        if iterative.is_some() {
            assert_eq!(
                iterative.as_deref(),
                Some("relaxed_order"),
                "pgvector {pgvector}"
            );
        }
        let hits = db.search_vector(a, &q, 40).await.unwrap();
        assert_eq!(
            hits.len(),
            3,
            "all of the user's own chunks, none of the other's"
        );
    }

    #[test]
    fn like_patterns_are_escaped() {
        assert_eq!(like_pattern(" a_b%c "), "%a\\_b\\%c%");
    }

    #[test]
    fn snippets_are_cut() {
        assert_eq!(snippet("short"), "short");
        assert!(snippet(&"x ".repeat(400)).ends_with('…'));
    }
}
