//! Where items come from, and where the pipeline keeps its progress.
//!
//! [`ItemFeed`] is the small slice of the store the pipeline needs. The first run (and any run
//! after a reset) lists each source's item keys and versions (`item_versions`) and diffs them
//! against the pipeline's ledger, which also reveals deletions. After that, a feed with a change
//! log ([`ItemFeed::items_changed_since`], implemented for [`nugget_store::Store`] over its item
//! change feed with deletion tombstones) makes every run incremental: only changed and deleted
//! items are looked at. Feeds without one keep the full diff.

use std::collections::HashMap;

use nugget_core::Result;
use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_store::{Chunk, Item, ItemChange, Store};
use serde_json::Value;
use time::OffsetDateTime;

/// Chunk positions requested per `get_chunks_by_ord` call at most.
const ORD_PAGE: u32 = 128;
/// Chunk positions requested by the first call; each next call asks for twice as many (up to
/// [`ORD_PAGE`]). Each position is one lookup and most items have few chunks.
const FIRST_ORD_PAGE: u32 = 4;

/// Read access to items plus a small key/value area for pipeline state.
pub trait ItemFeed: Send + Sync {
    /// Every source that currently has items.
    fn sources(&self) -> Result<Vec<SourceId>>;
    /// Every item key of a source with its connector version (`None`: no version token).
    fn item_versions(&self, source: &SourceId) -> Result<HashMap<ItemKey, Option<String>>>;
    fn get_item(&self, item: &ItemRef) -> Result<Option<Item>>;
    /// The item's chunks in order, at most `limit`.
    fn chunks(&self, item: &ItemRef, limit: usize) -> Result<Vec<Chunk>>;
    /// Pipeline state stored under `key` (the store's cursor table for [`Store`]).
    fn get_state(&self, key: &str) -> Result<Option<Value>>;
    fn set_state(&self, key: &str, value: Value) -> Result<()>;
    fn clear_state(&self, key: &str) -> Result<()>;

    /// Items changed or deleted after `cursor` (at most `limit`, 0 = all) and the next cursor, or
    /// `None` when this feed keeps no change log (the pipeline then diffs every source on every
    /// run).
    fn items_changed_since(
        &self,
        cursor: u64,
        limit: usize,
    ) -> Result<Option<(Vec<ItemChange>, u64)>> {
        let _ = (cursor, limit);
        Ok(None)
    }

    /// The change log's current position (`None` without a change log).
    fn item_changes_head(&self) -> Result<Option<u64>> {
        Ok(None)
    }

    /// The date each item's facts are as of (see `extract::item_date`: the extracted content
    /// date, else the source's modification time, else when it was stored). Unknown items are
    /// left out. The default loads each item.
    fn item_dates(&self, items: &[ItemRef]) -> Result<HashMap<ItemRef, OffsetDateTime>> {
        let mut out = HashMap::new();
        for i in items {
            if let Some(item) = self.get_item(i)? {
                out.insert(i.clone(), crate::extract::item_date(&item));
            }
        }
        Ok(out)
    }
}

impl ItemFeed for Store {
    fn sources(&self) -> Result<Vec<SourceId>> {
        Ok(self
            .source_counts()?
            .into_iter()
            .filter(|s| s.items > 0)
            .map(|s| s.source)
            .collect())
    }

    fn item_versions(&self, source: &SourceId) -> Result<HashMap<ItemKey, Option<String>>> {
        Store::item_versions(self, source)
    }

    fn get_item(&self, item: &ItemRef) -> Result<Option<Item>> {
        Store::get_item(self, item)
    }

    fn chunks(&self, item: &ItemRef, limit: usize) -> Result<Vec<Chunk>> {
        let mut out = Vec::new();
        let mut from = 0u32;
        let mut want = FIRST_ORD_PAGE;
        while out.len() < limit {
            let ords: Vec<u32> = (from..from.saturating_add(want)).collect();
            let page = self.get_chunks_by_ord(item, &ords)?;
            let n = page.len();
            out.extend(page);
            if n < want as usize {
                break;
            }
            from = from.saturating_add(want);
            want = (want * 2).min(ORD_PAGE);
        }
        out.truncate(limit);
        Ok(out)
    }

    fn get_state(&self, key: &str) -> Result<Option<Value>> {
        self.get_cursor(&SourceId::new(key))
    }

    fn set_state(&self, key: &str, value: Value) -> Result<()> {
        self.set_cursor(&SourceId::new(key), value)
    }

    fn clear_state(&self, key: &str) -> Result<()> {
        self.clear_cursor(&SourceId::new(key)).map(|_| ())
    }

    fn items_changed_since(
        &self,
        cursor: u64,
        limit: usize,
    ) -> Result<Option<(Vec<ItemChange>, u64)>> {
        Store::items_changed_since(self, cursor, limit).map(Some)
    }

    fn item_changes_head(&self) -> Result<Option<u64>> {
        Store::item_changes_head(self).map(Some)
    }

    fn item_dates(&self, items: &[ItemRef]) -> Result<HashMap<ItemRef, OffsetDateTime>> {
        Ok(Store::item_dates(self, items)?
            .into_iter()
            .map(|(i, d)| {
                let at = d
                    .extracted_date
                    .as_deref()
                    .and_then(crate::extract::parse_when)
                    .or(d.source_modified_at)
                    .unwrap_or(d.updated_at);
                (i, at)
            })
            .collect())
    }
}
