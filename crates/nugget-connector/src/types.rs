use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nugget_core::egress::EgressSink;
use nugget_core::ids::{ItemKey, SourceId};
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use serde_json::Value;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

/// The raw content of a document. The pipeline extracts text from it according to `mime`.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    /// Already plain text (or Markdown when `mime` says so).
    Text(String),
    /// Raw bytes, e.g. an RFC 822 message or an iCalendar object.
    Bytes(Vec<u8>),
    /// A local file, read by the pipeline (lets large files stream instead of sitting in memory).
    File(PathBuf),
}

/// One document as reported by a connector.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    /// Stable key within the source (relative path, message id, event uid, ...).
    pub key: ItemKey,
    pub title: Option<String>,
    /// MIME type of `content`, e.g. `text/markdown`, `application/pdf`, `message/rfc822`.
    pub mime: String,
    pub content: Content,
    /// Cheap change token (e.g. `mtime_ns:size`, an etag, a history id). The pipeline skips
    /// re-processing when it equals the stored version.
    pub version: Option<String>,
    pub modified_at: Option<OffsetDateTime>,
    /// Where the user can open the original (`file://...`, a web link).
    pub url: Option<String>,
    /// Source-specific metadata (labels, folder, attendees, ...). JSON object.
    pub metadata: Value,
}

impl Document {
    pub fn new(key: impl Into<String>, mime: impl Into<String>, content: Content) -> Self {
        Self {
            key: ItemKey::new(key),
            title: None,
            mime: mime.into(),
            content,
            version: None,
            modified_at: None,
            url: None,
            metadata: Value::Object(Default::default()),
        }
    }
}

/// A change to apply to the index.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Upsert(Document),
    Delete(ItemKey),
}

/// Where a connector sends its changes. Implemented by the ingest pipeline (and by test fakes).
#[async_trait]
pub trait ChangeSink: Send + Sync {
    /// Queue a change for this source. Waits when the pipeline is busy (backpressure).
    async fn send(&self, change: Change) -> Result<()>;
    /// Commit `cursor` once every change sent before this call has been durably applied.
    async fn checkpoint(&self, cursor: Value) -> Result<()>;
}

/// What the store already holds for this source: item key → connector version.
#[async_trait]
pub trait KnownItems: Send + Sync {
    async fn versions(&self) -> Result<HashMap<ItemKey, Option<String>>>;
}

/// OS-keychain access for one source's credentials.
pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>>;
    fn set(&self, account: &str, value: &str) -> Result<()>;
    fn delete(&self, account: &str) -> Result<()>;
}

/// Everything a connector may use from its host.
#[derive(Clone)]
pub struct ConnectorContext {
    pub source: SourceId,
    pub paths: Paths,
    pub egress: Arc<dyn EgressSink>,
    pub secrets: Arc<dyn SecretStore>,
    pub known: Arc<dyn KnownItems>,
}

impl std::fmt::Debug for ConnectorContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectorContext")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

/// An interactive authorization in progress: show `url` to the user (open it in the browser);
/// `done` resolves once the redirect arrived and credentials are stored.
pub struct AuthFlow {
    pub url: String,
    pub done: Pin<Box<dyn Future<Output = Result<()>> + Send>>,
}

impl std::fmt::Debug for AuthFlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthFlow")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

/// One configured source.
#[async_trait]
pub trait Connector: Send + Sync {
    /// Connector kind, e.g. `files`, `gmail`.
    fn kind(&self) -> &'static str;

    /// Run one sync pass from `cursor` (`None` = first run) and return the cursor to resume from.
    ///
    /// `cursor = None` does not only mean "first run": ingest also resets the cursor when the
    /// source's settings change (`sources.update`), while the items indexed under the old
    /// settings stay in the store. A pass without a cursor must therefore be a **reconciling
    /// full pass**: compare what the source holds now against the known items
    /// ([`ConnectorContext::known`]), send an upsert for every current document (unchanged
    /// versions are skipped cheaply by the pipeline) and a [`Change::Delete`] for every known
    /// key that is no longer in scope (e.g. outside the new folders, roots or backfill window).
    async fn sync(
        &self,
        cursor: Option<Value>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Value>;

    /// How often the pipeline should call [`sync`](Self::sync) when not watching.
    fn poll_interval(&self) -> Duration {
        Duration::from_secs(300)
    }

    /// Whether [`watch`](Self::watch) delivers live changes.
    fn supports_watch(&self) -> bool {
        false
    }

    /// Deliver live changes until `cancel` fires. Called after a successful sync pass. Live
    /// changes do not checkpoint; the next `sync` reconciles anything missed.
    async fn watch(&self, sink: Arc<dyn ChangeSink>, cancel: CancellationToken) -> Result<()> {
        let _ = (sink, cancel);
        Err(Error::Unsupported(format!(
            "{} does not support watching",
            self.kind()
        )))
    }

    /// Start an interactive authorization (OAuth). Default: not needed.
    async fn begin_auth(&self) -> Result<AuthFlow> {
        Err(Error::Unsupported(format!(
            "{} has no interactive authorization",
            self.kind()
        )))
    }
}
