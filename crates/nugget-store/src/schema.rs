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
    // v4: knowledge graph (see `graph`). Purely additive: no existing table changes, so a v3
    // store with data migrates in place. Everything lives in the same SQLCipher database.
    //
    // - `entities` rows are never deleted: a merged entity stays as a tombstone whose
    //   `merged_into` points at the survivor (chains are flattened on merge), so ids are never
    //   reused and old ids keep resolving. `name_lower` is the Unicode-lowercased name (SQLite's
    //   `lower()` is ASCII-only) used for ordering.
    // - `entity_aliases` holds every name an entity answers to: its current name (`is_name = 1`,
    //   kept in step with `entities.name`) and its aliases. One index on `alias_norm` therefore
    //   serves name lookup, and `entity_names_fts` (trigram, external content) serves substring
    //   search over names and aliases.
    // - Claim ids use AUTOINCREMENT so a claim folded away by a merge never has its id reused.
    // - `claim_evidence` and `mentions` reference items by `(source, key)` (not `items.id`) so
    //   provenance survives an item being re-ingested, and `retract_item` is one index range.
    //   `chunk_id` is deliberately not a foreign key: chunks are replaced on re-chunking.
    // - `graph_changes` keeps the latest change seq per entity; `graph_seq` is the monotonic
    //   counter behind it.
    r#"
    CREATE TABLE IF NOT EXISTS entities (
        id          TEXT PRIMARY KEY,
        kind        TEXT NOT NULL,
        name        TEXT NOT NULL,
        name_lower  TEXT NOT NULL,
        attributes  TEXT NOT NULL DEFAULT '{}',
        created_at  TEXT NOT NULL,
        updated_at  TEXT NOT NULL,
        merged_into TEXT REFERENCES entities (id),
        merged_at   TEXT,
        CHECK (merged_into IS NULL OR merged_into <> id)
    );
    CREATE INDEX IF NOT EXISTS entities_live_name
        ON entities (name_lower, id) WHERE merged_into IS NULL;
    CREATE INDEX IF NOT EXISTS entities_live_kind_name
        ON entities (kind, name_lower, id) WHERE merged_into IS NULL;
    CREATE INDEX IF NOT EXISTS entities_merged_into
        ON entities (merged_into) WHERE merged_into IS NOT NULL;

    CREATE TABLE IF NOT EXISTS entity_aliases (
        id          INTEGER PRIMARY KEY,
        entity_id   TEXT NOT NULL REFERENCES entities (id),
        alias       TEXT NOT NULL,
        alias_norm  TEXT NOT NULL,
        alias_lower TEXT NOT NULL,
        is_name     INTEGER NOT NULL DEFAULT 0 CHECK (is_name IN (0, 1))
    );
    CREATE UNIQUE INDEX IF NOT EXISTS entity_aliases_unique
        ON entity_aliases (entity_id, is_name, alias);
    CREATE UNIQUE INDEX IF NOT EXISTS entity_aliases_one_name
        ON entity_aliases (entity_id) WHERE is_name = 1;
    CREATE INDEX IF NOT EXISTS entity_aliases_norm ON entity_aliases (alias_norm);

    CREATE VIRTUAL TABLE IF NOT EXISTS entity_names_fts USING fts5 (
        alias_lower,
        content = 'entity_aliases',
        content_rowid = 'id',
        tokenize = 'trigram'
    );
    CREATE TRIGGER IF NOT EXISTS entity_names_fts_ai AFTER INSERT ON entity_aliases BEGIN
        INSERT INTO entity_names_fts (rowid, alias_lower) VALUES (new.id, new.alias_lower);
    END;
    CREATE TRIGGER IF NOT EXISTS entity_names_fts_ad AFTER DELETE ON entity_aliases BEGIN
        INSERT INTO entity_names_fts (entity_names_fts, rowid, alias_lower)
            VALUES ('delete', old.id, old.alias_lower);
    END;
    CREATE TRIGGER IF NOT EXISTS entity_names_fts_au AFTER UPDATE OF alias_lower ON entity_aliases
        BEGIN
        INSERT INTO entity_names_fts (entity_names_fts, rowid, alias_lower)
            VALUES ('delete', old.id, old.alias_lower);
        INSERT INTO entity_names_fts (rowid, alias_lower) VALUES (new.id, new.alias_lower);
    END;

    CREATE TABLE IF NOT EXISTS entity_identifiers (
        id        INTEGER PRIMARY KEY,
        entity_id TEXT NOT NULL REFERENCES entities (id),
        scheme    TEXT NOT NULL,
        value     TEXT NOT NULL,
        UNIQUE (scheme, value)
    );
    CREATE INDEX IF NOT EXISTS entity_identifiers_entity ON entity_identifiers (entity_id);

    CREATE TABLE IF NOT EXISTS claims (
        id             INTEGER PRIMARY KEY AUTOINCREMENT,
        subject        TEXT NOT NULL REFERENCES entities (id),
        predicate      TEXT NOT NULL,
        object_entity  TEXT REFERENCES entities (id),
        object_literal TEXT,
        text           TEXT NOT NULL,
        confidence     REAL NOT NULL,
        status         TEXT NOT NULL
            CHECK (status IN ('active', 'superseded', 'retracted', 'user_asserted')),
        valid_from     TEXT,
        valid_to       TEXT,
        created_at     TEXT NOT NULL,
        updated_at     TEXT NOT NULL,
        CHECK ((object_entity IS NULL) <> (object_literal IS NULL))
    );
    -- "Find the active claim with this triple" and "claims with this subject".
    CREATE INDEX IF NOT EXISTS claims_triple
        ON claims (subject, predicate, object_entity, object_literal, status);
    CREATE INDEX IF NOT EXISTS claims_object
        ON claims (object_entity, status) WHERE object_entity IS NOT NULL;

    CREATE TABLE IF NOT EXISTS claim_evidence (
        id         INTEGER PRIMARY KEY,
        claim_id   INTEGER NOT NULL REFERENCES claims (id) ON DELETE CASCADE,
        source     TEXT NOT NULL,
        key        TEXT NOT NULL,
        chunk_id   INTEGER,
        quote      TEXT,
        created_at TEXT NOT NULL
    );
    -- One row per (claim, item, chunk, quote). NULLs map to an empty blob, which never equals
    -- an integer chunk id or a text quote, so "no chunk" / "no quote" are distinct values.
    CREATE UNIQUE INDEX IF NOT EXISTS claim_evidence_unique
        ON claim_evidence (claim_id, source, key, ifnull(chunk_id, X''), ifnull(quote, X''));
    CREATE INDEX IF NOT EXISTS claim_evidence_item ON claim_evidence (source, key);

    CREATE TABLE IF NOT EXISTS mentions (
        id         INTEGER PRIMARY KEY,
        entity_id  TEXT NOT NULL REFERENCES entities (id),
        source     TEXT NOT NULL,
        key        TEXT NOT NULL,
        chunk_id   INTEGER,
        surface    TEXT NOT NULL,
        confidence REAL NOT NULL,
        created_at TEXT NOT NULL
    );
    CREATE UNIQUE INDEX IF NOT EXISTS mentions_unique
        ON mentions (entity_id, source, key, ifnull(chunk_id, X''), surface);
    CREATE INDEX IF NOT EXISTS mentions_entity ON mentions (entity_id, id);
    CREATE INDEX IF NOT EXISTS mentions_item ON mentions (source, key);

    CREATE TABLE IF NOT EXISTS graph_seq (
        id  INTEGER PRIMARY KEY CHECK (id = 1),
        seq INTEGER NOT NULL
    );
    INSERT OR IGNORE INTO graph_seq (id, seq) VALUES (1, 0);
    CREATE TABLE IF NOT EXISTS graph_changes (
        entity_id TEXT PRIMARY KEY,
        seq       INTEGER NOT NULL
    );
    CREATE UNIQUE INDEX IF NOT EXISTS graph_changes_seq ON graph_changes (seq);
    "#,
    // v5: knowledge-graph contract additions (M2 wave 2) and the item change feed.
    //
    // - `claims` is rebuilt so its status CHECK admits 'rejected' (SQLite cannot alter a CHECK).
    //   `migrate` runs migrations with foreign keys off, so dropping the old table does not
    //   cascade into `claim_evidence`; the AUTOINCREMENT high-water mark is carried over so claim
    //   ids are still never reused. `PRAGMA foreign_key_check` runs before the commit.
    // - `entities.deleted_at` marks entities removed by `prune_orphans` (tombstones: the row and
    //   its id stay, so ids are never reused; lookups treat it as gone).
    // - `entity_detail_sources`: which item supplied each alias / identifier of an entity
    //   (provenance for scoped MCP clients). Keyed by kind, scheme and normalised value.
    // - `item_changes`: one row per item key with the seq of its latest insert, update or delete
    //   (`deleted = 1`: a tombstone), maintained by triggers on `items` so every write path
    //   (upserts, `delete_item`, `delete_source`) is covered. Existing items are backfilled in id
    //   order.
    r#"
    CREATE TABLE claims_v5 (
        id             INTEGER PRIMARY KEY AUTOINCREMENT,
        subject        TEXT NOT NULL REFERENCES entities (id),
        predicate      TEXT NOT NULL,
        object_entity  TEXT REFERENCES entities (id),
        object_literal TEXT,
        text           TEXT NOT NULL,
        confidence     REAL NOT NULL,
        status         TEXT NOT NULL
            CHECK (status IN ('active', 'superseded', 'retracted', 'user_asserted', 'rejected')),
        valid_from     TEXT,
        valid_to       TEXT,
        created_at     TEXT NOT NULL,
        updated_at     TEXT NOT NULL,
        CHECK ((object_entity IS NULL) <> (object_literal IS NULL))
    );
    INSERT INTO claims_v5 (id, subject, predicate, object_entity, object_literal, text,
                           confidence, status, valid_from, valid_to, created_at, updated_at)
        SELECT id, subject, predicate, object_entity, object_literal, text, confidence, status,
               valid_from, valid_to, created_at, updated_at
        FROM claims ORDER BY id;
    DELETE FROM sqlite_sequence WHERE name = 'claims_v5';
    INSERT INTO sqlite_sequence (name, seq)
        SELECT 'claims_v5', seq FROM sqlite_sequence WHERE name = 'claims';
    DROP TABLE claims;
    ALTER TABLE claims_v5 RENAME TO claims;
    CREATE INDEX IF NOT EXISTS claims_triple
        ON claims (subject, predicate, object_entity, object_literal, status);
    CREATE INDEX IF NOT EXISTS claims_object
        ON claims (object_entity, status) WHERE object_entity IS NOT NULL;

    -- `entities.deleted_at` is added by `ADDED_COLUMNS` (only when missing).

    CREATE TABLE IF NOT EXISTS entity_detail_sources (
        id         INTEGER PRIMARY KEY,
        entity_id  TEXT NOT NULL REFERENCES entities (id),
        kind       TEXT NOT NULL CHECK (kind IN ('alias', 'identifier')),
        scheme     TEXT NOT NULL,
        value      TEXT NOT NULL,
        source     TEXT NOT NULL,
        key        TEXT NOT NULL,
        created_at TEXT NOT NULL
    );
    CREATE UNIQUE INDEX IF NOT EXISTS entity_detail_sources_unique
        ON entity_detail_sources (entity_id, kind, scheme, value, source, key);
    CREATE INDEX IF NOT EXISTS entity_detail_sources_item ON entity_detail_sources (source, key);

    CREATE TABLE IF NOT EXISTS item_changes (
        source  TEXT NOT NULL,
        key     TEXT NOT NULL,
        seq     INTEGER NOT NULL,
        deleted INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1)),
        PRIMARY KEY (source, key)
    ) WITHOUT ROWID;
    CREATE UNIQUE INDEX IF NOT EXISTS item_changes_seq ON item_changes (seq);
    INSERT OR IGNORE INTO item_changes (source, key, seq, deleted)
        SELECT source, key, row_number() OVER (ORDER BY id), 0 FROM items;
    CREATE TRIGGER IF NOT EXISTS items_changes_ai AFTER INSERT ON items BEGIN
        INSERT INTO item_changes (source, key, seq, deleted)
            VALUES (new.source, new.key,
                    (SELECT coalesce(max(seq), 0) + 1 FROM item_changes), 0)
            ON CONFLICT (source, key) DO UPDATE SET seq = excluded.seq, deleted = 0;
    END;
    CREATE TRIGGER IF NOT EXISTS items_changes_au AFTER UPDATE ON items BEGIN
        INSERT INTO item_changes (source, key, seq, deleted)
            VALUES (new.source, new.key,
                    (SELECT coalesce(max(seq), 0) + 1 FROM item_changes), 0)
            ON CONFLICT (source, key) DO UPDATE SET seq = excluded.seq, deleted = 0;
    END;
    CREATE TRIGGER IF NOT EXISTS items_changes_ad AFTER DELETE ON items BEGIN
        INSERT INTO item_changes (source, key, seq, deleted)
            VALUES (old.source, old.key,
                    (SELECT coalesce(max(seq), 0) + 1 FROM item_changes), 1)
            ON CONFLICT (source, key) DO UPDATE SET seq = excluded.seq, deleted = 1;
    END;
    "#,
    // v6: two-phase egress ledger (see `egress`). A row is written `open` before the first byte
    // is sent and becomes `done` when the exchange ends; rows still `open` when a store is
    // opened were left by a crash and become `interrupted`. Existing rows were one-shot: `done`.
    // `egress.state` is added by `ADDED_COLUMNS`, its index by `AFTER_COLUMNS`.
    "",
    // v7: keyed audit chain (see `audit`). Entries from `audit_v2_from` on are linked with an
    // HMAC keyed from the store key; the SHA-256 entries written before this migration keep
    // their v1 links and are still verified as such.
    r#"
    INSERT OR IGNORE INTO meta (key, value)
        SELECT 'audit_v2_from', coalesce(max(seq), 0) + 1 FROM audit;
    "#,
    // v8: item text stored once. When an item's chunks cover its text, the text is kept as a
    // compact layout over the chunks (literal gaps + chunk byte ranges, see `items::text_layout`)
    // in `item_text`, and `items.text` is NULL. A row here always takes precedence over
    // `items.text`; it goes away with the item. Existing rows keep their text in `items.text`.
    // Kept in a side table (not a column on `items`) so re-chunking an item rewrites its layout
    // without touching the `items` row, its `updated_at` or the item change feed.
    r#"
    CREATE TABLE IF NOT EXISTS item_text (
        item_id INTEGER PRIMARY KEY REFERENCES items (id) ON DELETE CASCADE,
        layout  BLOB NOT NULL
    );
    "#,
];

