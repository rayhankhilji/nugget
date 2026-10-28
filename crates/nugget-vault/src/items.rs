//! Source lookups for citation footnotes.

use nugget_core::Result;
use nugget_core::ids::ItemRef;

/// What a citation shows about its source item.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceInfo {
    pub title: Option<String>,
    /// The item's own url (`metadata.url`), if it has one.
    pub url: Option<String>,
}

/// Resolves evidence items to titles and urls. Implemented for `nugget_store::Store` (feature
/// `store`, on by default); tests and tools can use [`NoItems`] or their own map.
pub trait ItemLookup: Send + Sync {
    fn source_info(&self, item: &ItemRef) -> Result<Option<SourceInfo>>;
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
impl ItemLookup for nugget_store::Store {
    fn source_info(&self, item: &ItemRef) -> Result<Option<SourceInfo>> {
        Ok(self.get_item(item)?.map(|it| SourceInfo {
            title: it.title,
            url: it
                .metadata
                .get("url")
                .and_then(|u| u.as_str())
                .map(str::to_owned),
        }))
    }
}
