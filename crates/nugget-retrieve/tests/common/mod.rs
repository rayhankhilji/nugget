//! Helpers shared by the integration tests.
#![allow(dead_code)]

use nugget_retrieve::RetrievedItem;
use nugget_retrieve::eval::HashEmbedder;
use nugget_store::{NewChunk, NewItem, Store, content_hash};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub fn at(s: &str) -> OffsetDateTime {
    OffsetDateTime::parse(s, &Rfc3339).unwrap()
}

pub fn keys(items: &[RetrievedItem]) -> Vec<&str> {
    items.iter().map(|r| r.item.key.as_str()).collect()
}

/// Index `text` as the one chunk of item `key` with `sensitive` metadata (`None`: no flag),
/// embedded with `emb` when given.
pub fn flagged(
    store: &Store,
    key: &str,
    text: &str,
    sensitive: Option<bool>,
    emb: Option<&HashEmbedder>,
) {
    let meta = sensitive.map_or(
        serde_json::json!({}),
        |s| serde_json::json!({ "sensitive": s }),
    );
    let item = NewItem::new("notes", key, content_hash(text))
        .with_title(key)
        .with_metadata(meta);
    let chunk = match emb {
        Some(e) => NewChunk::embedded(text, e.embed_text(text)),
        None => NewChunk::text(text),
    };
    store.upsert_item_with_chunks(&item, &[chunk]).unwrap();
}
