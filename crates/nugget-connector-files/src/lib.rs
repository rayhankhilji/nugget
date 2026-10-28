//! Local folder (`files`) and Obsidian vault (`obsidian`) connectors.
//!
//! ## Items
//! - **Key**: `<root index>:<relative path>` with `/` separators on every OS, e.g.
//!   `0:Projects/plan.md` (see [`item_key`]). Obsidian vaults have one root, so keys start `0:`.
//! - **Version**: `mtime_ns:size` (nanoseconds since the Unix epoch, negative before 1970).
//! - **URL**: a `file://` URL; for Obsidian an `obsidian://open?vault=…&file=…` link, with the
//!   `file://` URL in `metadata.file_url`.
//! - **Content**: always [`Content::File`](nugget_connector::Content::File); the pipeline extracts
//!   text according to `mime` (from [`nugget_extract::detect_mime`]; Obsidian notes are
//!   `text/markdown`).
//!
//! ## Sync
//! Every pass walks the roots with the configured filters, compares each file's version with
//! [`KnownItems`](nugget_connector::KnownItems), upserts new or changed files and finally deletes
//! keys that are gone. A cursor checkpoint is sent every [`CHECKPOINT_EVERY`] files. When the root
//! list changes (keys are relative to root *indices*), the pass ignores stored versions and
//! re-sends everything. Unreadable files and folders are logged and skipped, and keep any
//! previously indexed copy; a root that is missing (an unmounted drive) keeps its items.
//!
//! ## Watch
//! A native watcher (inotify, FSEvents, ReadDirectoryChangesW via `notify`) with a per-path
//! debounce (`debounce_ms`). Each batch of events is turned into the set of paths it touched, and
//! those paths are re-examined with the same filters as `sync`: a vanished path deletes its key
//! and everything below it, an existing file is upserted when its version changed, a new or
//! moved-in directory is walked. A rename is therefore `Delete(old)` + `Upsert(new)` on every OS.
//! Watcher overflow (`Rescan`) or errors trigger a reconcile; so does a change to a `.gitignore`
//! or `.nuggetignore` (for its directory). On start, `watch` reconciles once to catch changes
//! made since the sync pass, and while running it reconciles every `rescan_interval_secs` as a
//! safety net (every 60 s while some root cannot be watched).

mod config;
mod connector;
mod key;
mod scan;

use std::sync::Arc;

use nugget_connector::{
    AuthKind, Connector, ConnectorContext, ConnectorFactory, KindInfo, Registry,
};
use nugget_core::Result;
use serde_json::Value;

pub use config::{
    ALWAYS_SKIP_DIRS, DEFAULT_DEBOUNCE_MS, DEFAULT_EXTENSIONS, DEFAULT_MAX_FILE_BYTES,
    DEFAULT_RESCAN_INTERVAL_SECS, FilesSettings, OBSIDIAN_ATTACHMENTS, ObsidianSettings,
    files_schema, obsidian_schema,
};
pub use connector::CHECKPOINT_EVERY;
pub use key::{PathStyle, file_url, item_key, item_key_from_str, obsidian_url, parse_key};

use config::Config;
use connector::FsConnector;

/// Kind name of the folder connector.
pub const FILES_KIND: &str = "files";
/// Kind name of the Obsidian vault connector.
pub const OBSIDIAN_KIND: &str = "obsidian";

/// Factory for the `files` kind.
#[derive(Debug, Default, Clone, Copy)]
pub struct FilesFactory;

impl ConnectorFactory for FilesFactory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: FILES_KIND,
            display_name: "Folders",
            auth: AuthKind::None,
            settings_schema: files_schema(),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        Config::files(settings)?.check_roots_exist()
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        Ok(Arc::new(FsConnector::new(Config::files(settings)?, ctx)))
    }
}

/// Factory for the `obsidian` kind.
#[derive(Debug, Default, Clone, Copy)]
pub struct ObsidianFactory;

impl ConnectorFactory for ObsidianFactory {
    fn info(&self) -> KindInfo {
        KindInfo {
            kind: OBSIDIAN_KIND,
            display_name: "Obsidian vault",
            auth: AuthKind::None,
            settings_schema: obsidian_schema(),
        }
    }

    fn validate(&self, settings: &Value) -> Result<()> {
        Config::obsidian(settings)?.check_roots_exist()
    }

    fn build(&self, settings: &Value, ctx: ConnectorContext) -> Result<Arc<dyn Connector>> {
        Ok(Arc::new(FsConnector::new(Config::obsidian(settings)?, ctx)))
    }
}

/// Register both kinds.
pub fn register(registry: &mut Registry) {
    registry.register(Arc::new(FilesFactory));
    registry.register(Arc::new(ObsidianFactory));
}
