//! Query-plan checks: the store's hot search and lookup statements must reach `items` and
//! `chunks` through an index or the integer primary key, never a full table scan.
//!
//! Statements that live inline elsewhere are repeated here verbatim; keep them in sync.

use rusqlite::Connection;

use crate::search::{FIRST_CHUNK_SQL, SNIPPET_SQL, TEXT_SEARCH_SQL, TITLE_SEARCH_SQL};
use crate::types::{NewChunk, NewItem, content_hash};
use crate::{Result, Store};

/// `sources::CHUNK_COLUMNS`.
const CHUNK_COLUMNS: &str = "c.id, i.source, i.key, c.ord, c.text, i.title";

/// `(name, sql)` of every statement checked.
fn statements() -> Vec<(&'static str, String)> {
    vec![
        ("text search", TEXT_SEARCH_SQL.to_owned()),
        ("snippet", SNIPPET_SQL.to_owned()),
        ("title search", TITLE_SEARCH_SQL.to_owned()),
        ("first chunk lookup", FIRST_CHUNK_SQL.to_owned()),
        // search::search_vector
        (
            "vector hit detail lookup",
            "SELECT i.source, i.key, i.title, ch.text
             FROM chunks ch JOIN items i ON i.id = ch.item_id
             WHERE ch.id = ?1"
                .to_owned(),
        ),
        // sources::get_chunks
        (
            "get_chunks",
            format!(
                "SELECT {CHUNK_COLUMNS}
                 FROM chunks c JOIN items i ON i.id = c.item_id
                 WHERE c.id IN (?,?,?,?)"
            ),
        ),
        // sources::get_chunks_by_ord
        (
            "get_chunks_by_ord",
            format!(
                "SELECT {CHUNK_COLUMNS}
                 FROM items i JOIN chunks c ON c.item_id = i.id
                 WHERE i.source = ?1 AND i.key = ?2 AND c.ord = ?3"
            ),
        ),
        // sources::neighbours
        (
            "neighbours: find chunk",
            "SELECT item_id, ord FROM chunks WHERE id = ?1".to_owned(),
        ),
        (
            "neighbours",
            format!(
                "SELECT {CHUNK_COLUMNS}
                 FROM chunks c JOIN items i ON i.id = c.item_id
                 WHERE c.item_id = ?1 AND c.ord BETWEEN ?2 AND ?3
                 ORDER BY c.ord"
            ),
        ),
        // sources::item_versions
        (
            "item versions by source",
            "SELECT key, version FROM items WHERE source = ?1".to_owned(),
        ),
        // items: read an item, find its id, check for chunks
        (
            "item by (source, key)",
            "SELECT * FROM items WHERE source = ?1 AND key = ?2".to_owned(),
        ),
        (
            "item id by (source, key)",
            "SELECT id FROM items WHERE source = ?1 AND key = ?2".to_owned(),
        ),
        (
            "item has chunks",
            "SELECT EXISTS (SELECT 1 FROM chunks WHERE item_id = ?1)".to_owned(),
        ),
    ]
}

fn plan(c: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = c
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap_or_else(|e| panic!("explain {sql}: {e}"));
    // EXPLAIN only plans the statement, but rusqlite still wants every parameter bound.
    let nulls = vec![rusqlite::types::Null; stmt.parameter_count()];
    stmt.query_map(rusqlite::params_from_iter(nulls), |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// A plan step that reads every row of a real table (FTS5 virtual-table scans are index
/// lookups; a constant row is not a table; `top` is text search's bounded CTE).
fn is_full_scan(detail: &str) -> bool {
    detail.starts_with("SCAN ")
        && !detail.contains("VIRTUAL TABLE")
        && !detail.contains("CONSTANT ROW")
        && detail != "SCAN top"
}

fn check_plans(store: &Store, label: &str) {
    store
        .with_conn(|c| {
            for (name, sql) in statements() {
                let steps = plan(c, &sql);
                assert!(!steps.is_empty(), "{label}: {name}: empty plan");
                let scans: Vec<&String> = steps.iter().filter(|d| is_full_scan(d)).collect();
                assert!(
                    scans.is_empty(),
                    "{label}: {name} scans a table: {scans:?}\nfull plan: {steps:#?}"
                );
            }
            Ok(())
        })
        .unwrap();
}

fn populate(store: &Store) -> Result<()> {
    for i in 0..200 {
        let body = format!("item {i} body about topic {}", i % 7);
        let it = NewItem::new(
            format!("src{}", i % 3),
            format!("key{i}"),
            content_hash(&body),
        )
        .with_title(format!("title {i}"))
        .with_text(body.clone());
        store.upsert_item(&it)?;
        let chunks: Vec<NewChunk> = (0..5)
            .map(|k| NewChunk::text(format!("{body} part {k}")))
            .collect();
        store.replace_chunks(&it.item, &chunks)?;
    }
    Ok(())
}

#[test]
fn hot_statements_use_indexes() {
    let store = Store::open_in_memory().unwrap();
    // Control: an unindexed filter is caught.
    store
        .with_conn(|c| {
            let steps = plan(c, "SELECT id FROM items WHERE title = ?1");
            assert!(steps.iter().any(|d| is_full_scan(d)), "{steps:?}");
            Ok(())
        })
        .unwrap();
    check_plans(&store, "empty store");
    populate(&store).unwrap();
    check_plans(&store, "populated store");
    // With table statistics the planner may change its mind; it must still not scan.
    store
        .with_conn(|c| {
            c.execute_batch("ANALYZE;").unwrap();
            Ok(())
        })
        .unwrap();
    check_plans(&store, "after ANALYZE");
}

#[test]
fn full_scan_detection() {
    assert!(is_full_scan("SCAN items"));
    assert!(is_full_scan("SCAN c"));
    assert!(!is_full_scan("SCAN chunks_fts VIRTUAL TABLE INDEX 0:M1"));
    assert!(!is_full_scan(
        "SEARCH ch USING INTEGER PRIMARY KEY (rowid=?)"
    ));
    assert!(!is_full_scan("SCAN CONSTANT ROW"));
    assert!(!is_full_scan("USE TEMP B-TREE FOR ORDER BY"));
}
