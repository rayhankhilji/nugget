//! Encrypted local store for Nugget.

mod acl;
mod audit;
mod backup;
mod changes;
mod disk;
mod egress;
mod envelope;
mod error;
mod export;
mod fts;
mod graph;
mod integrity;
mod item_graph;
mod items;
mod key;
mod schema;
mod search;
mod sources;
mod store;
mod types;
mod vec_ext;
mod vector;

pub use audit::AuditAnchor;
pub use backup::{BackupStats, read_backup_files, strip_backup_files};
pub use egress::{EgressRow, EgressState};
pub use envelope::KeyEnvelope;
pub use export::{ExportItem, GRAPH_EXPORT_FORMAT, GraphExportCounts, PAGE_BYTE_BUDGET};
pub use graph::{ENTITY_ID_PREFIX, MAX_MERGE_DEPTH, MAX_NEIGHBOURHOOD_NODES, SqliteGraph};
pub use integrity::{
    BACKGROUND_CHECK_INTERVAL, ConsistencyReport, FileHealth, OpenCheck, check_file,
    check_file_in_background, check_file_on_open,
};
pub use item_graph::{EntityLinkRow, ItemMention, ItemMeta};
pub use key::{KEYCHAIN_ENV, KeySource, StoreKey};
pub use schema::LATEST_VERSION as SCHEMA_VERSION;
pub use search::TITLE_WEIGHT;
pub use sources::{MAX_CHUNK_IDS, MAX_NEIGHBOUR_RADIUS};
pub use store::Store;
pub use types::{
    AuditEntry, AuditRecord, Chunk, ChunkId, Counts, Hit, Item, ItemChange, ItemDates, NewChunk,
    NewItem, SourceCounts, UpsertOutcome, content_hash,
};
pub use vector::{ANN_MIN_VECTOR_BYTES, ANN_MIN_VECTORS, ANN_OVERSAMPLE};

pub use nugget_core::{Error, Result};
