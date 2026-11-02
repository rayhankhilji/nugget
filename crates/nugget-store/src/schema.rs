//! Versioned schema migrations, tracked with `PRAGMA user_version`.

use nugget_core::{Error, Result};
use rusqlite::{Connection, TransactionBehavior};

use crate::error::DbExt;

/// User decisions on claims that must outlive re-extraction (see `GraphStore`).
const USER_DECISIONS: &str = r#"
    DROP TABLE IF EXISTS claim_user_replaced;
    CREATE TABLE IF NOT EXISTS claim_replacements (
        claim_id    INTEGER PRIMARY KEY REFERENCES claims (id) ON DELETE CASCADE,
        replaced_by INTEGER REFERENCES claims (id) ON DELETE CASCADE
    );
    CREATE INDEX IF NOT EXISTS claim_replacements_by ON claim_replacements (replaced_by);
    CREATE TABLE IF NOT EXISTS claim_replacement_evidence (
        claim_id  INTEGER NOT NULL REFERENCES claim_replacements (claim_id) ON DELETE CASCADE,
        source    TEXT NOT NULL,
        key       TEXT NOT NULL,
        quote     TEXT NOT NULL,
        item_hash TEXT,
        PRIMARY KEY (claim_id, source, key, quote)
    ) WITHOUT ROWID;
    CREATE TABLE IF NOT EXISTS claim_assertions (
        claim_id    INTEGER PRIMARY KEY REFERENCES claims (id) ON DELETE CASCADE,
        asserted_at TEXT NOT NULL
    );
    INSERT OR IGNORE INTO claim_assertions (claim_id, asserted_at)
        SELECT id, updated_at FROM claims WHERE status = 'user_asserted';
"#;

