//! Behavioural tests for the public `nugget-store` API (see docs/m0-spec.md, WP-A).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use nugget_core::egress::{EgressEvent, EgressSink};
use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_store::{
    AuditEntry, Error, NewChunk, NewItem, SCHEMA_VERSION, Store, StoreKey, UpsertOutcome,
    content_hash,
};
use serde_json::json;
use time::OffsetDateTime;

fn mem() -> Store {
    Store::open_in_memory().expect("open in-memory store")
}

fn iref(source: &str, key: &str) -> ItemRef {
    ItemRef {
        source: SourceId::new(source),
        key: ItemKey::new(key),
    }
}

fn item(key: &str, title: &str, body: &str) -> NewItem {
    NewItem::new("files:test", key, content_hash(body))
        .with_title(title)
        .with_text(body)
}

fn key_a() -> StoreKey {
    StoreKey::from_bytes([7u8; 32])
}

fn db_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("data").join("nugget.db")
}

/// Open the encrypted file with plain rusqlite, as an attacker holding the key could.
fn raw_conn(path: &Path, key: &[u8; 32]) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex(key)))
        .unwrap();
    conn
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------- items, dedup, cascade

#[test]
fn crud_and_dedup_by_hash() {
    let s = mem();
    let a = item("a.txt", "A", "alpha body");
    let r = a.item.clone();

    assert_eq!(s.get_item(&r).unwrap(), None);
    assert_eq!(s.upsert_item(&a).unwrap(), UpsertOutcome::Inserted);
    assert_eq!(s.upsert_item(&a).unwrap(), UpsertOutcome::Unchanged);
    assert_eq!(s.counts().unwrap().items, 1);

    let got = s.get_item(&r).unwrap().expect("item exists");
    assert_eq!(got.item, r);
    assert_eq!(got.title.as_deref(), Some("A"));
    assert_eq!(got.content_hash, content_hash("alpha body"));
    assert_eq!(got.text.as_deref(), Some("alpha body"));
    assert_eq!(got.metadata, json!({}));
    assert_eq!(got.created_at, got.updated_at);

    // Same hash but changed fields: updated, chunks kept (content did not change).
    s.replace_chunks(&r, &[NewChunk::text("alpha body")])
        .unwrap();
    let retitled = a
        .clone()
        .with_title("A2")
        .with_metadata(json!({"size": 10}));
    assert_eq!(s.upsert_item(&retitled).unwrap(), UpsertOutcome::Updated);
    assert_eq!(s.upsert_item(&retitled).unwrap(), UpsertOutcome::Unchanged);
    let got = s.get_item(&r).unwrap().unwrap();
    assert_eq!(got.title.as_deref(), Some("A2"));
    assert_eq!(got.metadata, json!({"size": 10}));
    assert_eq!(s.counts().unwrap().chunks, 1);

    // New content hash: updated, and stale chunks are dropped.
    let edited = item("a.txt", "A2", "alpha edited").with_metadata(json!({"size": 10}));
    assert_eq!(s.upsert_item(&edited).unwrap(), UpsertOutcome::Updated);
    assert_eq!(s.counts().unwrap().chunks, 0);
    assert!(s.search_text("body", 10).unwrap().is_empty());
    let got = s.get_item(&r).unwrap().unwrap();
    assert_eq!(got.content_hash, content_hash("alpha edited"));

    // Same key in another source is a different item.
    let other = NewItem::new("gmail:me", "a.txt", content_hash("alpha edited"));
    assert_eq!(s.upsert_item(&other).unwrap(), UpsertOutcome::Inserted);
    assert_eq!(s.counts().unwrap().items, 2);

    // Timestamps from the source round-trip and count as a change.
    let ts = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
    let mut dated = other.clone();
    dated.source_modified_at = Some(ts);
    assert_eq!(s.upsert_item(&dated).unwrap(), UpsertOutcome::Updated);
    assert_eq!(s.upsert_item(&dated).unwrap(), UpsertOutcome::Unchanged);
    assert_eq!(
        s.get_item(&dated.item).unwrap().unwrap().source_modified_at,
        Some(ts)
    );
}

