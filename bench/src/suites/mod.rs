//! Registered suites. See `bench/README.md` for methodology.

pub mod cua;
pub mod draft;
pub mod memory;
pub mod perf;
pub mod proactive;

use nugget_core::ids::{ItemKey, ItemRef, SourceId};

use crate::corpus::{Doc, marker};

/// Source id used for every synthetic item.
pub const SYNTHETIC_SOURCE: &str = "bench:synthetic";

pub(crate) fn item_ref(id: usize) -> ItemRef {
    ItemRef {
        source: SourceId::new(SYNTHETIC_SOURCE),
        key: ItemKey::new(format!("doc-{id}")),
    }
}

/// The text indexed for a document version: title, body and a unique version marker.
pub(crate) fn doc_text(doc: &Doc, version: u32) -> String {
    format!("{}\n{}\n{}", doc.title, doc.body, marker(doc.id, version))
}

/// Guard against division by a zero-length timing on very fast machines.
pub(crate) fn secs(d: std::time::Duration) -> f64 {
    d.as_secs_f64().max(1e-9)
}
