//! Versioned schema migrations, tracked with `PRAGMA user_version`.
//!
//! Each migration runs in its own transaction together with the `user_version` bump, so a crash
//! leaves the database at a well-defined version and re-running is a no-op. Statements also use
//! `IF NOT EXISTS` as a second line of defence.
//!
//! The `sqlite-vec` table (`chunk_vec`) is not created here: its dimension is only known when the
//! first embedding arrives (see `items::ensure_vec_table`).

use nugget_core::{Error, Result};
use rusqlite::{Connection, TransactionBehavior};

use crate::error::DbExt;

/// Ordered migrations; `MIGRATIONS[i]` takes the schema from version `i` to `i + 1`.
const MIGRATIONS: &[&str] = &[
    // v1: items, chunks, FTS5, cursors, egress ledger, audit log.
    r#"
    CREATE TABLE IF NOT EXISTS meta (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS items (
        id                 INTEGER PRIMARY KEY,
        source             TEXT NOT NULL,
        key                TEXT NOT NULL,
        title              TEXT,
        content_hash       TEXT NOT NULL,
        mime               TEXT,
        text               TEXT,
        metadata           TEXT NOT NULL DEFAULT '{}',
        source_modified_at TEXT,
        created_at         TEXT NOT NULL,
        updated_at         TEXT NOT NULL,
        UNIQUE (source, key)
    );
    CREATE INDEX IF NOT EXISTS items_content_hash ON items (content_hash);

    CREATE TABLE IF NOT EXISTS chunks (
        id      INTEGER PRIMARY KEY,
        item_id INTEGER NOT NULL REFERENCES items (id) ON DELETE CASCADE,
        ord     INTEGER NOT NULL,
        text    TEXT NOT NULL,
        UNIQUE (item_id, ord)
    );

    CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5 (
        text,
        content = 'chunks',
        content_rowid = 'id',
        tokenize = 'porter unicode61 remove_diacritics 2'
    );
    CREATE TRIGGER IF NOT EXISTS chunks_fts_ai AFTER INSERT ON chunks BEGIN
        INSERT INTO chunks_fts (rowid, text) VALUES (new.id, new.text);
    END;
    CREATE TRIGGER IF NOT EXISTS chunks_fts_ad AFTER DELETE ON chunks BEGIN
        INSERT INTO chunks_fts (chunks_fts, rowid, text) VALUES ('delete', old.id, old.text);
    END;
    CREATE TRIGGER IF NOT EXISTS chunks_fts_au AFTER UPDATE OF text ON chunks BEGIN
        INSERT INTO chunks_fts (chunks_fts, rowid, text) VALUES ('delete', old.id, old.text);
        INSERT INTO chunks_fts (rowid, text) VALUES (new.id, new.text);
    END;

    CREATE TABLE IF NOT EXISTS cursors (
        source     TEXT PRIMARY KEY,
        value      TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS egress (
        id             INTEGER PRIMARY KEY,
        at             TEXT NOT NULL,
        host           TEXT NOT NULL,
        feature        TEXT NOT NULL,
        bytes_sent     INTEGER NOT NULL,
        bytes_received INTEGER NOT NULL,
        items          TEXT NOT NULL DEFAULT '[]'
    );

    CREATE TABLE IF NOT EXISTS audit (
        seq       INTEGER PRIMARY KEY,
        at        TEXT NOT NULL,
        actor     TEXT NOT NULL,
        action    TEXT NOT NULL,
        target    TEXT NOT NULL,
        detail    TEXT NOT NULL,
        prev_hash TEXT NOT NULL,
        hash      TEXT NOT NULL
    );
    CREATE TRIGGER IF NOT EXISTS audit_no_update BEFORE UPDATE ON audit BEGIN
        SELECT RAISE(ABORT, 'audit log is append-only');
    END;
    CREATE TRIGGER IF NOT EXISTS audit_no_delete BEFORE DELETE ON audit BEGIN
        SELECT RAISE(ABORT, 'audit log is append-only');
    END;
    "#,
    // v2: connector-reported item versions (for cheap change detection) and per-source scans.
    r#"
    ALTER TABLE items ADD COLUMN version TEXT;
    CREATE INDEX IF NOT EXISTS items_source ON items (source);
    "#,
    // v3: full-text index over item titles, fused into text search (see `search::search_text`).
    // External-content table over `items`, kept in sync by triggers. A missing title is indexed
    // as '' so the insert, delete and update triggers always see the same values (an FTS5
    // 'delete' must be given exactly what was indexed). Existing rows are backfilled.
    r#"
    CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5 (
        title,
        content = 'items',
        content_rowid = 'id',
        tokenize = 'porter unicode61 remove_diacritics 2'
    );
    CREATE TRIGGER IF NOT EXISTS items_fts_ai AFTER INSERT ON items BEGIN
        INSERT INTO items_fts (rowid, title) VALUES (new.id, coalesce(new.title, ''));
    END;
    CREATE TRIGGER IF NOT EXISTS items_fts_ad AFTER DELETE ON items BEGIN
        INSERT INTO items_fts (items_fts, rowid, title)
            VALUES ('delete', old.id, coalesce(old.title, ''));
    END;
    CREATE TRIGGER IF NOT EXISTS items_fts_au AFTER UPDATE OF title ON items
        WHEN old.title IS NOT new.title BEGIN
        INSERT INTO items_fts (items_fts, rowid, title)
            VALUES ('delete', old.id, coalesce(old.title, ''));
        INSERT INTO items_fts (rowid, title) VALUES (new.id, coalesce(new.title, ''));
    END;
    INSERT INTO items_fts (rowid, title) SELECT id, coalesce(title, '') FROM items;
    "#,
];

/// Schema version this build creates and understands.
pub const LATEST_VERSION: u32 = MIGRATIONS.len() as u32;

pub(crate) fn user_version(conn: &Connection) -> Result<u32> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .db("read schema version")
}

/// Bring the schema up to [`LATEST_VERSION`]. Returns how many migrations were applied
/// (0 when already current). Refuses databases written by a newer build.
pub(crate) fn migrate(conn: &mut Connection) -> Result<u32> {
    let current = user_version(conn)?;
    if current > LATEST_VERSION {
        return Err(Error::Unsupported(format!(
            "store schema version {current} is newer than this build supports ({LATEST_VERSION}); \
             upgrade Nugget"
        )));
    }
    let mut applied = 0;
    for (idx, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let target = idx as u32 + 1;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .db("begin migration")?;
        // Re-check inside the write lock in case another connection migrated concurrently.
        if user_version(&tx)? >= target {
            continue;
        }
        tx.execute_batch(sql)
            .db(&format!("apply migration v{target}"))?;
        tx.pragma_update(None, "user_version", target)
            .db("bump schema version")?;
        tx.commit().db("commit migration")?;
        tracing::info!(version = target, "applied store migration");
        applied += 1;
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema_snapshot(conn: &Connection) -> Vec<(String, String)> {
        let mut stmt = conn
            .prepare("SELECT name, IFNULL(sql, '') FROM sqlite_master ORDER BY name")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn migrate_twice_is_a_noop() {
        let mut conn = Connection::open_in_memory().unwrap();
        assert_eq!(user_version(&conn).unwrap(), 0);
        assert_eq!(migrate(&mut conn).unwrap(), LATEST_VERSION);
        let before = schema_snapshot(&conn);
        assert_eq!(user_version(&conn).unwrap(), LATEST_VERSION);

        assert_eq!(migrate(&mut conn).unwrap(), 0);
        assert_eq!(user_version(&conn).unwrap(), LATEST_VERSION);
        assert_eq!(schema_snapshot(&conn), before);
    }

    #[test]
    fn create_statements_are_idempotent_even_without_version_tracking() {
        // v1 is pure `CREATE ... IF NOT EXISTS`; later migrations may use `ALTER TABLE`, which
        // SQLite cannot make conditional, so they rely on `user_version` tracking alone.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
    }

    /// Apply migrations up to (and including) version `to` only, as an older build would.
    fn migrate_to(conn: &mut Connection, to: u32) {
        for (idx, sql) in MIGRATIONS.iter().enumerate().take(to as usize) {
            conn.execute_batch(sql).unwrap();
            conn.pragma_update(None, "user_version", idx as u32 + 1)
                .unwrap();
        }
    }

    fn title_matches(conn: &Connection, q: &str) -> Vec<i64> {
        let mut stmt = conn
            .prepare("SELECT rowid FROM items_fts WHERE items_fts MATCH ?1 ORDER BY rowid")
            .unwrap();
        stmt.query_map([q], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn v2_database_with_data_migrates_to_v3_with_searchable_titles() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_to(&mut conn, 2);
        assert_eq!(user_version(&conn).unwrap(), 2);
        conn.execute_batch(
            "INSERT INTO items (id, source, key, title, content_hash, created_at, updated_at, version)
                 VALUES (1, 's', 'a', 'Boiler repair notes', 'h1', 't', 't', 'v1'),
                        (2, 's', 'b', NULL, 'h2', 't', 't', NULL),
                        (3, 's', 'c', 'Garden plan', 'h3', 't', 't', NULL);
             INSERT INTO chunks (item_id, ord, text) VALUES (1, 0, 'gas valve'), (2, 0, 'x');",
        )
        .unwrap();

        assert_eq!(migrate(&mut conn).unwrap(), LATEST_VERSION - 2);
        assert_eq!(user_version(&conn).unwrap(), LATEST_VERSION);
        // Existing rows were backfilled; data and v2 columns are intact.
        assert_eq!(title_matches(&conn, "boiler"), vec![1]);
        assert_eq!(title_matches(&conn, "gardens"), vec![3], "stemmed");
        let version: String = conn
            .query_row("SELECT version FROM items WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, "v1");
        conn.execute_batch("INSERT INTO items_fts (items_fts) VALUES ('integrity-check');")
            .unwrap();

        // Triggers keep the index in step: title change, NULL ↔ text, delete.
        conn.execute_batch(
            "UPDATE items SET title = 'Kitchen boiler' WHERE id = 3;
             UPDATE items SET title = 'Untitled draft' WHERE id = 2;
             UPDATE items SET title = NULL WHERE id = 1;
             UPDATE items SET content_hash = 'h4' WHERE id = 3;",
        )
        .unwrap();
        assert_eq!(title_matches(&conn, "boiler"), vec![3]);
        assert_eq!(title_matches(&conn, "draft"), vec![2]);
        assert!(title_matches(&conn, "garden").is_empty());
        conn.execute_batch("DELETE FROM items WHERE id = 3;")
            .unwrap();
        assert!(title_matches(&conn, "boiler").is_empty());
        conn.execute_batch("INSERT INTO items_fts (items_fts) VALUES ('integrity-check');")
            .unwrap();

        // Re-running is a no-op.
        assert_eq!(migrate(&mut conn).unwrap(), 0);
    }

    #[test]
    fn newer_schema_is_refused() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", LATEST_VERSION + 1)
            .unwrap();
        assert!(matches!(migrate(&mut conn), Err(Error::Unsupported(_))));
    }
}