/// Columns a migration adds, as `(version, table, column, type)`. Added from Rust only when
/// missing, so these migrations stay idempotent (`ALTER TABLE ADD COLUMN` cannot be made
/// conditional in SQL).
const ADDED_COLUMNS: &[(u32, &str, &str, &str)] = &[
    (5, "entities", "deleted_at", "TEXT"),
    (
        6,
        "egress",
        "state",
        "TEXT NOT NULL DEFAULT 'done' CHECK (state IN ('open', 'done', 'interrupted'))",
    ),
];

/// Idempotent statements a migration runs after its `ADDED_COLUMNS` (they use those columns).
const AFTER_COLUMNS: &[(u32, &str)] = &[(
    6,
    "CREATE INDEX IF NOT EXISTS egress_unfinished ON egress (state) WHERE state <> 'done';",
)];

fn ensure_column(conn: &Connection, table: &str, column: &str, ty: &str) -> Result<()> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2)",
            [table, column],
            |r| r.get(0),
        )
        .db("inspect table columns")?;
    if !exists {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty};"))
            .db(&format!("add column {table}.{column}"))?;
    }
    Ok(())
}

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
    if current == LATEST_VERSION {
        return Ok(0);
    }
    // Table rebuilds (v5) must not cascade deletes, and `PRAGMA foreign_keys` cannot change
    // inside a transaction: switch enforcement off around the migrations and check before each
    // commit instead.
    let fk_on: bool = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .db("read foreign key setting")?;
    if fk_on {
        conn.execute_batch("PRAGMA foreign_keys = OFF;")
            .db("disable foreign keys for migration")?;
    }
    let res = apply_migrations(conn, current);
    if fk_on {
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .db("re-enable foreign keys")?;
    }
    res
}

