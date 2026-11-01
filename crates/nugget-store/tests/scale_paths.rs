//! Batch upserts, `text_matches_among` and `items_page`.

use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_store::{NewChunk, NewItem, Store, UpsertOutcome, content_hash};

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
fn batch_upsert_is_all_or_nothing() {
    let s = Store::open_in_memory().unwrap();
    let ok = (
        item("files:a", "good", "fine"),
        vec![NewChunk::embedded("fine", vec![1.0, 0.0])],
    );
    let mixed = (
        item("files:a", "bad", "other"),
        vec![NewChunk::embedded("other", vec![1.0, 0.0, 0.0])],
    );
    assert!(s.upsert_items_with_chunks(&[ok.clone(), mixed]).is_err());
    assert_eq!(s.counts().unwrap().items, 0);
    let nan = (
        item("files:a", "nan", "x"),
        vec![NewChunk::embedded("x", vec![f32::NAN, 0.0])],
    );
    assert!(s.upsert_items_with_chunks(&[ok.clone(), nan]).is_err());
    assert_eq!(s.counts().unwrap().items, 0);
    assert_eq!(
        s.upsert_items_with_chunks(&[ok]).unwrap(),
        [UpsertOutcome::Inserted]
    );
    assert!(s.upsert_items_with_chunks(&[]).unwrap().is_empty());
}

/// `text_matches_among` agrees with the `search_text(q, at_most + 1)` check it replaces.
#[test]
fn text_matches_among_agrees_with_search_text() {
    let s = Store::open_in_memory().unwrap();
    for i in 0..60 {
        let mut body = format!("common filler {i}");
        if i % 12 == 0 {
            body.push_str(" rare");
        }
        let title = if i == 3 || i == 40 {
            "titleword note"
        } else {
            "note"
        };
        s.upsert_item_with_chunks(
            &NewItem::new("files:a", format!("k{i}"), content_hash(&body)).with_title(title),
            &[NewChunk::text(body.clone())],
        )
        .unwrap();
    }
    let among: Vec<ItemRef> = (0..60).map(|i| iref("files:a", &format!("k{i}"))).collect();
    for (q, at_most) in [
        ("rare", 5),
        ("rare", 4),
        ("titleword", 3),
        ("common", 59),
        ("absent", 3),
        ("", 3),
    ] {
        let hits = s.search_text(q, at_most + 1).unwrap();
        let expected = (hits.len() <= at_most).then(|| {
            among
                .iter()
                .map(|r| hits.iter().any(|h| &h.item == r))
                .collect::<Vec<bool>>()
        });
        assert_eq!(
            s.text_matches_among(q, at_most, &among).unwrap(),
            expected,
            "{q} at most {at_most}"
        );
    }
    assert_eq!(s.text_matches_among("rare", 10, &[]).unwrap(), Some(vec![]));
}

#[test]
fn items_page_walks_a_source_in_key_order() {
    let s = Store::open_in_memory().unwrap();
    for i in 0..23 {
        let body = format!("text {i}");
        s.upsert_item_with_chunks(
            &NewItem::new("files:a", format!("k{i:02}"), content_hash(&body))
                .with_text(body.clone()),
            &[NewChunk::text(body.clone())],
        )
        .unwrap();
    }
    s.upsert_item(&NewItem::new("files:b", "other", content_hash("x")))
        .unwrap();
    let src = SourceId::new("files:a");
    let mut keys = Vec::new();
    let mut after: Option<ItemKey> = None;
    let mut pages = 0;
    loop {
        let (page, next) = s.items_page(&src, after.as_ref(), 5).unwrap();
        pages += 1;
        assert!(page.len() <= 5);
        keys.extend(page.iter().map(|it| it.item.key.as_str().to_owned()));
        assert!(
            page.iter()
                .all(|it| it.text.as_deref().is_some_and(|t| t.starts_with("text")))
        );
        if pages == 1 {
            assert!(s.delete_item(&iref("files:a", "k06")).unwrap());
        }
        match next {
            Some(k) => after = Some(k),
            None => break,
        }
    }
    let want: Vec<String> = (0..23)
        .filter(|i| *i != 6)
        .map(|i| format!("k{i:02}"))
        .collect();
    assert_eq!(keys, want);
    assert!(s.items_page(&src, None, 0).unwrap().0.is_empty());
    let (none, next) = s.items_page(&SourceId::new("nope"), None, 5).unwrap();
    assert!(none.is_empty() && next.is_none());
}