/// Per-entity counts kept exact by triggers, so "most connected entities first"
/// (`GraphStore::top_entities`, the graph overview) is an index range instead of counting every
/// entity's claims and mentions.
pub(crate) const ENTITY_STATS: &str = r#"
    CREATE TABLE IF NOT EXISTS entity_stats (
        entity_id TEXT PRIMARY KEY,
        claims    INTEGER NOT NULL DEFAULT 0,
        mentions  INTEGER NOT NULL DEFAULT 0
    ) WITHOUT ROWID;
    CREATE INDEX IF NOT EXISTS entity_stats_rank ON entity_stats (claims DESC, mentions DESC);
    DELETE FROM entity_stats;
    INSERT INTO entity_stats (entity_id, claims, mentions)
        SELECT e.id,
               (SELECT count(*) FROM claims c
                WHERE c.subject = e.id AND c.status IN ('active', 'user_asserted'))
             + (SELECT count(*) FROM claims c
                WHERE c.object_entity = e.id AND c.subject <> e.id
                  AND c.status IN ('active', 'user_asserted')),
               (SELECT count(*) FROM mentions m WHERE m.entity_id = e.id)
        FROM entities e;

    CREATE TRIGGER IF NOT EXISTS entity_stats_entities_ai AFTER INSERT ON entities BEGIN
        INSERT OR IGNORE INTO entity_stats (entity_id) VALUES (new.id);
    END;
    CREATE TRIGGER IF NOT EXISTS entity_stats_entities_ad AFTER DELETE ON entities BEGIN
        DELETE FROM entity_stats WHERE entity_id = old.id;
    END;

    CREATE TRIGGER IF NOT EXISTS entity_stats_mentions_ai AFTER INSERT ON mentions BEGIN
        INSERT INTO entity_stats (entity_id, mentions) VALUES (new.entity_id, 1)
            ON CONFLICT (entity_id) DO UPDATE SET mentions = mentions + 1;
    END;
    CREATE TRIGGER IF NOT EXISTS entity_stats_mentions_ad AFTER DELETE ON mentions BEGIN
        UPDATE entity_stats SET mentions = mentions - 1 WHERE entity_id = old.entity_id;
    END;
    CREATE TRIGGER IF NOT EXISTS entity_stats_mentions_au AFTER UPDATE OF entity_id ON mentions
        WHEN old.entity_id IS NOT new.entity_id BEGIN
        UPDATE entity_stats SET mentions = mentions - 1 WHERE entity_id = old.entity_id;
        INSERT INTO entity_stats (entity_id, mentions) VALUES (new.entity_id, 1)
            ON CONFLICT (entity_id) DO UPDATE SET mentions = mentions + 1;
    END;

    CREATE TRIGGER IF NOT EXISTS entity_stats_claims_ai AFTER INSERT ON claims
        WHEN new.status IN ('active', 'user_asserted') BEGIN
        INSERT INTO entity_stats (entity_id, claims) VALUES (new.subject, 1)
            ON CONFLICT (entity_id) DO UPDATE SET claims = claims + 1;
        INSERT INTO entity_stats (entity_id, claims)
            SELECT new.object_entity, 1
            WHERE new.object_entity IS NOT NULL AND new.object_entity <> new.subject
            ON CONFLICT (entity_id) DO UPDATE SET claims = claims + 1;
    END;
    CREATE TRIGGER IF NOT EXISTS entity_stats_claims_ad AFTER DELETE ON claims
        WHEN old.status IN ('active', 'user_asserted') BEGIN
        UPDATE entity_stats SET claims = claims - 1 WHERE entity_id = old.subject;
        UPDATE entity_stats SET claims = claims - 1
            WHERE entity_id = old.object_entity AND old.object_entity <> old.subject;
    END;
    CREATE TRIGGER IF NOT EXISTS entity_stats_claims_au
        AFTER UPDATE OF subject, object_entity, status ON claims
        WHEN (old.status IN ('active', 'user_asserted') OR new.status IN ('active', 'user_asserted'))
         AND (old.subject IS NOT new.subject OR old.object_entity IS NOT new.object_entity
              OR old.status IS NOT new.status) BEGIN
        UPDATE entity_stats SET claims = claims - 1
            WHERE old.status IN ('active', 'user_asserted') AND entity_id = old.subject;
        UPDATE entity_stats SET claims = claims - 1
            WHERE old.status IN ('active', 'user_asserted')
              AND entity_id = old.object_entity AND old.object_entity <> old.subject;
        INSERT INTO entity_stats (entity_id, claims)
            SELECT new.subject, 1 WHERE new.status IN ('active', 'user_asserted')
            ON CONFLICT (entity_id) DO UPDATE SET claims = claims + 1;
        INSERT INTO entity_stats (entity_id, claims)
            SELECT new.object_entity, 1
            WHERE new.status IN ('active', 'user_asserted')
              AND new.object_entity IS NOT NULL AND new.object_entity <> new.subject
            ON CONFLICT (entity_id) DO UPDATE SET claims = claims + 1;
    END;
