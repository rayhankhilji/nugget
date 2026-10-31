//! Source lookups for citation footnotes and sensitivity.

use std::collections::HashMap;

use nugget_core::Result;
use nugget_core::ids::ItemRef;

/// What a citation shows about its source item.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceInfo {
    pub title: Option<String>,
    /// The item's own url (`metadata.url`), if it has one.
    pub url: Option<String>,
    /// The item is sensitive (`nugget_retrieve::is_sensitive` for store items). The vault never
    /// quotes or cites a sensitive item, and leaves out facts and entities supported only by
    /// sensitive items (see the crate docs).
    pub sensitive: bool,
}

/// Resolves evidence items to titles, urls and sensitivity. Implemented for
/// `nugget_store::Store` (feature `store`, on by default); tests and tools can use [`NoItems`]
/// or their own map.
pub trait ItemLookup: Send + Sync {
    fn source_info(&self, item: &ItemRef) -> Result<Option<SourceInfo>>;

    /// Whether an item this lookup does not know (`source_info` is `None`) counts as sensitive.
    /// `false` by default (a lookup that knows no items); the store says `true`, since an item
    /// missing from the store cannot be shown to be safe.
    fn missing_is_sensitive(&self) -> bool {
        false
    }

    /// [`ItemLookup::source_info`] for many items at once; items the lookup does not know are
    /// left out. The default asks one item at a time; the store reads only titles and metadata
    /// (never item text), a batch per query. The vault calls this once per entity it checks
    /// and caches the answers for the render pass.
    fn source_infos(&self, items: &[ItemRef]) -> Result<HashMap<ItemRef, SourceInfo>> {
        let mut out = HashMap::with_capacity(items.len());
        for item in items {
            if let Some(info) = self.source_info(item)? {
                out.insert(item.clone(), info);
            }
        }
        Ok(out)
    }
}

/// A lookup that knows no items: citations fall back to the item key and a `nugget://` link.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoItems;

impl ItemLookup for NoItems {
    fn source_info(&self, _item: &ItemRef) -> Result<Option<SourceInfo>> {
        Ok(None)
    }
}

impl<F> ItemLookup for F
where
    F: Fn(&ItemRef) -> Option<SourceInfo> + Send + Sync,
{
    fn source_info(&self, item: &ItemRef) -> Result<Option<SourceInfo>> {
        Ok(self(item))
    }
}

#[cfg(feature = "store")]
fn store_source(meta: nugget_store::ItemMeta) -> SourceInfo {
    SourceInfo {
        sensitive: nugget_retrieve::is_sensitive(&meta.metadata),
        url: meta
            .metadata
            .get("url")
            .and_then(|u| u.as_str())
            .map(str::to_owned),
        title: meta.title,
    }
}

/// Reads titles and metadata only ([`nugget_store::Store::item_meta`]), never item text.
#[cfg(feature = "store")]
impl ItemLookup for nugget_store::Store {
    fn source_info(&self, item: &ItemRef) -> Result<Option<SourceInfo>> {
        Ok(self
            .item_meta(std::slice::from_ref(item))?
            .remove(item)
            .map(store_source))
    }

    fn missing_is_sensitive(&self) -> bool {
        true
    }

    fn source_infos(&self, items: &[ItemRef]) -> Result<HashMap<ItemRef, SourceInfo>> {
        Ok(self
            .item_meta(items)?
            .into_iter()
            .map(|(k, m)| (k, store_source(m)))
            .collect())
    }
}
