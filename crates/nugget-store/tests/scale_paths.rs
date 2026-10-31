//! II5: the store's bulk-write and hot status paths.

use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_store::{NewChunk, NewItem, Store, StoreKey, UpsertOutcome, content_hash};

fn item(source: &str, key: &str, body: &str) -> NewItem {
    NewItem::new(source, key, content_hash(body)).with_title(key)
}

fn iref(source: &str, key: &str) -> ItemRef {
    ItemRef {
        source: SourceId::new(source),
        key: ItemKey::new(key),
    }
}

#[test]
fn batch_upsert_matches_single_upserts() {
    let batch_store = Store::open_in_memory().unwrap();
    let single_store = Store::open_in_memory().unwrap();
    let batch: Vec<(NewItem, Vec<NewChunk>)> = (0..50)
        .map(|i| {
            let body = format!("walrus number {i} swims");
            (
                item("files:a", &format!("k{i}"), &body),
                vec![
                    NewChunk::embedded(body.clone(), vec![1.0, i as f32, 0.5]),
                    NewChunk::text(format!("tail {i}")),
                ],
            )
        })
        .collect();
    let out = batch_store.upsert_items_with_chunks(&batch).unwrap();
    assert!(out.iter().all(|o| *o == UpsertOutcome::Inserted));
    for (it, chunks) in &batch {
        single_store.upsert_item_with_chunks(it, chunks).unwrap();
    }
    assert_eq!(
        batch_store.counts().unwrap(),
        single_store.counts().unwrap()
    );
    assert_eq!(batch_store.counts().unwrap().items, 50);
    assert_eq!(batch_store.counts().unwrap().chunks, 100);
    assert_eq!(
        batch_store.search_text("walrus", 100).unwrap().len(),
        single_store.search_text("walrus", 100).unwrap().len()
    );
    // Re-upserting the same batch changes nothing.
    let again = batch_store.upsert_items_with_chunks(&batch).unwrap();
    assert!(again.iter().all(|o| *o == UpsertOutcome::Unchanged));
    assert!(
        batch_store
            .get_item(&iref("files:a", "k7"))
            .unwrap()
            .is_some()
    );
}

#[test]
fn batch_upsert_is_all_or_nothing() {
    let s = Store::open_in_memory().unwrap();
    let ok = (
        item("files:a", "good", "fine"),
        vec![NewChunk::embedded("fine".into(), vec![1.0, 0.0])],
    );
    // A different dimension in the same batch is refused before anything is written.
    let mixed = (
        item("files:a", "bad", "other"),
        vec![NewChunk::embedded("other".into(), vec![1.0, 0.0, 0.0])],
    );
    assert!(s.upsert_items_with_chunks(&[ok.clone(), mixed]).is_err());
    assert_eq!(s.counts().unwrap().items, 0);
    // An invalid embedding anywhere fails the whole batch.
    let nan = (
        item("files:a", "nan", "x"),
        vec![NewChunk::embedded("x".into(), vec![f32::NAN, 0.0])],
    );
    assert!(s.upsert_items_with_chunks(&[ok.clone(), nan]).is_err());
    assert_eq!(s.counts().unwrap().items, 0);
    assert_eq!(
        s.upsert_items_with_chunks(&[ok]).unwrap(),
        [UpsertOutcome::Inserted]
    );
    assert!(s.upsert_items_with_chunks(&[]).unwrap().is_empty());
}

#[test]
fn counts_follow_writes_and_deletes() {
    let s = Store::open_in_memory().unwrap();
    for src in ["files:a", "files:b"] {
        for i in 0..3 {
            let body = format!("{src} {i}");
            s.upsert_item_with_chunks(
                &item(src, &format!("k{i}"), &body),
                &[NewChunk::text(body.clone()), NewChunk::text("x".into())],
            )
            .unwrap();
        }
    }
    let c = s.counts().unwrap();
    assert_eq!((c.items, c.chunks), (6, 12));
    assert!(s.delete_item(&iref("files:a", "k0")).unwrap());
    let c = s.counts().unwrap();
    assert_eq!((c.items, c.chunks), (5, 10));
    // Rewriting an item's chunks keeps the totals exact.
    s.upsert_item_with_chunks(
        &item("files:b", "k1", "changed"),
        &[NewChunk::text("changed".into())],
    )
    .unwrap();
    let c = s.counts().unwrap();
    assert_eq!((c.items, c.chunks), (5, 9));
}

#[test]
fn counts_on_an_empty_store() {
    let s = Store::open_in_memory().unwrap();
    let c = s.counts().unwrap();
    assert_eq!((c.items, c.chunks), (0, 0));
    let dir = tempfile::tempdir().unwrap();
    let f = Store::open(dir.path().join("n.db"), &StoreKey::from_bytes([3; 32])).unwrap();
    assert_eq!(f.counts().unwrap().items, 0);
}
