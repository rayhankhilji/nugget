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