"#;

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
    // v4: knowledge graph (see `graph`).
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
    // v6: two-phase egress ledger (see `egress`).
    "",
    // v7: keyed audit chain (see `audit`).
    r#"
    INSERT OR IGNORE INTO meta (key, value)
        SELECT 'audit_v2_from', coalesce(max(seq), 0) + 1 FROM audit;
    "#,
    // v8: item text stored once.
    r#"
    CREATE TABLE IF NOT EXISTS item_text (
        item_id INTEGER PRIMARY KEY REFERENCES items (id) ON DELETE CASCADE,
        layout  BLOB NOT NULL
    );
    "#,
    // v9: egress rows record their owner, the store handle that wrote them (see `egress`), so
    // opening the store only marks `interrupted` the `open` rows whose owner is gone, not those of
    // another process (the daemon) that still has the database open.
    "",
    // v10: per-source item and chunk counts kept by triggers, so `Store::source_counts` reads a row
    // per source instead of aggregating every chunk (tens to hundreds of milliseconds at
    r#"
    CREATE TABLE IF NOT EXISTS source_stats (
        source TEXT PRIMARY KEY,
        items  INTEGER NOT NULL DEFAULT 0,
        chunks INTEGER NOT NULL DEFAULT 0
    ) WITHOUT ROWID;
    DELETE FROM source_stats;
    INSERT INTO source_stats (source, items, chunks)
        SELECT i.source, count(*), 0 FROM items i GROUP BY i.source;
    UPDATE source_stats SET chunks = (
        SELECT count(*) FROM chunks c JOIN items i ON i.id = c.item_id
        WHERE i.source = source_stats.source
    );
    CREATE TRIGGER IF NOT EXISTS items_stats_ai AFTER INSERT ON items BEGIN
        INSERT INTO source_stats (source, items) VALUES (new.source, 1)
            ON CONFLICT (source) DO UPDATE SET items = items + 1;
    END;
    CREATE TRIGGER IF NOT EXISTS items_stats_bd BEFORE DELETE ON items BEGIN
        UPDATE source_stats
            SET items = items - 1,
                chunks = chunks - (SELECT count(*) FROM chunks WHERE item_id = old.id)
            WHERE source = old.source;
    END;
    CREATE TRIGGER IF NOT EXISTS items_stats_au AFTER UPDATE OF source ON items
        WHEN old.source IS NOT new.source BEGIN
        UPDATE source_stats
            SET items = items - 1,
                chunks = chunks - (SELECT count(*) FROM chunks WHERE item_id = new.id)
            WHERE source = old.source;
        INSERT INTO source_stats (source, items, chunks)
            VALUES (new.source, 1, (SELECT count(*) FROM chunks WHERE item_id = new.id))
            ON CONFLICT (source) DO UPDATE
                SET items = items + 1, chunks = chunks + excluded.chunks;
    END;
    CREATE TRIGGER IF NOT EXISTS chunks_stats_ai AFTER INSERT ON chunks BEGIN
        UPDATE source_stats SET chunks = chunks + 1
            WHERE source = (SELECT source FROM items WHERE id = new.item_id);
    END;
    CREATE TRIGGER IF NOT EXISTS chunks_stats_ad AFTER DELETE ON chunks BEGIN
        UPDATE source_stats SET chunks = chunks - 1
            WHERE source = (SELECT source FROM items WHERE id = old.item_id);
    END;
    CREATE TRIGGER IF NOT EXISTS chunks_stats_au AFTER UPDATE OF item_id ON chunks
        WHEN old.item_id IS NOT new.item_id BEGIN
        UPDATE source_stats SET chunks = chunks - 1
            WHERE source = (SELECT source FROM items WHERE id = old.item_id);
        UPDATE source_stats SET chunks = chunks + 1
            WHERE source = (SELECT source FROM items WHERE id = new.item_id);
    END;
    "#,
    // v11: CJK and Thai text is indexed as overlapping bigrams (see `fts::segment`), so
    // `chunks_fts` and `items_fts` hold `fts_segment(text)` while their content tables keep the
    // original text.
    r#"
    DROP TRIGGER IF EXISTS chunks_fts_ai;
    DROP TRIGGER IF EXISTS chunks_fts_ad;
    DROP TRIGGER IF EXISTS chunks_fts_au;
    CREATE TRIGGER chunks_fts_ai AFTER INSERT ON chunks BEGIN
        INSERT INTO chunks_fts (rowid, text) VALUES (new.id, fts_segment(new.text));
    END;
    CREATE TRIGGER chunks_fts_ad AFTER DELETE ON chunks BEGIN
        INSERT INTO chunks_fts (chunks_fts, rowid, text)
            VALUES ('delete', old.id, fts_segment(old.text));
    END;
    CREATE TRIGGER chunks_fts_au AFTER UPDATE OF text ON chunks BEGIN
        INSERT INTO chunks_fts (chunks_fts, rowid, text)
            VALUES ('delete', old.id, fts_segment(old.text));
        INSERT INTO chunks_fts (rowid, text) VALUES (new.id, fts_segment(new.text));
    END;
    INSERT INTO chunks_fts (chunks_fts, rowid, text)
        SELECT 'delete', id, text FROM chunks WHERE fts_segment(text) IS NOT text;
    INSERT INTO chunks_fts (rowid, text)
        SELECT id, fts_segment(text) FROM chunks WHERE fts_segment(text) IS NOT text;

    DROP TRIGGER IF EXISTS items_fts_ai;
    DROP TRIGGER IF EXISTS items_fts_ad;
    DROP TRIGGER IF EXISTS items_fts_au;
    CREATE TRIGGER items_fts_ai AFTER INSERT ON items BEGIN
        INSERT INTO items_fts (rowid, title) VALUES (new.id, fts_segment(coalesce(new.title, '')));
    END;
    CREATE TRIGGER items_fts_ad AFTER DELETE ON items BEGIN
        INSERT INTO items_fts (items_fts, rowid, title)
            VALUES ('delete', old.id, fts_segment(coalesce(old.title, '')));
    END;
    CREATE TRIGGER items_fts_au AFTER UPDATE OF title ON items
        WHEN old.title IS NOT new.title BEGIN
        INSERT INTO items_fts (items_fts, rowid, title)
            VALUES ('delete', old.id, fts_segment(coalesce(old.title, '')));
        INSERT INTO items_fts (rowid, title)
            VALUES (new.id, fts_segment(coalesce(new.title, '')));
    END;
    INSERT INTO items_fts (items_fts, rowid, title)
        SELECT 'delete', id, title FROM items
        WHERE title IS NOT NULL AND fts_segment(title) IS NOT title;
    INSERT INTO items_fts (rowid, title)
        SELECT id, fts_segment(title) FROM items
        WHERE title IS NOT NULL AND fts_segment(title) IS NOT title;
    "#,
    // v12: user decisions on claims that must outlive re-extraction (see `USER_DECISIONS`).
    USER_DECISIONS,
    // v13: the same, idempotently, for development stores that ran an earlier v12 (which made
    // an obsolete `claim_user_replaced` table instead).
    USER_DECISIONS,
    // v14: spelling variants (see `fts::index_form`): the FTS tables hold `fts_normalize(text)`,
    // which folds width, Arabic and ß spellings and adds variant forms of some words.
    r#"
    CREATE VIRTUAL TABLE IF NOT EXISTS chunks_rev USING fts5 (
        text,
        content = '',
        contentless_delete = 1,
        tokenize = 'unicode61 remove_diacritics 2 categories ''L* N* Co M*'''
    );
    DROP TRIGGER IF EXISTS chunks_fts_ai;
    DROP TRIGGER IF EXISTS chunks_fts_ad;
    DROP TRIGGER IF EXISTS chunks_fts_au;
    CREATE TRIGGER chunks_fts_ai AFTER INSERT ON chunks BEGIN
        INSERT INTO chunks_fts (rowid, text) VALUES (new.id, fts_normalize(new.text));
        INSERT INTO chunks_rev (rowid, text)
            SELECT new.id, r FROM (SELECT fts_reversed(new.text) AS r) WHERE r <> '';
    END;
    CREATE TRIGGER chunks_fts_ad AFTER DELETE ON chunks BEGIN
        INSERT INTO chunks_fts (chunks_fts, rowid, text)
            VALUES ('delete', old.id, fts_normalize(old.text));
        DELETE FROM chunks_rev WHERE rowid = old.id;
    END;
    CREATE TRIGGER chunks_fts_au AFTER UPDATE OF text ON chunks BEGIN
        INSERT INTO chunks_fts (chunks_fts, rowid, text)
            VALUES ('delete', old.id, fts_normalize(old.text));
        INSERT INTO chunks_fts (rowid, text) VALUES (new.id, fts_normalize(new.text));
        DELETE FROM chunks_rev WHERE rowid = old.id;
        INSERT INTO chunks_rev (rowid, text)
            SELECT new.id, r FROM (SELECT fts_reversed(new.text) AS r) WHERE r <> '';
    END;
    INSERT INTO chunks_fts (chunks_fts, rowid, text)
        SELECT 'delete', id, fts_segment(text) FROM chunks
        WHERE fts_normalize(text) IS NOT fts_segment(text);
    INSERT INTO chunks_fts (rowid, text)
        SELECT id, fts_normalize(text) FROM chunks
        WHERE fts_normalize(text) IS NOT fts_segment(text);
    DELETE FROM chunks_rev;
    INSERT INTO chunks_rev (rowid, text)
        SELECT id, r FROM (SELECT id, fts_reversed(text) AS r FROM chunks) WHERE r <> '';

    DROP TRIGGER IF EXISTS items_fts_ai;
    DROP TRIGGER IF EXISTS items_fts_ad;
    DROP TRIGGER IF EXISTS items_fts_au;
    CREATE TRIGGER items_fts_ai AFTER INSERT ON items BEGIN
        INSERT INTO items_fts (rowid, title)
            VALUES (new.id, fts_normalize(coalesce(new.title, '')));
    END;
    CREATE TRIGGER items_fts_ad AFTER DELETE ON items BEGIN
        INSERT INTO items_fts (items_fts, rowid, title)
            VALUES ('delete', old.id, fts_normalize(coalesce(old.title, '')));
    END;
    CREATE TRIGGER items_fts_au AFTER UPDATE OF title ON items
        WHEN old.title IS NOT new.title BEGIN
        INSERT INTO items_fts (items_fts, rowid, title)
            VALUES ('delete', old.id, fts_normalize(coalesce(old.title, '')));
        INSERT INTO items_fts (rowid, title)
            VALUES (new.id, fts_normalize(coalesce(new.title, '')));
    END;
    INSERT INTO items_fts (items_fts, rowid, title)
        SELECT 'delete', id, fts_segment(title) FROM items
        WHERE title IS NOT NULL AND fts_normalize(title) IS NOT fts_segment(title);
    INSERT INTO items_fts (rowid, title)
        SELECT id, fts_normalize(title) FROM items
        WHERE title IS NOT NULL AND fts_normalize(title) IS NOT fts_segment(title);
    "#,
    // v15: per-entity claim and mention counts (see `ENTITY_STATS`).
    ENTITY_STATS,
];

