//! Batched item reads: `get_items` and `item_dates` agree with per-item lookups, keep order,
//! and span several lookup batches.

use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_store::{NewChunk, NewItem, Store, StoreKey};

fn iref(source: &str, key: &str) -> ItemRef {
    ItemRef {
        source: SourceId::new(source),
        key: ItemKey::new(key),
    }
}

/// Items of three shapes: text kept as a layout over chunks, text in the row (chunks do not
/// cover it), and no text.
fn populate(s: &Store, n: usize) {
    for i in 0..n {
        let key = format!("k{i}");
        match i % 3 {
            0 => {
                let text = format!(
                    "first part {i}, long enough to be stored only once over the chunks. \
                     second part {i}, which the layout also points at instead of copying."
                );
                let (a, b) = text.split_at(text.find(" second").unwrap());
                let it = NewItem::new("files", key, format!("h{i}")).with_text(text.clone());
                s.upsert_item_with_chunks(&it, &[NewChunk::text(a), NewChunk::text(b.trim())])
                    .unwrap();
            }
            1 => {
                let it = NewItem::new("files", key, format!("h{i}"))
                    .with_title(format!("title {i}"))
                    .with_text(format!("whole text {i}"));
                s.upsert_item_with_chunks(&it, &[NewChunk::text("unrelated")])
                    .unwrap();
            }
            _ => {
                s.upsert_item(&NewItem::new("mail", key, format!("h{i}")))
                    .unwrap();
            }
        }
    }
}

fn check(s: &Store, n: usize) {
    // Every item, in a shuffled order, with unknown refs and duplicates mixed in.
    let mut refs: Vec<ItemRef> = (0..n)
        .map(|i| iref(if i % 3 == 2 { "mail" } else { "files" }, &format!("k{i}")))
        .collect();
    refs.reverse();
    refs.insert(3, iref("files", "missing"));
    refs.push(refs[0].clone());
    refs.push(iref("mail", "k0"));

    let batched = s.get_items(&refs).unwrap();
    assert_eq!(batched.len(), refs.len());
    for (r, got) in refs.iter().zip(&batched) {
        assert_eq!(got, &s.get_item(r).unwrap(), "{r:?}");
        if let Some(it) = got {
            assert_eq!(&it.item, r);
        }
    }
    let known = batched.iter().filter(|i| i.is_some()).count();
    assert_eq!(known, n + 1, "every item once, plus the duplicate");
    // Layout-backed texts came back whole.
    let k0 = batched
        .iter()
        .flatten()
        .find(|i| i.item.key.as_str() == "k0");
    assert_eq!(
        k0.unwrap().text.as_deref(),
        Some(
            "first part 0, long enough to be stored only once over the chunks. \
             second part 0, which the layout also points at instead of copying."
        )
    );

    let dates = s.item_dates(&refs).unwrap();
    assert_eq!(dates.len(), n);
    for it in batched.iter().flatten() {
        let d = &dates[&it.item];
        assert_eq!(d.created_at, it.created_at);
        assert_eq!(d.updated_at, it.updated_at);
        assert_eq!(d.source_modified_at, it.source_modified_at);
    }
    assert!(s.get_items(&[]).unwrap().is_empty());
}

#[test]
fn get_items_matches_get_item_in_memory() {
    let s = Store::open_in_memory().unwrap();
    populate(&s, 1200);
    check(&s, 1200);
}

/// File-backed stores read through the reader pool.
#[test]
fn get_items_matches_get_item_on_a_file_store() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(
        dir.path().join("nugget.db"),
        &StoreKey::from_bytes([9u8; 32]),
    )
    .unwrap();
    populate(&s, 600);
    check(&s, 600);
}
