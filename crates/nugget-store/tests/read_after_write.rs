//! Reads through the reader pool see a write as soon as it returns.

use nugget_store::{NewChunk, NewItem, Store, StoreKey, content_hash};

#[test]
fn search_sees_writes_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(
        dir.path().join("db.sqlite"),
        &StoreKey::from_bytes([3u8; 32]),
    )
    .unwrap();
    for i in 0..200 {
        let body = format!("note number {i} carries marker zq{i}x");
        let it = NewItem::new("files:test", format!("k{}", i % 7), content_hash(&body))
            .with_text(body.clone());
        s.upsert_item_with_chunks(&it, &[NewChunk::text(body)])
            .unwrap();
        let hits = s.search_text(&format!("zq{i}x"), 5).unwrap();
        assert_eq!(hits.len(), 1, "write {i} not visible to the next search");
        if i >= 7 {
            // The item's previous version is gone at once too.
            let old = s.search_text(&format!("zq{}x", i - 7), 5).unwrap();
            assert!(old.is_empty(), "write {i} left the old text searchable");
        }
    }
}