fn apply_migrations(conn: &mut Connection, current: u32) -> Result<u32> {
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
        for (_, table, column, ty) in ADDED_COLUMNS.iter().filter(|c| c.0 == target) {
            ensure_column(&tx, table, column, ty)?;
        }
        for (_, sql) in AFTER_COLUMNS.iter().filter(|c| c.0 == target) {
            tx.execute_batch(sql)
                .db(&format!("apply migration v{target}"))?;
        }
        let violations: i64 = tx
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .db("check foreign keys")?;
        if violations > 0 {
            return Err(Error::Integrity(format!(
                "migration v{target} would leave {violations} foreign key violations"
            )));
        }
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
            for (_, t, c, ty) in ADDED_COLUMNS.iter().filter(|c| c.0 == idx as u32 + 1) {
                ensure_column(conn, t, c, ty).unwrap();
            }
            for (_, sql) in AFTER_COLUMNS.iter().filter(|c| c.0 == idx as u32 + 1) {
                conn.execute_batch(sql).unwrap();
            }
            conn.pragma_update(None, "user_version", idx as u32 + 1)
                .unwrap();
        }
    }

    #[test]
    fn v4_graph_migrates_to_v5_keeping_claims_evidence_and_ids() {
        use nugget_core::ids::{ItemKey, ItemRef, SourceId};
        use nugget_graph::{
            ClaimObject, ClaimStatus, EntityKind, Evidence, GraphStore, NewClaim, NewEntity,
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nugget.db");
        let key = [9u8; 32];
        {
            let mut conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex::encode(key)))
                .unwrap();
            conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
            migrate_to(&mut conn, 4);
            // Claim 2 was folded away by a merge: the AUTOINCREMENT mark (3) is above max(id).
            conn.execute_batch(
                "INSERT INTO items (id, source, key, content_hash, created_at, updated_at)
                     VALUES (1, 'mail', 'm1', 'h1', 't', 't'), (2, 'mail', 'm2', 'h2', 't', 't');
                 INSERT INTO entities (id, kind, name, name_lower, created_at, updated_at)
                     VALUES ('ent_a', 'person', 'Sam', 'sam', '2026-01-01T00:00:00Z',
                             '2026-01-01T00:00:00Z');
                 INSERT INTO entity_aliases (entity_id, alias, alias_norm, alias_lower, is_name)
                     VALUES ('ent_a', 'Sam', 'sam', 'sam', 1);
                 INSERT INTO claims (id, subject, predicate, object_literal, text, confidence,
                                     status, created_at, updated_at)
                     VALUES (1, 'ent_a', 'likes', 'tea', 'Sam likes tea', 0.8, 'active',
                             '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
                            (3, 'ent_a', 'likes', 'jazz', 'Sam likes jazz', 0.7, 'superseded',
                             '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
                 UPDATE sqlite_sequence SET seq = 3 WHERE name = 'claims';
                 INSERT INTO claim_evidence (claim_id, source, key, chunk_id, quote, created_at)
                     VALUES (1, 'mail', 'm1', 1, 'tea please', 't'),
                            (3, 'mail', 'm2', NULL, NULL, 't');",
            )
            .unwrap();
        }
        let store = crate::Store::open(&path, &crate::StoreKey::from_bytes(key)).unwrap();
        assert_eq!(store.schema_version().unwrap(), LATEST_VERSION);
        let g = store.graph();
        let tea = g.get_claim(1).unwrap().unwrap();
        assert_eq!(
            tea.evidence.len(),
            1,
            "evidence survived the claims rebuild"
        );
        assert_eq!(tea.evidence[0].quote.as_deref(), Some("tea please"));
        assert_eq!(
            g.get_claim(3).unwrap().unwrap().status,
            ClaimStatus::Superseded
        );
        // The new status is accepted, and ids keep counting from the old high-water mark.
        g.set_claim_status(1, ClaimStatus::Rejected).unwrap();
        let sam = nugget_core::ids::EntityId::new("ent_a");
        let next = g
            .add_claim(&NewClaim {
                subject: sam.clone(),
                predicate: "plays".into(),
                object: ClaimObject::Literal("chess".into()),
                text: "Sam plays chess".into(),
                confidence: 0.5,
                status: ClaimStatus::Active,
                evidence: vec![Evidence {
                    item: ItemRef {
                        source: SourceId::new("mail"),
                        key: ItemKey::new("m1"),
                    },
                    chunk_id: None,
                    quote: None,
                }],
                valid_from: None,
                valid_to: None,
            })
            .unwrap();
        assert_eq!(next, 4, "claim ids are never reused across the rebuild");
        // Existing items were backfilled into the change feed, in id order.
        let (changes, head) = store.items_changed_since(0, 0).unwrap();
        let keys: Vec<&str> = changes.iter().map(|c| c.item.key.as_str()).collect();
        assert_eq!(keys, ["m1", "m2"]);
        assert_eq!(head, 2);
        // Foreign keys hold and cascade again after the migration.
        store
            .with_conn(|c| {
                let fk: bool = c
                    .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
                    .db("fk")?;
                assert!(fk, "foreign keys re-enabled");
                c.execute_batch("PRAGMA foreign_key_check;").db("fk check")
            })
            .unwrap();
        assert!(g.prune_orphans(None).unwrap().is_empty());
        g.create_entity(&NewEntity::new(EntityKind::Topic, "Tea"))
            .unwrap();
        drop(g);
        drop(store);
        let store = crate::Store::open(&path, &crate::StoreKey::from_bytes(key)).unwrap();
        assert_eq!(store.migrate().unwrap(), 0);
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
    fn encrypted_v3_store_with_data_migrates_to_v4_graph() {
        use nugget_core::ids::{ItemKey, ItemRef, SourceId};
        use nugget_graph::{
            ClaimObject, ClaimStatus, EntityKind, Evidence, GraphStore, NewClaim, NewEntity,
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nugget.db");
        let key = [5u8; 32];
        // A v3 database exactly as the previous build left it: encrypted, with items, chunks,
        // title/chunk FTS, a cursor and an audit entry.
        {
            let mut conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex::encode(key)))
                .unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            migrate_to(&mut conn, 3);
            conn.execute_batch(
                "INSERT INTO items (id, source, key, title, content_hash, created_at, updated_at)
                     VALUES (1, 'mail', 'm1', 'Quarterly boiler review', 'h1', 't', 't'),
                            (2, 'mail', 'm2', 'Garden plan', 'h2', 't', 't');
                 INSERT INTO chunks (item_id, ord, text)
                     VALUES (1, 0, 'Sam from Acme serviced the boiler'), (2, 0, 'tulips');
                 INSERT INTO cursors (source, value, updated_at) VALUES ('mail', '{\"n\":1}', 't');",
            )
            .unwrap();
        }
        assert!(
            !std::fs::read(&path)
                .unwrap()
                .starts_with(b"SQLite format 3"),
            "fixture is encrypted"
        );

        let store = crate::Store::open(&path, &crate::StoreKey::from_bytes(key)).unwrap();
        assert_eq!(store.schema_version().unwrap(), LATEST_VERSION);
        assert_eq!(store.counts().unwrap().items, 2);
        assert_eq!(store.search_text("boiler", 5).unwrap().len(), 1);
        assert_eq!(
            store.get_cursor(&SourceId::new("mail")).unwrap(),
            Some(serde_json::json!({"n": 1}))
        );
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO items_fts (items_fts) VALUES ('integrity-check');
                 INSERT INTO entity_names_fts (entity_names_fts) VALUES ('integrity-check');
                 PRAGMA foreign_key_check;",
                )
                .db("integrity")
            })
            .unwrap();

        // The new graph works against the migrated items and their delete path.
        let g = store.graph();
        let m1 = ItemRef {
            source: SourceId::new("mail"),
            key: ItemKey::new("m1"),
        };
        let sam = g
            .create_entity(&NewEntity::new(EntityKind::Person, "Sam"))
            .unwrap();
        let claim = g
            .add_claim(&NewClaim {
                subject: sam.clone(),
                predicate: "services".into(),
                object: ClaimObject::Literal("the boiler".into()),
                text: "Sam services the boiler".into(),
                confidence: 0.8,
                status: ClaimStatus::Active,
                evidence: vec![Evidence {
                    item: m1.clone(),
                    chunk_id: Some(1),
                    quote: Some("serviced the boiler".into()),
                }],
                valid_from: None,
                valid_to: None,
            })
            .unwrap();
        assert!(store.delete_item(&m1).unwrap());
        assert_eq!(
            g.get_claim(claim).unwrap().unwrap().status,
            ClaimStatus::Retracted
        );
        drop(g);
        drop(store);

        // Still encrypted after migration, and re-opening is a no-op migration.
        assert!(
            !std::fs::read(&path)
                .unwrap()
                .starts_with(b"SQLite format 3")
        );
        assert!(
            crate::Store::open(&path, &crate::StoreKey::from_bytes([6u8; 32])).is_err(),
            "wrong key"
        );
        let store = crate::Store::open(&path, &crate::StoreKey::from_bytes(key)).unwrap();
        assert_eq!(store.migrate().unwrap(), 0);
        assert_eq!(store.graph().get_entity(&sam).unwrap().unwrap().name, "Sam");
    }

    #[test]
    fn newer_schema_is_refused() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", LATEST_VERSION + 1)
            .unwrap();
        assert!(matches!(migrate(&mut conn), Err(Error::Unsupported(_))));
    }
}