/// Columns a migration adds, as `(version, table, column, type)`.
const ADDED_COLUMNS: &[(u32, &str, &str, &str)] = &[
    (5, "entities", "deleted_at", "TEXT"),
    (
        6,
        "egress",
        "state",
        "TEXT NOT NULL DEFAULT 'done' CHECK (state IN ('open', 'done', 'interrupted'))",
    ),
    (9, "egress", "owner", "TEXT"),
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

/// Bring the schema up to [`LATEST_VERSION`].
pub(crate) fn migrate(conn: &mut Connection) -> Result<u32> {
    // The FTS triggers and migrations call the `fts_*` functions; register them even where the
    // auto-extension (see `vec_ext`) has not been.
    crate::fts::register_functions(conn).db("register SQL functions")?;
    let current = user_version(conn)?;
    if current > LATEST_VERSION {
        return Err(Error::Unsupported(format!(
            "store schema version {current} is newer than this build supports ({LATEST_VERSION}); \
             upgrade Heed"
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
    use nugget_core::ids::{EntityId, ItemKey, ItemRef, SourceId};
    use nugget_graph::{ClaimObject, ClaimStatus, GraphStore, NewClaim};

    use super::*;
    use crate::{Store, StoreKey};

    fn schema_snapshot(conn: &Connection) -> Vec<(String, String)> {
        let mut stmt = conn
            .prepare("SELECT name, IFNULL(sql, '') FROM sqlite_master ORDER BY name")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn rows<T: rusqlite::types::FromSql>(conn: &Connection, sql: &str) -> Vec<T> {
        let mut stmt = conn.prepare(sql).unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// Apply the migrations after the current version up to `to`, as an older build would.
    fn migrate_to(conn: &mut Connection, to: u32) {
        let from = user_version(conn).unwrap() as usize;
        for (idx, sql) in MIGRATIONS.iter().enumerate().take(to as usize).skip(from) {
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
    fn migrate_twice_is_a_noop_and_newer_schemas_are_refused() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        assert_eq!(migrate(&mut conn).unwrap(), LATEST_VERSION);
        let before = schema_snapshot(&conn);
        assert_eq!(migrate(&mut conn).unwrap(), 0);
        assert_eq!(schema_snapshot(&conn), before);
        conn.pragma_update(None, "user_version", LATEST_VERSION + 1)
            .unwrap();
        assert!(matches!(migrate(&mut conn), Err(Error::Unsupported(_))));
    }

    /// An encrypted v2 store with graph rows added at v4 opens at the latest version with every
    /// backfill done (titles v3, claims rebuild v5, feed v5, counts v10/v15, CJK v11,
    /// assertions v12, variants v14).
    #[test]
    fn encrypted_v2_store_migrates_to_latest_keeping_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nugget.db");
        let key = [5u8; 32];
        {
            let mut conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex::encode(key)))
                .unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            crate::fts::register_functions(&conn).unwrap();
            migrate_to(&mut conn, 2);
            conn.execute_batch(
                "INSERT INTO items (id, source, key, title, content_hash, created_at, updated_at,
                                    version)
                     VALUES (1, 'mail', 'm1', 'Boiler repair notes', 'h1', 't', 't', 'v1'),
                            (2, 'mail', 'm2', '見積書の送付', 'h2', 't', 't', NULL),
                            (3, 'files', 'f1', 'Königstraße', 'h3', 't', 't', NULL);
                 INSERT INTO chunks (id, item_id, ord, text) VALUES
                     (10, 1, 0, 'gas valve'),
                     (20, 2, 0, '我们下周二在上海开会讨论预算'),
                     (30, 3, 0, 'Lenkungsausschuss in der Königstraße'),
                     (31, 3, 1, 'second');
                 INSERT INTO cursors (source, value, updated_at) VALUES ('mail', '{\"n\":1}', 't');",
            )
            .unwrap();
            migrate_to(&mut conn, 4);
            let t = "2026-01-01T00:00:00Z";
            conn.execute_batch(&format!(
                "INSERT INTO entities (id, kind, name, name_lower, created_at, updated_at)
                     VALUES ('ent_a', 'person', 'Sam', 'sam', '{t}', '{t}'),
                            ('ent_b', 'organization', 'Acme', 'acme', '{t}', '{t}');
                 INSERT INTO entity_aliases (entity_id, alias, alias_norm, alias_lower, is_name)
                     VALUES ('ent_a', 'Sam', 'sam', 'sam', 1);
                 INSERT INTO claims (id, subject, predicate, object_entity, object_literal, text,
                                     confidence, status, created_at, updated_at)
                     VALUES (1, 'ent_a', 'likes', NULL, 'tea', 'x', 0.8, 'active', '{t}', '{t}'),
                            (3, 'ent_a', 'works_at', 'ent_b', NULL, 'x', 1.0, 'user_asserted',
                             '{t}', '2026-02-01T00:00:00Z'),
                            (4, 'ent_a', 'likes', NULL, 'jazz', 'x', 0.7, 'superseded',
                             '{t}', '{t}');
                 UPDATE sqlite_sequence SET seq = 5 WHERE name = 'claims';
                 INSERT INTO claim_evidence (claim_id, source, key, chunk_id, quote, created_at)
                     VALUES (1, 'mail', 'm1', 10, 'tea please', '{t}');
                 INSERT INTO mentions (entity_id, source, key, surface, confidence, created_at)
                     VALUES ('ent_a', 'mail', 'm1', 'Sam', 1, '{t}'),
                            ('ent_a', 'mail', 'm2', 'Sam', 1, '{t}');"
            ))
            .unwrap();
        }
        assert!(
            !std::fs::read(&path)
                .unwrap()
                .starts_with(b"SQLite format 3")
        );

        let store = Store::open(&path, &StoreKey::from_bytes(key)).unwrap();
        assert_eq!(store.schema_version().unwrap(), LATEST_VERSION);
        let found = |q: &str| -> Vec<String> {
            let hits = store.search_text(q, 5).unwrap();
            hits.into_iter().map(|h| h.item.key.to_string()).collect()
        };
        assert_eq!(found("boiler"), ["m1"]);
        assert_eq!(found("上海"), ["m2"]);
        assert_eq!(found("見積書"), ["m2"]);
        assert_eq!(found("Koenigstrasse"), ["f1"]);
        assert_eq!(
            store.get_cursor(&SourceId::new("mail")).unwrap(),
            Some(serde_json::json!({"n": 1}))
        );
        let counts: Vec<(String, u64, u64)> = store
            .source_counts()
            .unwrap()
            .into_iter()
            .map(|c| (c.source.to_string(), c.items, c.chunks))
            .collect();
        assert_eq!(counts, [("files".into(), 1, 2), ("mail".into(), 2, 2)]);
        let (changes, head) = store.items_changed_since(0, 0).unwrap();
        let keys: Vec<&str> = changes.iter().map(|c| c.item.key.as_str()).collect();
        assert_eq!((keys, head), (vec!["m1", "m2", "f1"], 3));

        let g = store.graph();
        let tea = g.get_claim(1).unwrap().unwrap();
        assert_eq!(tea.evidence[0].quote.as_deref(), Some("tea please"));
        assert_eq!(
            g.get_claim(4).unwrap().unwrap().status,
            ClaimStatus::Superseded
        );
        g.set_claim_status(1, ClaimStatus::Rejected).unwrap();
        let next = g
            .add_claim(&NewClaim {
                subject: EntityId::new("ent_a"),
                predicate: "plays".into(),
                object: ClaimObject::Literal("chess".into()),
                text: "Sam plays chess".into(),
                confidence: 0.5,
                status: ClaimStatus::Active,
                evidence: vec![nugget_graph::Evidence {
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
        assert_eq!(next, 6, "claim ids are never reused across the rebuild");
        store
            .with_conn(|c| {
                let asserted: Vec<String> = rows(c, "SELECT asserted_at FROM claim_assertions");
                assert_eq!(asserted, ["2026-02-01T00:00:00Z"]);
                let stats: Vec<String> = rows(
                    c,
                    "SELECT entity_id || ':' || claims || ':' || mentions FROM entity_stats
                     ORDER BY entity_id",
                );
                assert_eq!(stats, ["ent_a:2:2", "ent_b:1:0"]);
                assert!(
                    c.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, bool>(0))
                        .db("fk")?
                );
                c.execute_batch(
                    "PRAGMA foreign_key_check;
                     INSERT INTO chunks_fts (chunks_fts) VALUES ('integrity-check');
                     INSERT INTO items_fts (items_fts) VALUES ('integrity-check');
                     INSERT INTO chunks_rev (chunks_rev) VALUES ('integrity-check');",
                )
                .db("integrity")
            })
            .unwrap();
        drop(g);
        drop(store);
        assert!(Store::open(&path, &StoreKey::from_bytes([6u8; 32])).is_err());
        let store = Store::open(&path, &StoreKey::from_bytes(key)).unwrap();
        assert_eq!(store.migrate().unwrap(), 0);
    }

    /// v13 repairs a store that ran an earlier v12 (an obsolete table, the new ones missing).
    #[test]
    fn an_early_v12_is_repaired() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::fts::register_functions(&conn).unwrap();
        migrate_to(&mut conn, 11);
        conn.execute_batch(
            "INSERT INTO entities (id, kind, name, name_lower, created_at, updated_at)
                 VALUES ('ent_a', 'person', 'Sam', 'sam', 't', 't');
             INSERT INTO claims (id, subject, predicate, object_literal, text, confidence,
                                 status, created_at, updated_at)
                 VALUES (1, 'ent_a', 'likes', 'tea', 'Sam likes tea', 1.0, 'user_asserted',
                         '2026-01-01T00:00:00Z', '2026-02-01T00:00:00Z');
             CREATE TABLE claim_user_replaced (claim_id INTEGER, source TEXT, key TEXT,
                                               quote TEXT);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 12).unwrap();
        assert_eq!(migrate(&mut conn).unwrap(), LATEST_VERSION - 12);
        let asserted: Vec<String> = rows(&conn, "SELECT asserted_at FROM claim_assertions");
        assert_eq!(asserted, ["2026-02-01T00:00:00Z"]);
        let names: Vec<String> = rows(&conn, "SELECT name FROM sqlite_master");
        assert!(!names.contains(&"claim_user_replaced".to_string()));
        for t in [
            "claim_replacements",
            "claim_replacement_evidence",
            "claim_assertions",
        ] {
            assert!(names.contains(&t.to_string()), "{t}");
        }
    }
}