#[test]
fn upsert_validates_input() {
    let s = mem();
    assert!(matches!(
        s.upsert_item(&NewItem::new("files:x", "k", "")),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        s.upsert_item(&NewItem::new("", "k", "h")),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        s.replace_chunks(&iref("files:x", "missing"), &[NewChunk::text("t")]),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn delete_cascades_to_chunks_fts_and_vectors() {
    let s = mem();
    let a = item("a", "A", "kiwi");
    let b = item("b", "B", "kiwi too");
    s.upsert_item(&a).unwrap();
    s.upsert_item(&b).unwrap();
    s.replace_chunks(
        &a.item,
        &[
            NewChunk::embedded("kiwi one", vec![1.0, 0.0, 0.0]),
            NewChunk::embedded("kiwi two", vec![0.9, 0.1, 0.0]),
        ],
    )
    .unwrap();
    s.replace_chunks(
        &b.item,
        &[NewChunk::embedded("kiwi three", vec![0.0, 1.0, 0.0])],
    )
    .unwrap();
    assert_eq!(s.counts().unwrap().chunks, 3);
    assert_eq!(s.search_text("kiwi", 10).unwrap().len(), 3);
    assert_eq!(s.search_vector(&[1.0, 0.0, 0.0], 10).unwrap().len(), 3);

    assert!(s.delete_item(&a.item).unwrap());
    assert!(!s.delete_item(&a.item).unwrap(), "second delete is a no-op");
    assert_eq!(s.get_item(&a.item).unwrap(), None);

    let counts = s.counts().unwrap();
    assert_eq!((counts.items, counts.chunks), (1, 1));
    let text = s.search_text("kiwi", 10).unwrap();
    assert_eq!(text.len(), 1);
    assert_eq!(text[0].item, b.item);
    let vec = s.search_vector(&[1.0, 0.0, 0.0], 10).unwrap();
    assert_eq!(vec.len(), 1, "vectors of deleted chunks are gone");
    assert_eq!(vec[0].item, b.item);
}

#[test]
fn replace_chunks_replaces_and_is_atomic() {
    let s = mem();
    let a = item("a", "A", "x");
    s.upsert_item(&a).unwrap();
    let first = s
        .replace_chunks(
            &a.item,
            &[NewChunk::text("apple"), NewChunk::text("banana")],
        )
        .unwrap();
    assert_eq!(first.len(), 2);
    let second = s
        .replace_chunks(&a.item, &[NewChunk::text("cherry")])
        .unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(s.counts().unwrap().chunks, 1);
    assert!(s.search_text("apple", 5).unwrap().is_empty());
    assert_eq!(s.search_text("cherry", 5).unwrap()[0].chunk_id, second[0]);

    // A bad batch (NaN, mixed dimensions) changes nothing.
    for bad in [
        vec![NewChunk::embedded("d", vec![f32::NAN, 1.0])],
        vec![
            NewChunk::embedded("d", vec![1.0, 0.0]),
            NewChunk::embedded("e", vec![1.0, 0.0, 0.0]),
        ],
        vec![NewChunk::embedded("d", vec![])],
    ] {
        assert!(matches!(
            s.replace_chunks(&a.item, &bad),
            Err(Error::Invalid(_))
        ));
    }
    assert_eq!(s.search_text("cherry", 5).unwrap().len(), 1);
    assert_eq!(s.embedding_dim().unwrap(), None);
}

// ---------------------------------------------------------------- search

#[test]
fn fts_ranking_prefers_better_matches() {
    let s = mem();
    let docs = [
        (
            "weak",
            "the garden has a fence and some flowers near the shed",
        ),
        (
            "strong",
            "rust ownership rules: rust borrows, rust lifetimes and rust traits",
        ),
        ("medium", "notes about rust and gardening"),
    ];
    for (key, body) in docs {
        let it = item(key, &format!("title {key}"), body);
        s.upsert_item(&it).unwrap();
        s.replace_chunks(&it.item, &[NewChunk::text(body)]).unwrap();
    }

    let hits = s.search_text("rust", 10).unwrap();
    let keys: Vec<_> = hits.iter().map(|h| h.item.key.as_str()).collect();
    assert_eq!(keys, ["strong", "medium"]);
    assert!(hits[0].score > hits[1].score);
    assert!(hits[0].snippet.contains("rust"));
    assert_eq!(hits[0].title.as_deref(), Some("title strong"));

    // Stemming: "gardens" matches "garden" and "gardening".
    let stemmed = s.search_text("gardens", 10).unwrap();
    assert_eq!(stemmed.len(), 2);

    // More matching terms rank higher.
    let multi = s.search_text("rust gardening", 10).unwrap();
    assert_eq!(multi[0].item.key.as_str(), "medium");

    // FTS5 syntax in user input is inert; empty input and limit 0 yield nothing.
    assert_eq!(
        s.search_text("rust\" -(xyzzy* ^", 10).unwrap()[0]
            .item
            .key
            .as_str(),
        "strong"
    );
    assert!(s.search_text("  ?! ", 10).unwrap().is_empty());
    assert!(s.search_text("rust", 0).unwrap().is_empty());
    assert_eq!(s.search_text("rust", 1).unwrap().len(), 1);
}

#[test]
fn vector_knn_orders_by_cosine_similarity() {
    let s = mem();
    let vectors = [
        ("east", vec![1.0, 0.0, 0.0]),
        ("north-east", vec![0.7, 0.7, 0.0]),
        ("north", vec![0.0, 1.0, 0.0]),
        ("west", vec![-1.0, 0.0, 0.0]),
    ];
    for (key, v) in &vectors {
        let it = item(key, key, key);
        s.upsert_item(&it).unwrap();
        s.replace_chunks(&it.item, &[NewChunk::embedded(*key, v.clone())])
            .unwrap();
    }
    assert_eq!(s.embedding_dim().unwrap(), Some(3));

    // Magnitude does not matter for cosine.
    let hits = s.search_vector(&[10.0, 1.0, 0.0], 3).unwrap();
    let keys: Vec<_> = hits.iter().map(|h| h.item.key.as_str()).collect();
    assert_eq!(keys, ["east", "north-east", "north"]);
    assert!(hits.windows(2).all(|w| w[0].score >= w[1].score));
    assert!(hits[0].score > 0.99 && hits[0].score <= 1.0001);
    assert_eq!(hits[0].snippet, "east");

    let all = s.search_vector(&[1.0, 0.0, 0.0], 100).unwrap();
    assert_eq!(all.last().unwrap().item.key.as_str(), "west");
    assert!(all.last().unwrap().score < -0.99);

    // Dimension is fixed per database.
    assert!(matches!(
        s.search_vector(&[1.0, 0.0], 3),
        Err(Error::Invalid(_))
    ));
    let x = item("x", "x", "x");
    s.upsert_item(&x).unwrap();
    assert!(matches!(
        s.replace_chunks(&x.item, &[NewChunk::embedded("x", vec![1.0; 4])]),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn vector_search_on_empty_store_is_empty() {
    let s = mem();
    assert!(s.search_vector(&[1.0, 2.0], 5).unwrap().is_empty());
    assert!(matches!(s.search_vector(&[], 5), Err(Error::Invalid(_))));
}

#[test]
fn hybrid_fusion_rewards_agreement() {
    let s = mem();
    // "text": best keyword match, opposite vector.
    // "vector": no keyword, best vector match.
    // "both": decent on both lists, so RRF puts it first.
    let docs = [
        (
            "text",
            "invoice invoice invoice payment invoice due",
            vec![-1.0, 0.0, 0.0],
        ),
        (
            "vector",
            "a note about quarterly planning",
            vec![1.0, 0.0, 0.0],
        ),
        (
            "both",
            "the invoice from the vendor arrived",
            vec![0.9, 0.3, 0.0],
        ),
        ("noise1", "lunch menu for friday", vec![0.0, 1.0, 0.0]),
        ("noise2", "gym schedule", vec![0.3, 0.3, 0.9]),
        ("noise3", "flight check-in reminder", vec![0.2, 0.9, 0.1]),
    ];
    for (key, body, v) in &docs {
        let it = item(key, key, body);
        s.upsert_item(&it).unwrap();
        s.replace_chunks(&it.item, &[NewChunk::embedded(*body, v.clone())])
            .unwrap();
    }
    let q = [1.0, 0.0, 0.0];

    let text_only: Vec<_> = s
        .search_text("invoice", 10)
        .unwrap()
        .into_iter()
        .map(|h| h.item.key.0)
        .collect();
    assert_eq!(text_only, ["text", "both"]);
    let vec_only: Vec<_> = s
        .search_vector(&q, 10)
        .unwrap()
        .into_iter()
        .map(|h| h.item.key.0)
        .collect();
    assert_eq!(&vec_only[..2], ["vector", "both"]);

    let hybrid = s.search_hybrid("invoice", Some(&q), 10).unwrap();
    let keys: Vec<_> = hybrid.iter().map(|h| h.item.key.as_str()).collect();
    assert_eq!(keys[0], "both", "{keys:?}");
    assert_eq!(keys.len(), docs.len());
    let expected = 1.0 / 62.0 + 1.0 / 62.0;
    assert!((hybrid[0].score - expected).abs() < 1e-6);
    assert!(hybrid.windows(2).all(|w| w[0].score >= w[1].score));
    // The full-text snippet is kept for chunks found by both.
    assert!(hybrid[0].snippet.contains("invoice"));

    // Without an embedding, hybrid degrades to the text ranking.
    let text_rrf: Vec<_> = s
        .search_hybrid("invoice", None, 10)
        .unwrap()
        .into_iter()
        .map(|h| h.item.key.0)
        .collect();
    assert_eq!(text_rrf, ["text", "both"]);
    assert_eq!(s.search_hybrid("invoice", Some(&q), 1).unwrap().len(), 1);
    assert!(s.search_hybrid("invoice", Some(&q), 0).unwrap().is_empty());
    assert!(matches!(
        s.search_hybrid("invoice", Some(&[1.0]), 5),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn hit_converts_to_ipc_search_hit() {
    let s = mem();
    let it = item("a", "Title", "hello world");
    s.upsert_item(&it).unwrap();
    s.replace_chunks(&it.item, &[NewChunk::text("hello world")])
        .unwrap();
    let hit = s.search_text("hello", 1).unwrap().remove(0);
    let ipc: nugget_core::ipc::SearchHit = hit.clone().into();
    assert_eq!(ipc.item, hit.item);
    assert_eq!(ipc.title.as_deref(), Some("Title"));
    assert_eq!(ipc.snippet, "hello world");
}

// ---------------------------------------------------------------- cursors, counts

#[test]
fn cursor_round_trip() {
    let s = mem();
    let src = SourceId::new("gmail:me@example.com");
    assert_eq!(s.get_cursor(&src).unwrap(), None);
    let v1 = json!({"history_id": 42, "pages": ["a", "b"]});
    s.set_cursor(&src, v1.clone()).unwrap();
    assert_eq!(s.get_cursor(&src).unwrap(), Some(v1));
    let v2 = json!("opaque-token");
    s.set_cursor(&src, v2.clone()).unwrap();
    assert_eq!(s.get_cursor(&src).unwrap(), Some(v2));
    assert_eq!(s.get_cursor(&SourceId::new("other")).unwrap(), None);
}

// ---------------------------------------------------------------- egress ledger

#[test]
fn egress_ledger_records_via_sink() {
    let s = mem();
    let sink: Arc<dyn EgressSink> = Arc::new(s.clone());
    let at = OffsetDateTime::from_unix_timestamp(1_750_000_000).unwrap();
    for i in 0..5u64 {
        sink.record(EgressEvent {
            at,
            host: format!("api{i}.example.com"),
            feature: "chat".into(),
            bytes_sent: 100 + i,
            bytes_received: u64::MAX,
            items: vec![iref("files:x", &format!("doc{i}"))],
        });
    }
    let recent = s.recent_egress(3).unwrap();
    assert_eq!(recent.len(), 3);
    assert_eq!(recent[0].host, "api4.example.com", "newest first");
    assert_eq!(recent[0].bytes_sent, 104);
    assert_eq!(
        recent[0].bytes_received,
        i64::MAX as u64,
        "clamped, not wrapped"
    );
    assert_eq!(recent[0].items, vec![iref("files:x", "doc4")]);
    assert_eq!(recent[0].at, at);
    assert_eq!(s.recent_egress(100).unwrap().len(), 5);
    assert!(s.recent_egress(0).unwrap().is_empty());
}

// ---------------------------------------------------------------- audit log

fn audit(i: u32) -> AuditEntry {
    AuditEntry {
        actor: "agent".into(),
        action: "item.delete".into(),
        target: format!("files:x/doc{i}"),
        detail: json!({"i": i}),
    }
}

#[test]
fn audit_chain_links_entries() {
    let s = mem();
    s.verify_audit_chain().unwrap();
    let r1 = s.append_audit(&audit(1)).unwrap();
    let r2 = s.append_audit(&audit(2)).unwrap();
    assert_eq!((r1.seq, r2.seq), (1, 2));
    assert_eq!(r1.prev_hash, "0".repeat(64));
    assert_eq!(r2.prev_hash, r1.hash);
    assert_ne!(r1.hash, r2.hash);
    s.verify_audit_chain().unwrap();

    let recent = s.recent_audit(10).unwrap();
    assert_eq!(recent, vec![r2.clone(), r1]);
    assert_eq!(recent[0].detail, json!({"i": 2}));
    assert!(matches!(
        s.append_audit(&AuditEntry {
            actor: String::new(),
            ..audit(3)
        }),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn audit_chain_detects_tampered_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    let s = Store::open(&path, &key_a()).unwrap();
    for i in 1..=3 {
        s.append_audit(&audit(i)).unwrap();
    }
    s.verify_audit_chain().unwrap();

    let raw = raw_conn(&path, &[7u8; 32]);
    // The table is append-only...
    assert!(
        raw.execute("UPDATE audit SET detail = '{}' WHERE seq = 2", [])
            .is_err()
    );
    // ...but someone holding the key can drop the guard; the chain still catches it.
    raw.execute_batch(
        "DROP TRIGGER audit_no_update;
         UPDATE audit SET detail = '{\"i\":99}' WHERE seq = 2;",
    )
    .unwrap();
    let err = s.verify_audit_chain().unwrap_err();
    assert!(
        matches!(&err, Error::Integrity(m) if m.contains("entry 2")),
        "{err}"
    );
}

#[test]
fn audit_chain_detects_removed_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    let s = Store::open(&path, &key_a()).unwrap();
    for i in 1..=4 {
        s.append_audit(&audit(i)).unwrap();
    }
    let raw = raw_conn(&path, &[7u8; 32]);
    raw.execute_batch("DROP TRIGGER audit_no_delete;").unwrap();

    // Removing the tail is caught by the stored chain head.
    raw.execute("DELETE FROM audit WHERE seq = 4", []).unwrap();
    assert!(matches!(
        s.verify_audit_chain(),
        Err(Error::Integrity(m)) if m.contains("head")
    ));
    // Removing a middle entry breaks sequence continuity.
    raw.execute("DELETE FROM audit WHERE seq = 2", []).unwrap();
    assert!(matches!(
        s.verify_audit_chain(),
        Err(Error::Integrity(m)) if m.contains("expected entry 2")
    ));
}

// ---------------------------------------------------------------- encryption, migrations

#[test]
fn encrypted_db_rejects_wrong_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    {
        let s = Store::open(&path, &key_a()).unwrap();
        assert!(s.is_encrypted());
        let it = item("secret", "Secret", "the launch code is swordfish");
        s.upsert_item(&it).unwrap();
        s.replace_chunks(&it.item, &[NewChunk::text("the launch code is swordfish")])
            .unwrap();
    }

    // Nothing in plaintext on disk (main file and WAL).
    let header = std::fs::read(&path).unwrap();
    assert!(
        !header.starts_with(b"SQLite format 3\0"),
        "file is not encrypted"
    );
    for suffix in ["", "-wal"] {
        let mut p = path.clone().into_os_string();
        p.push(suffix);
        if let Ok(bytes) = std::fs::read(&p) {
            assert!(
                !bytes.windows(9).any(|w| w == b"swordfish"),
                "plaintext found in {p:?}"
            );
        }
    }

    let wrong = StoreKey::from_bytes([8u8; 32]);
    let err = Store::open(&path, &wrong).unwrap_err();
    assert!(matches!(err, Error::Integrity(_)), "{err}");

    let s = Store::open(&path, &key_a()).unwrap();
    assert_eq!(s.search_text("swordfish", 5).unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn database_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    let _s = Store::open(&path, &key_a()).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let dir_mode = std::fs::metadata(path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(dir_mode & 0o777, 0o700);
}

#[test]
fn migration_rerun_is_a_noop() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    {
        let s = Store::open(&path, &key_a()).unwrap();
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(s.migrate().unwrap(), 0);
        let it = item("a", "A", "persisted");
        s.upsert_item(&it).unwrap();
        s.replace_chunks(&it.item, &[NewChunk::embedded("persisted", vec![1.0, 0.0])])
            .unwrap();
        s.append_audit(&audit(1)).unwrap();
    }
    let s = Store::open(&path, &key_a()).unwrap();
    assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(s.migrate().unwrap(), 0);
    assert_eq!(s.counts().unwrap().items, 1);
    assert_eq!(s.embedding_dim().unwrap(), Some(2));
    assert_eq!(s.search_vector(&[1.0, 0.0], 1).unwrap().len(), 1);
    s.verify_audit_chain().unwrap();
}

#[test]
fn newer_schema_version_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    drop(Store::open(&path, &key_a()).unwrap());
    raw_conn(&path, &[7u8; 32])
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    assert!(matches!(
        Store::open(&path, &key_a()),
        Err(Error::Unsupported(_))
    ));
}

// ---------------------------------------------------------------- concurrency

#[test]
fn store_is_shareable_across_threads() {
    fn assert_traits<T: Clone + Send + Sync + 'static>() {}
    assert_traits::<Store>();

    let s = mem();
    let handles: Vec<_> = (0..4)
        .map(|t| {
            let s = s.clone();
            std::thread::spawn(move || {
                for i in 0..25 {
                    let it = item(&format!("t{t}-{i}"), "t", &format!("thread {t} item {i}"));
                    s.upsert_item(&it).unwrap();
                    s.replace_chunks(&it.item, &[NewChunk::text("shared words")])
                        .unwrap();
                    s.append_audit(&audit(i)).unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(s.counts().unwrap().items, 100);
    assert_eq!(s.search_text("shared", 1000).unwrap().len(), 100);
    s.verify_audit_chain().unwrap();
}

/// Events queued through the sink are all written before the last `Store` handle is gone.
#[test]
fn egress_events_survive_dropping_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    {
        let s = Store::open(&path, &key_a()).unwrap();
        let sink: Arc<dyn EgressSink> = Arc::new(s.clone());
        drop(s);
        for i in 0..200u64 {
            sink.record(EgressEvent {
                at: OffsetDateTime::from_unix_timestamp(1_750_000_000).unwrap(),
                host: "api.example.com".into(),
                feature: "chat".into(),
                bytes_sent: i,
                bytes_received: 0,
                items: vec![],
            });
        }
        // The last handle: dropping it must flush the queue and join the writer.
        drop(sink);
    }
    let s = Store::open(&path, &key_a()).unwrap();
    let got = s.recent_egress(1000).unwrap();
    assert_eq!(got.len(), 200);
    assert_eq!(got[0].bytes_sent, 199);
}

mod sources {
    use nugget_core::ids::{ItemKey, SourceId};
    use nugget_store::{NewChunk, NewItem, Store};

    fn put(store: &Store, source: &str, key: &str, version: Option<&str>, chunks: &[&str]) {
        let mut item = NewItem::new(source, key, format!("{source}/{key}/{version:?}"))
            .with_title(key.to_uppercase());
        if let Some(v) = version {
            item = item.with_version(v);
        }
        store.upsert_item(&item).unwrap();
        let chunks: Vec<_> = chunks.iter().map(|t| NewChunk::text(*t)).collect();
        store.replace_chunks(&item.item, &chunks).unwrap();
    }

    #[test]
    fn items_missing_embeddings_lists_text_only_items_in_pages() {
        let store = Store::open_in_memory().unwrap();
        // No vector table yet: every item with chunks is listed; chunkless items never are.
        put(&store, "b", "two", None, &["beta"]);
        put(&store, "a", "one", None, &["alpha", "alpha 2"]);
        put(&store, "a", "empty", None, &[]);
        let r = |s: &str, k: &str| nugget_core::ids::ItemRef {
            source: SourceId::new(s),
            key: ItemKey::new(k),
        };
        assert_eq!(
            store.items_missing_embeddings(None, 10).unwrap(),
            vec![r("a", "one"), r("b", "two")]
        );
        // Embed one item: it drops out; pagination continues after the given item.
        store
            .replace_chunks(
                &r("a", "one"),
                &[
                    NewChunk::embedded("alpha", vec![1.0, 0.0]),
                    NewChunk::embedded("alpha 2", vec![0.0, 1.0]),
                ],
            )
            .unwrap();
        put(&store, "a", "three", None, &["gamma"]);
        assert_eq!(
            store.items_missing_embeddings(None, 1).unwrap(),
            vec![r("a", "three")]
        );
        assert_eq!(
            store
                .items_missing_embeddings(Some(&r("a", "three")), 10)
                .unwrap(),
            vec![r("b", "two")]
        );
        assert!(store.items_missing_embeddings(None, 0).unwrap().is_empty());
        // A partially embedded item is listed too.
        let mixed = r("c", "mixed");
        store.upsert_item(&NewItem::new("c", "mixed", "h")).unwrap();
        store
            .replace_chunks(
                &mixed,
                &[NewChunk::embedded("x", vec![1.0, 1.0]), NewChunk::text("y")],
            )
            .unwrap();
        assert!(
            store
                .items_missing_embeddings(Some(&r("b", "two")), 10)
                .unwrap()
                .contains(&mixed)
        );
    }

    #[test]
    fn versions_are_stored_and_listed_per_source() {
        let store = Store::open_in_memory().unwrap();
        put(&store, "files", "a.md", Some("1:10"), &["alpha"]);
        put(&store, "files", "b.md", None, &["beta"]);
        put(&store, "mail", "m1", Some("h7"), &["gamma"]);

        let v = store.item_versions(&SourceId::new("files")).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[&ItemKey::new("a.md")].as_deref(), Some("1:10"));
        assert_eq!(v[&ItemKey::new("b.md")], None);

        // A version change alone is a real update.
        let item = NewItem::new("files", "a.md", "files/a.md/Some(\"1:10\")")
            .with_title("A.MD")
            .with_version("2:10");
        assert_eq!(
            store.upsert_item(&item).unwrap(),
            nugget_store::UpsertOutcome::Updated
        );
        let got = store.get_item(&item.item).unwrap().unwrap();
        assert_eq!(got.version.as_deref(), Some("2:10"));
    }

    #[test]
    fn delete_source_removes_items_chunks_and_cursor_only_for_that_source() {
        let store = Store::open_in_memory().unwrap();
        put(&store, "files", "a.md", None, &["needle one", "two"]);
        put(&store, "mail", "m1", None, &["needle three"]);
        store
            .set_cursor(&SourceId::new("files"), serde_json::json!({"x": 1}))
            .unwrap();

        assert_eq!(store.delete_source(&SourceId::new("files")).unwrap(), 1);
        assert!(store.get_cursor(&SourceId::new("files")).unwrap().is_none());
        let hits = store.search_text("needle", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].item.source.as_str(), "mail");

        let counts = store.source_counts().unwrap();
        assert_eq!(counts.len(), 1);
        assert_eq!((counts[0].items, counts[0].chunks), (1, 1));
    }

    #[test]
    fn get_chunks_preserves_requested_order_and_skips_unknown_ids() {
        let store = Store::open_in_memory().unwrap();
        put(&store, "files", "a.md", None, &["first", "second"]);
        let hits = store.search_text("second", 1).unwrap();
        let second = hits[0].chunk_id;
        let first = store.search_text("first", 1).unwrap()[0].chunk_id;

        let got = store.get_chunks(&[second, 999_999, first]).unwrap();
        let texts: Vec<_> = got.iter().map(|c| (c.text.as_str(), c.ord)).collect();
        assert_eq!(texts, vec![("second", 1), ("first", 0)]);
        assert_eq!(got[0].title.as_deref(), Some("A.MD"));
        assert!(store.get_chunks(&vec![1; 513]).is_err());
    }
}

// ---------------------------------------------------------------- M1-J: retrieval-quality APIs

mod m1j {
    use std::collections::HashMap;

    use nugget_core::ids::SourceId;
    use nugget_store::{
        Error, MAX_CHUNK_IDS, NewChunk, NewItem, SCHEMA_VERSION, Store, UpsertOutcome, content_hash,
    };

    use super::{db_path, iref, item, key_a, mem, raw_conn};

    fn put(s: &Store, key: &str, title: Option<&str>, chunks: &[&str]) -> Vec<i64> {
        let mut it = NewItem::new("files:test", key, content_hash(chunks.join("|")));
        if let Some(t) = title {
            it = it.with_title(t);
        }
        s.upsert_item(&it).unwrap();
        let chunks: Vec<_> = chunks.iter().map(|t| NewChunk::text(*t)).collect();
        s.replace_chunks(&it.item, &chunks).unwrap()
    }

    fn keys(hits: &[nugget_store::Hit]) -> Vec<&str> {
        hits.iter().map(|h| h.item.key.as_str()).collect()
    }

    #[test]
    fn title_only_matches_are_found_and_ranked() {
        let s = mem();
        let mortgage = put(
            &s,
            "m",
            Some("Mortgage renewal"),
            &["Fixed rate ends in March.", "Broker call booked."],
        );
        put(&s, "n", Some("Shopping"), &["eggs, milk, bread"]);
        put(&s, "untitled", None, &["nothing about that"]);
        // Only the title says "mortgage": the item is found, represented by its first chunk.
        let hits = s.search_text("mortgage", 10).unwrap();
        assert_eq!(keys(&hits), ["m"]);
        assert_eq!(hits[0].chunk_id, mortgage[0]);
        assert_eq!(hits[0].snippet, "Fixed rate ends in March.");
        assert_eq!(hits[0].title.as_deref(), Some("Mortgage renewal"));
        assert!(hits[0].score > 0.0);
        // Hybrid search sees it too.
        assert_eq!(keys(&s.search_hybrid("mortgage", None, 5).unwrap()), ["m"]);

        // Body and title both matching beats body alone; chunks keep their own body order.
        put(
            &s,
            "body-only",
            Some("Weekly notes"),
            &["the broker said the broker will call"],
        );
        let hits = s.search_text("broker mortgage", 10).unwrap();
        assert_eq!(hits[0].item.key.as_str(), "m", "{hits:?}");
        assert!(keys(&hits).contains(&"body-only"));
        // The title boost is added to the item's matching chunk, not to a second hit.
        assert_eq!(keys(&hits).iter().filter(|k| **k == "m").count(), 1);
        assert_eq!(hits[0].chunk_id, mortgage[1]);
    }

    #[test]
    fn title_index_follows_upsert_rename_and_delete() {
        let s = mem();
        put(&s, "a", Some("Alpha plan"), &["body text"]);
        assert_eq!(keys(&s.search_text("alpha", 5).unwrap()), ["a"]);

        // Title change: the old title no longer matches, the new one does.
        let renamed =
            NewItem::new("files:test", "a", content_hash("body text")).with_title("Beta plan");
        assert_eq!(s.upsert_item(&renamed).unwrap(), UpsertOutcome::Updated);
        assert!(s.search_text("alpha", 5).unwrap().is_empty());
        assert_eq!(keys(&s.search_text("beta", 5).unwrap()), ["a"]);

        // Title removed.
        let untitled = NewItem::new("files:test", "a", content_hash("body text"));
        s.upsert_item(&untitled).unwrap();
        assert!(s.search_text("beta", 5).unwrap().is_empty());
        assert_eq!(keys(&s.search_text("body", 5).unwrap()), ["a"]);

        // Delete (single and per source).
        put(&s, "b", Some("Gamma"), &["x"]);
        assert!(s.delete_item(&iref("files:test", "b")).unwrap());
        assert!(s.search_text("gamma", 5).unwrap().is_empty());
        put(&s, "c", Some("Delta"), &["y"]);
        s.delete_source(&SourceId::new("files:test")).unwrap();
        assert!(s.search_text("delta", 5).unwrap().is_empty());

        // An item without chunks cannot be a hit, even when its title matches.
        s.upsert_item(&NewItem::new("files:test", "empty", "h").with_title("Epsilon"))
            .unwrap();
        assert!(s.search_text("epsilon", 5).unwrap().is_empty());
        // Row ids get reused after deletes; the index must stay consistent.
        put(&s, "d", Some("Zeta"), &["z"]);
        assert_eq!(keys(&s.search_text("zeta", 5).unwrap()), ["d"]);
    }

    #[test]
    fn encrypted_v2_database_migrates_to_v3_and_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = db_path(&dir);
        {
            let s = Store::open(&path, &key_a()).unwrap();
            let it = item("a", "Quarterly tax return", "receipts and invoices");
            s.upsert_item(&it.clone().with_version("v1")).unwrap();
            s.replace_chunks(
                &it.item,
                &[NewChunk::embedded("receipts and invoices", vec![1.0, 0.0])],
            )
            .unwrap();
            s.set_cursor(&SourceId::new("files:test"), serde_json::json!({"n": 1}))
                .unwrap();
        }
        // Turn the file back into a v2 database, as an older build left it.
        raw_conn(&path, &[7u8; 32])
            .execute_batch(
                "DROP TRIGGER items_fts_ai; DROP TRIGGER items_fts_ad; DROP TRIGGER items_fts_au;
                 DROP TABLE items_fts;
                 PRAGMA user_version = 2;",
            )
            .unwrap();

        let s = Store::open(&path, &key_a()).unwrap();
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(keys(&s.search_text("tax", 5).unwrap()), ["a"], "backfilled");
        let got = s.get_item(&iref("files:test", "a")).unwrap().unwrap();
        assert_eq!(got.version.as_deref(), Some("v1"));
        assert_eq!(s.search_vector(&[1.0, 0.0], 1).unwrap().len(), 1);
        assert!(
            s.get_cursor(&SourceId::new("files:test"))
                .unwrap()
                .is_some()
        );
        assert_eq!(s.migrate().unwrap(), 0);
    }

    #[test]
    fn neighbours_and_chunks_by_ord_use_positions() {
        let s = mem();
        let a = put(&s, "a", Some("A"), &["a0", "a1", "a2", "a3", "a4"]);
        let b = put(&s, "b", Some("B"), &["b0", "b1"]);
        // Re-chunking `a` after `b` moves its chunks to new ids; lookups go by position.
        let a = {
            let ids = put(&s, "a", Some("A"), &["a0", "a1", "a2", "a3", "a4"]);
            assert_ne!(ids, a);
            ids
        };

        let texts = |v: Vec<nugget_store::Chunk>| -> Vec<(String, u32)> {
            v.into_iter().map(|c| (c.text, c.ord)).collect()
        };
        assert_eq!(
            texts(s.neighbours(a[2], 1).unwrap()),
            [("a1".into(), 1), ("a2".into(), 2), ("a3".into(), 3)]
        );
        // Clamped at the item's edges; never crosses into another item.
        assert_eq!(
            texts(s.neighbours(a[0], 1).unwrap()),
            [("a0".into(), 0), ("a1".into(), 1)]
        );
        assert_eq!(
            texts(s.neighbours(b[1], 2).unwrap()),
            [("b0".into(), 0), ("b1".into(), 1)]
        );
        assert_eq!(texts(s.neighbours(a[4], 0).unwrap()), [("a4".into(), 4)]);
        assert!(s.neighbours(999_999, 1).unwrap().is_empty());
        assert_eq!(
            s.neighbours(a[2], u32::MAX).unwrap().len(),
            5,
            "capped radius"
        );
        let n = s.neighbours(a[1], 1).unwrap();
        assert!(n.iter().all(|c| c.item == iref("files:test", "a")));
        assert_eq!(n[0].title.as_deref(), Some("A"));

        let by_ord = s
            .get_chunks_by_ord(&iref("files:test", "a"), &[3, 9, 0, 3])
            .unwrap();
        assert_eq!(
            texts(by_ord),
            [("a3".into(), 3), ("a0".into(), 0), ("a3".into(), 3)]
        );
        assert!(
            s.get_chunks_by_ord(&iref("files:test", "zzz"), &[0])
                .unwrap()
                .is_empty()
        );
        assert!(
            s.get_chunks_by_ord(&iref("files:test", "a"), &[])
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            s.get_chunks_by_ord(&iref("files:test", "a"), &vec![0; MAX_CHUNK_IDS + 1]),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn get_embeddings_reads_stored_vectors_back() {
        let s = mem();
        assert!(
            s.get_embeddings(&[1, 2]).unwrap().is_empty(),
            "no vectors yet"
        );
        let a = item("a", "A", "x");
        s.upsert_item(&a).unwrap();
        let v0 = vec![0.25f32, -1.5, 3.0e-7];
        let v1 = vec![1.0f32, 0.0, f32::MIN_POSITIVE];
        let ids = s
            .replace_chunks(
                &a.item,
                &[
                    NewChunk::embedded("one", v0.clone()),
                    NewChunk::text("two (text only)"),
                    NewChunk::embedded("three", v1.clone()),
                ],
            )
            .unwrap();
        let got = s
            .get_embeddings(&[ids[2], ids[1], ids[0], 424_242, ids[0]])
            .unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[&ids[0]], v0, "bit-exact round trip");
        assert_eq!(got[&ids[2]], v1);
        assert!(!got.contains_key(&ids[1]));
        assert!(s.get_embeddings(&[]).unwrap().is_empty());
        assert!(matches!(
            s.get_embeddings(&vec![1; MAX_CHUNK_IDS + 1]),
            Err(Error::Invalid(_))
        ));
        // Deleted chunks lose their vectors.
        s.delete_item(&a.item).unwrap();
        assert!(s.get_embeddings(&ids).unwrap().is_empty());
    }

    #[test]
    fn upsert_item_with_chunks_is_atomic() {
        let s = mem();
        let v1 = NewItem::new("files:test", "doc", content_hash("one"))
            .with_title("Doc")
            .with_version("1");
        let chunks = [
            NewChunk::embedded("first part", vec![1.0, 0.0]),
            NewChunk::embedded("second part", vec![0.0, 1.0]),
        ];
        assert_eq!(
            s.upsert_item_with_chunks(&v1, &chunks).unwrap(),
            UpsertOutcome::Inserted
        );
        assert_eq!(s.counts().unwrap().chunks, 2);
        assert_eq!(s.search_vector(&[1.0, 0.0], 5).unwrap().len(), 2);
        let ver = |s: &Store| s.get_item(&v1.item).unwrap().unwrap().version;
        assert_eq!(ver(&s).as_deref(), Some("1"));

        // Same item again: nothing written, chunk ids stable.
        let before = s.search_text("first", 1).unwrap()[0].chunk_id;
        assert_eq!(
            s.upsert_item_with_chunks(&v1, &chunks).unwrap(),
            UpsertOutcome::Unchanged
        );
        assert_eq!(s.search_text("first", 1).unwrap()[0].chunk_id, before);

        // New content: item, version and chunks change together.
        let v2 = NewItem::new("files:test", "doc", content_hash("two"))
            .with_title("Doc")
            .with_version("2");
        assert_eq!(
            s.upsert_item_with_chunks(&v2, &[NewChunk::embedded("rewritten", vec![0.6, 0.8])])
                .unwrap(),
            UpsertOutcome::Updated
        );
        assert!(s.search_text("first", 5).unwrap().is_empty());
        assert_eq!(s.search_text("rewritten", 5).unwrap().len(), 1);
        assert_eq!(ver(&s).as_deref(), Some("2"));
        assert_eq!(s.counts().unwrap().chunks, 1);

        // A bad batch changes nothing: not the item, its version, nor its chunks.
        let v3 = NewItem::new("files:test", "doc", content_hash("three")).with_version("3");
        for bad in [
            vec![NewChunk::embedded("x", vec![1.0, 0.0, 0.0])], // store dimension is 2
            vec![NewChunk::embedded("x", vec![f32::NAN, 0.0])],
        ] {
            assert!(matches!(
                s.upsert_item_with_chunks(&v3, &bad),
                Err(Error::Invalid(_))
            ));
            assert_eq!(ver(&s).as_deref(), Some("2"));
            assert_eq!(s.search_text("rewritten", 5).unwrap().len(), 1);
        }
        let fresh = NewItem::new("files:test", "never", "h");
        assert!(
            s.upsert_item_with_chunks(&fresh, &[NewChunk::embedded("x", vec![1.0; 3])])
                .is_err()
        );
        assert_eq!(s.get_item(&fresh.item).unwrap(), None, "rolled back");
        assert!(matches!(
            s.upsert_item_with_chunks(&NewItem::new("files:test", "k", ""), &[]),
            Err(Error::Invalid(_))
        ));

        // Healing: an item left without chunks by an interrupted non-atomic write gets them.
        let half = NewItem::new("files:test", "half", content_hash("h")).with_version("1");
        s.upsert_item(&half).unwrap();
        assert_eq!(
            s.upsert_item_with_chunks(&half, &[NewChunk::text("healed text")])
                .unwrap(),
            UpsertOutcome::Updated
        );
        assert_eq!(s.search_text("healed", 5).unwrap().len(), 1);
        assert_eq!(
            s.upsert_item_with_chunks(&half, &[NewChunk::text("healed text")])
                .unwrap(),
            UpsertOutcome::Unchanged
        );
        // An intentionally chunk-less item stays unchanged.
        let meta_only = NewItem::new("files:test", "meta", "m");
        s.upsert_item_with_chunks(&meta_only, &[]).unwrap();
        assert_eq!(
            s.upsert_item_with_chunks(&meta_only, &[]).unwrap(),
            UpsertOutcome::Unchanged
        );
    }

    #[test]
    fn upsert_item_with_chunks_from_many_threads() {
        let s = mem();
        let handles: Vec<_> = (0..4)
            .map(|t| {
                let s = s.clone();
                std::thread::spawn(move || {
                    for i in 0..20 {
                        let body = format!("thread {t} item {i} payload");
                        let it =
                            NewItem::new("files:test", format!("{t}-{i}"), content_hash(&body))
                                .with_version("1");
                        s.upsert_item_with_chunks(&it, &[NewChunk::text(body)])
                            .unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let counts = s.counts().unwrap();
        assert_eq!((counts.items, counts.chunks), (80, 80));
        let per_item: HashMap<_, _> = s
            .search_text("payload", 1000)
            .unwrap()
            .into_iter()
            .map(|h| (h.item, h.chunk_id))
            .collect();
        assert_eq!(per_item.len(), 80);
    }

    #[test]
    fn hybrid_fusion_depth_bounds_each_list() {
        let s = mem();
        // "a" is best in text and last in vector, "b" best in vector only, "c" second in both,
        // "d" vector only.
        for (key, body, v) in [
            ("a", "apple apple apple", vec![-1.0, 0.0]),
            ("b", "banana", vec![1.0, 0.0]),
            ("c", "apple pie", vec![0.9, 0.1]),
            ("d", "durian", vec![0.5, 0.5]),
        ] {
            let it = item(key, key, body);
            s.upsert_item_with_chunks(&it, &[NewChunk::embedded(body, v)])
                .unwrap();
        }
        let q = [1.0, 0.0];
        let keys = |h: Vec<nugget_store::Hit>| -> Vec<String> {
            h.into_iter().map(|h| h.item.key.0).collect()
        };
        // Deep lists: "c" is in both and wins.
        let deep = s
            .search_hybrid_with_depth("apple", Some(&q), 10, 10)
            .unwrap();
        assert_eq!(keys(deep.clone())[0], "c");
        assert_eq!(deep.len(), 4);
        // Depth 1: only each list's best hit takes part.
        let shallow = keys(
            s.search_hybrid_with_depth("apple", Some(&q), 10, 1)
                .unwrap(),
        );
        assert_eq!(shallow, ["a", "b"]);
        // `limit` still caps the output, and zero depth or limit yields nothing.
        assert_eq!(
            s.search_hybrid_with_depth("apple", Some(&q), 1, 10)
                .unwrap()
                .len(),
            1
        );
        assert!(
            s.search_hybrid_with_depth("apple", Some(&q), 10, 0)
                .unwrap()
                .is_empty()
        );
        assert!(
            s.search_hybrid_with_depth("apple", Some(&q), 0, 10)
                .unwrap()
                .is_empty()
        );
        // `search_hybrid` is the depth-explicit search at max(4 × limit, 50).
        assert_eq!(
            s.search_hybrid("apple", Some(&q), 10).unwrap(),
            s.search_hybrid_with_depth("apple", Some(&q), 10, 50)
                .unwrap()
        );
    }

    #[test]
    fn clear_cursor_forgets_only_that_source() {
        let s = mem();
        let a = SourceId::new("files:a");
        let b = SourceId::new("files:b");
        s.set_cursor(&a, serde_json::json!({"x": 1})).unwrap();
        s.set_cursor(&b, serde_json::json!("y")).unwrap();
        put(&s, "kept", None, &["still here"]);
        assert!(s.clear_cursor(&a).unwrap());
        assert!(!s.clear_cursor(&a).unwrap(), "already gone");
        assert_eq!(s.get_cursor(&a).unwrap(), None);
        assert_eq!(s.get_cursor(&b).unwrap(), Some(serde_json::json!("y")));
        assert_eq!(s.counts().unwrap().items, 1, "items are kept");
        // A fresh cursor can be set again afterwards.
        s.set_cursor(&a, serde_json::json!(2)).unwrap();
        assert_eq!(s.get_cursor(&a).unwrap(), Some(serde_json::json!(2)));
    }
}
