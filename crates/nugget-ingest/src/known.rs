//! [`KnownItems`] backed by [`Store::item_versions`].

use std::collections::HashMap;

use async_trait::async_trait;
use nugget_connector::KnownItems;
use nugget_core::ids::{ItemKey, SourceId};
use nugget_core::{Error, Result};
use nugget_store::Store;

/// What the store holds for one source: item key → connector version.
#[derive(Debug, Clone)]
pub struct StoreKnownItems {
    store: Store,
    source: SourceId,
}

impl StoreKnownItems {
    pub fn new(store: Store, source: SourceId) -> Self {
        Self { store, source }
    }
}

#[async_trait]
impl KnownItems for StoreKnownItems {
    async fn versions(&self) -> Result<HashMap<ItemKey, Option<String>>> {
        let store = self.store.clone();
        let source = self.source.clone();
        tokio::task::spawn_blocking(move || store.item_versions(&source))
            .await
            .map_err(join_err)?
    }
}

pub(crate) fn join_err(e: tokio::task::JoinError) -> Error {
    if e.is_cancelled() {
        Error::Cancelled
    } else {
        Error::Other(format!("background task panicked: {e}"))
    }
}
