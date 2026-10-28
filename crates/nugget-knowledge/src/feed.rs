//! Where items come from, and where the pipeline keeps its progress.
//!
//! [`ItemFeed`] is the small slice of the store the pipeline needs. It is implemented for
//! [`nugget_store::Store`] with today's APIs: the work queue lists each source's item keys and
//! versions (`item_versions`) and diffs them against its own ledger, which also reveals deletions.
//! A store-side change feed (`items_changed_since`, plus deletion tombstones) would make the scan
//! incremental; the daemon can provide one by implementing this trait.

use std::collections::HashMap;

use nugget_core::Result;
use nugget_core::ids::{ItemKey, ItemRef, SourceId};
use nugget_store::{Chunk, Item, Store};
use serde_json::Value;

/// Chunk positions requested per `get_chunks_by_ord` call.
const ORD_PAGE: u32 = 128;

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
        while out.len() < limit {
            let ords: Vec<u32> = (from..from.saturating_add(ORD_PAGE)).collect();
            let page = self.get_chunks_by_ord(item, &ords)?;
            let n = page.len();
            out.extend(page);
            if n < ORD_PAGE as usize {
                break;
            }
            from = from.saturating_add(ORD_PAGE);
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
}
