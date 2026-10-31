//! The item change feed (`items_changed_since`, with deletion tombstones) and cheap per-item
//! dates, so consumers such as knowledge extraction never have to diff whole sources.
//!
//! The feed is the `item_changes` table (schema v5), kept by triggers on `items`: every insert,
//! update and delete of an item gives its key a new, strictly increasing seq (one row per key, so
//! the feed is compact). Writes that do not touch the item row (re-chunking alone) are not
//! changes.

use std::collections::HashMap;

use nugget_core::Result;
use nugget_core::ids::ItemRef;
use rusqlite::{OptionalExtension, params};

use crate::Store;
use crate::error::{DbExt, parse_time};
use crate::items::{LOOKUP_BATCH, WANTED_ITEMS_JOIN, item_ref, refs_json};
use crate::types::{ItemChange, ItemDates};

fn sql_limit(limit: usize) -> i64 {
    if limit == 0 {
        -1
    } else {
        i64::try_from(limit).unwrap_or(i64::MAX)
    }
}

impl Store {
    /// Items inserted, updated or deleted after `cursor`, oldest change first, at most `limit`
    /// (0 = unlimited), and the cursor to pass next time (the last seq returned, or `cursor` when
    /// nothing changed). Each item appears once, at its latest change; `deleted` marks a
    /// tombstone (the item is gone). Start from 0 to list every item ever stored and every
    /// deletion since schema v5.
    pub fn items_changed_since(&self, cursor: u64, limit: usize) -> Result<(Vec<ItemChange>, u64)> {
        let from = i64::try_from(cursor).unwrap_or(i64::MAX);
        self.with_read_conn(|c| {
            let mut stmt = c
                .prepare_cached(
                    "SELECT ch.source, ch.key, ch.seq, ch.deleted, i.version
                     FROM item_changes ch
                     LEFT JOIN items i ON i.source = ch.source AND i.key = ch.key
                     WHERE ch.seq > ?1 ORDER BY ch.seq LIMIT ?2",
                )
                .db("prepare items_changed_since")?;
            let rows = stmt
                .query_map(params![from, sql_limit(limit)], |r| {
                    Ok(ItemChange {
                        item: item_ref(r.get(0)?, r.get(1)?),
                        seq: r.get::<_, i64>(2)? as u64,
                        deleted: r.get(3)?,
                        version: r.get(4)?,
                    })
                })
                .db("query items_changed_since")?;
            let changes: Vec<ItemChange> = rows
                .collect::<rusqlite::Result<_>>()
                .db("read items_changed_since")?;
            let next = changes.last().map_or(cursor, |c| c.seq);
            Ok((changes, next))
        })
    }

    /// The seq of the latest item change (0 when nothing was ever stored): a cursor that skips
    /// everything already in the store.
    pub fn item_changes_head(&self) -> Result<u64> {
        self.with_read_conn(|c| {
            let head: Option<i64> = c
                .query_row("SELECT max(seq) FROM item_changes", [], |r| r.get(0))
                .optional()
                .db("read item change head")?
                .flatten();
            Ok(head.unwrap_or(0) as u64)
        })
    }

    /// The dates of each stored item in `items` (unknown items are left out), without loading
    /// text or metadata: the stored times plus the few metadata dates an item's content date is
    /// read from (see [`ItemDates`]). Looked up in batches of [`LOOKUP_BATCH`] items, one query
    /// each, on a read connection (so a date-filtered search never waits on, or holds up,
    /// ingest).
    pub fn item_dates(&self, items: &[ItemRef]) -> Result<HashMap<ItemRef, ItemDates>> {
        /// A metadata string at `path`; NULL when absent or not a string.
        fn text(path: &str) -> String {
            format!(
                "CASE WHEN json_type(i.metadata, '{path}') = 'text' \
                 THEN json_extract(i.metadata, '{path}') END"
            )
        }
        let sql = format!(
            "SELECT want.key, {}, {}, {}, COALESCE({}, {}, {}), i.title,
                    i.source_modified_at, i.created_at, i.updated_at
             {WANTED_ITEMS_JOIN}",
            text("$.extracted.date"),
            text("$.extracted.frontmatter.date"),
            text("$.extracted.frontmatter.created"),
            text("$.extracted.start"),
            text("$.source.start.date_time"),
            text("$.source.start.date"),
        );
        let mut out = HashMap::with_capacity(items.len());
        for batch in items.chunks(LOOKUP_BATCH) {
            let refs = refs_json(batch)?;
            let rows: Vec<(usize, DateRow)> = self.with_read_conn(|c| {
                let mut stmt = c.prepare_cached(&sql).db("prepare item_dates")?;
                let rows = stmt
                    .query_map([&refs], |r| {
                        Ok((
                            r.get::<_, i64>(0)? as usize,
                            DateRow {
                                extracted: r.get(1)?,
                                fm_date: r.get(2)?,
                                fm_created: r.get(3)?,
                                event_start: r.get(4)?,
                                title: r.get(5)?,
                                modified: r.get(6)?,
                                created: r.get(7)?,
                                updated: r.get(8)?,
                            },
                        ))
                    })
                    .db("query item_dates")?;
                rows.collect::<rusqlite::Result<_>>().db("read item_dates")
            })?;
            for (idx, row) in rows {
                let Some(item) = batch.get(idx) else {
                    continue;
                };
                out.insert(
                    item.clone(),
                    ItemDates {
                        extracted_date: row.extracted,
                        frontmatter_date: row.fm_date,
                        frontmatter_created: row.fm_created,
                        event_start: row.event_start,
                        title: row.title,
                        source_modified_at: row.modified.as_deref().map(parse_time).transpose()?,
                        created_at: parse_time(&row.created)?,
                        updated_at: parse_time(&row.updated)?,
                    },
                );
            }
        }
        Ok(out)
    }
}

/// One row of [`Store::item_dates`].
struct DateRow {
    extracted: Option<String>,
    fm_date: Option<String>,
    fm_created: Option<String>,
    event_start: Option<String>,
    title: Option<String>,
    modified: Option<String>,
    created: String,
    updated: String,
}
