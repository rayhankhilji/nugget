//! The item change feed (`items_changed_since`, with deletion tombstones) and `item_dates`.

use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_store::{NewChunk, NewItem, Store, content_hash};
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

fn at(s: &str) -> OffsetDateTime {
    OffsetDateTime::parse(s, &Rfc3339).unwrap()
}

fn iref(source: &str, key: &str) -> ItemRef {
    ItemRef {
        source: SourceId::new(source),
        key: ItemKey::new(key),
    }
}

fn put(s: &Store, source: &str, key: &str, body: &str) {
    let it = NewItem::new(source, key, content_hash(body))
        .with_text(body)
        .with_version(format!("v-{body}"));
    s.upsert_item_with_chunks(&it, &[NewChunk::text(body)])
        .unwrap();
}

fn keys(c: &[nugget_store::ItemChange]) -> Vec<(String, bool)> {
    c.iter()
        .map(|c| (c.item.key.as_str().to_owned(), c.deleted))
        .collect()
}

#[test]
fn feed_reports_inserts_updates_and_deletions_once_each() {
    let s = Store::open_in_memory().unwrap();
    assert_eq!(s.item_changes_head().unwrap(), 0);
    assert_eq!(s.items_changed_since(0, 0).unwrap(), (vec![], 0));
    put(&s, "mail", "a", "one");
    put(&s, "mail", "b", "two");
    put(&s, "files", "c", "three");
    let (all, c1) = s.items_changed_since(0, 0).unwrap();
    assert_eq!(
        keys(&all),
        [
            ("a".into(), false),
            ("b".into(), false),
            ("c".into(), false)
        ]
    );
    assert_eq!(all[0].version.as_deref(), Some("v-one"));
    assert_eq!(c1, s.item_changes_head().unwrap());

    // Unchanged re-upserts are not changes; edits and deletes are.
    put(&s, "mail", "a", "one");
    assert!(s.items_changed_since(c1, 0).unwrap().0.is_empty());
    put(&s, "mail", "b", "two, edited");
    s.delete_item(&iref("mail", "a")).unwrap();
    let (next, c2) = s.items_changed_since(c1, 0).unwrap();
    assert_eq!(keys(&next), [("b".into(), false), ("a".into(), true)]);
    assert_eq!(next[0].version.as_deref(), Some("v-two, edited"));
    assert_eq!(next[1].version, None, "tombstones carry no version");

    // A full replay from 0 lists each key once, at its latest state.
    let (replay, _) = s.items_changed_since(0, 0).unwrap();
    assert_eq!(
        keys(&replay),
        [("c".into(), false), ("b".into(), false), ("a".into(), true)]
    );

    // delete_source tombstones every item; a re-insert replaces the tombstone.
    s.delete_source(&SourceId::new("files")).unwrap();
    put(&s, "mail", "a", "back");
    let (next, c3) = s.items_changed_since(c2, 0).unwrap();
    assert_eq!(keys(&next), [("c".into(), true), ("a".into(), false)]);

    // Paging.
    let (page, cur) = s.items_changed_since(0, 2).unwrap();
    assert_eq!(page.len(), 2);
    let (rest, end) = s.items_changed_since(cur, 2).unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(end, c3);
    assert_eq!(s.items_changed_since(end, 2).unwrap(), (vec![], end));
}

#[test]
fn item_dates_are_cheap_and_skip_unknown_items() {
    let s = Store::open_in_memory().unwrap();
    let it = NewItem::new("mail", "m1", "h1")
        .with_modified_at(at("2026-03-01T10:00:00Z"))
        .with_metadata(json!({"extracted": {"date": "2026-02-28T09:00:00Z"}}));
    s.upsert_item(&it).unwrap();
    s.upsert_item(
        &NewItem::new("mail", "m2", "h2").with_metadata(json!({"extracted": {"date": 5}})),
    )
    .unwrap();
    let got = s
        .item_dates(&[
            iref("mail", "m1"),
            iref("mail", "m2"),
            iref("mail", "nope"),
            iref("mail", "m1"),
        ])
        .unwrap();
    assert_eq!(got.len(), 2);
    let d1 = &got[&iref("mail", "m1")];
    assert_eq!(d1.extracted_date.as_deref(), Some("2026-02-28T09:00:00Z"));
    assert_eq!(d1.source_modified_at, Some(at("2026-03-01T10:00:00Z")));
    let d2 = &got[&iref("mail", "m2")];
    assert_eq!(d2.extracted_date, None, "non-string dates are ignored");
    assert_eq!(d2.source_modified_at, None);
    assert!(s.item_dates(&[]).unwrap().is_empty());
}
