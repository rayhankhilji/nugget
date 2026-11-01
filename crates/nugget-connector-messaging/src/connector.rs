//! The [`Connector`] shared by the three kinds: a reader produces conversations on a blocking
//! thread, they are rendered to month documents and reconciled against the known items.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use notify_debouncer_full::notify::{EventKind, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, NoCache, new_debouncer_opt};
use nugget_connector::{
    CancellationToken, Change, ChangeSink, Connector, ConnectorContext, Document,
};
use nugget_core::{Error, Result};
use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::model::Conversation;
use crate::render::{RenderOptions, conversation_docs};
use crate::settings::Common;

/// Checkpoint the cursor after this many sends.
pub const CHECKPOINT_EVERY: usize = 50;
/// Watcher debounce: an export being written produces bursts of events.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// What a reader reports while reading.
pub(crate) enum Found {
    Conversation(Conversation),
    /// Keep every known key starting with this prefix: its export could not be read this time
    /// (being written, corrupt), so its documents must not be deleted. `""` keeps everything.
    Keep(String),
}

/// One messaging source: reads its conversations (blocking I/O).
pub(crate) trait Reader: Send + Sync + 'static {
    fn kind(&self) -> &'static str;
    fn service(&self) -> &'static str;
    /// Read every conversation, calling `found` for each. Stop early (returning `Ok`) when
    /// `found` returns `false` or `cancel` fires. Errors abort the pass (no deletes).
    fn read(&self, cancel: &CancellationToken, found: &mut dyn FnMut(Found) -> bool) -> Result<()>;
    /// What to watch for live changes (`None`: polling only).
    fn watch_target(&self) -> Option<(PathBuf, RecursiveMode)> {
        None
    }
    fn poll_interval(&self) -> Duration;
    /// Called after a sync pass whose changes are committed (read, every change and delete
    /// sent, and the sink's checkpoint, which waits for them to be stored, succeeded), so a
    /// reader may remember what it read. Never after a live pass. The default does nothing.
    fn pass_succeeded(&self) {}
    /// Whether [`Reader::pass_succeeded`] does anything: `sync` then waits for the pass's
    /// changes to be stored before calling it (one more checkpoint per pass).
    fn remembers_passes(&self) -> bool {
        false
    }
}

enum Emit {
    Doc(Box<Document>),
    Keep(String),
}

pub(crate) struct MessagingConnector {
    reader: Arc<dyn Reader>,
    common: Common,
    ctx: ConnectorContext,
}

impl MessagingConnector {
    pub fn new(reader: Arc<dyn Reader>, common: Common, ctx: ConnectorContext) -> Self {
        Self {
            reader,
            common,
            ctx,
        }
    }

    /// One reconciling pass. With `checkpoint`, a `{"v":1}` cursor is committed every
    /// [`CHECKPOINT_EVERY`] sends (sync passes); live passes never checkpoint.
    async fn pass(
        &self,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
        checkpoint: bool,
    ) -> Result<()> {
        let known: HashMap<String, Option<String>> = self
            .ctx
            .known
            .versions()
            .await?
            .into_iter()
            .map(|(k, v)| (k.0, v))
            .collect();

        let (tx, mut rx) = tokio::sync::mpsc::channel::<Emit>(32);
        let reader = Arc::clone(&self.reader);
        let common = self.common;
        let token = cancel.clone();
        let opts = RenderOptions {
            service: reader.service(),
            private: common.private,
        };
        let cutoff = common
            .since_days
            .map(|d| OffsetDateTime::now_utc() - time::Duration::days(i64::from(d)));
        let handle = tokio::task::spawn_blocking(move || {
            reader.read(&token, &mut |found| match found {
                Found::Keep(prefix) => tx.blocking_send(Emit::Keep(prefix)).is_ok(),
                Found::Conversation(mut conv) => {
                    if conv.is_group && !common.include_groups {
                        return true;
                    }
                    if let Some(cutoff) = cutoff {
                        conv.messages.retain(|m| m.at >= cutoff);
                    }
                    for doc in conversation_docs(&conv, opts) {
                        if tx.blocking_send(Emit::Doc(Box::new(doc))).is_err() {
                            return false;
                        }
                    }
                    true
                }
            })
        });

        let mut produced: HashSet<String> = HashSet::new();
        let mut keep: Vec<String> = Vec::new();
        let mut sends = 0usize;
        let mut forwarded: Result<()> = Ok(());
        let cursor = json!({ "v": 1 });
        while let Some(msg) = rx.recv().await {
            let step = match msg {
                Emit::Keep(p) => {
                    keep.push(p);
                    Ok(())
                }
                Emit::Doc(doc) => {
                    let key = doc.key.0.clone();
                    let unchanged =
                        doc.version.is_some() && known.get(&key).is_some_and(|v| *v == doc.version);
                    let first = produced.insert(key.clone());
                    if unchanged || !first {
                        if !first {
                            tracing::warn!(key, "duplicate document key; keeping the first");
                        }
                        Ok(())
                    } else {
                        send(
                            sink,
                            Change::Upsert(*doc),
                            cancel,
                            checkpoint,
                            &mut sends,
                            &cursor,
                        )
                        .await
                    }
                }
            };
            if let Err(e) = step {
                forwarded = Err(e);
                break;
            }
        }
        drop(rx);
        let joined = handle.await;
        forwarded?;
        joined.map_err(|e| Error::Other(format!("chat reader failed: {e}")))??;
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }

        let mut gone: Vec<&String> = known
            .keys()
            .filter(|k| !produced.contains(*k) && !keep.iter().any(|p| k.starts_with(p.as_str())))
            .collect();
        gone.sort();
        for k in gone {
            send(
                sink,
                Change::Delete(nugget_core::ids::ItemKey::new(k.clone())),
                cancel,
                checkpoint,
                &mut sends,
                &cursor,
            )
            .await?;
        }
        tracing::debug!(
            kind = self.reader.kind(),
            documents = produced.len(),
            sends,
            "messaging pass done"
        );
        Ok(())
    }
}

async fn send(
    sink: &dyn ChangeSink,
    change: Change,
    cancel: &CancellationToken,
    checkpoint: bool,
    sends: &mut usize,
    cursor: &Value,
) -> Result<()> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(Error::Cancelled),
        r = sink.send(change) => r?,
    }
    *sends += 1;
    if checkpoint && (*sends).is_multiple_of(CHECKPOINT_EVERY) {
        sink.checkpoint(cursor.clone()).await?;
    }
    Ok(())
}

/// Whether a debounced batch contains anything but reads.
fn relevant(batch: &[DebounceEventResult]) -> bool {
    batch.iter().any(|r| match r {
        Err(_) => true,
        Ok(events) => events
            .iter()
            .any(|e| e.need_rescan() || !matches!(e.kind, EventKind::Access(_))),
    })
}

#[async_trait]
impl Connector for MessagingConnector {
    fn kind(&self) -> &'static str {
        self.reader.kind()
    }

    async fn sync(
        &self,
        _cursor: Option<Value>,
        sink: &dyn ChangeSink,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        // Every pass is a full reconcile, so the cursor carries no state.
        self.pass(sink, cancel, true).await?;
        let cursor = json!({ "v": 1 });
        // `send` only queues a change: the checkpoint returns once every change of the pass is
        // stored (or fails if one could not be). Only then may the reader skip unchanged input.
        if self.reader.remembers_passes() {
            sink.checkpoint(cursor.clone()).await?;
            self.reader.pass_succeeded();
        }
        Ok(cursor)
    }

    fn poll_interval(&self) -> Duration {
        self.reader.poll_interval()
    }

    fn supports_watch(&self) -> bool {
        self.reader.watch_target().is_some()
    }

    async fn watch(&self, sink: Arc<dyn ChangeSink>, cancel: CancellationToken) -> Result<()> {
        let Some((path, mode)) = self.reader.watch_target() else {
            return Err(Error::Unsupported(format!(
                "{} does not support watching",
                self.kind()
            )));
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<DebounceEventResult>();
        let mut debouncer = new_debouncer_opt::<_, RecommendedWatcher, NoCache>(
            DEBOUNCE,
            Some(DEBOUNCE),
            move |r: DebounceEventResult| {
                let _ = tx.send(r);
            },
            NoCache,
            notify_debouncer_full::notify::Config::default(),
        )
        .map_err(|e| Error::Other(format!("cannot start file watcher: {e}")))?;
        debouncer
            .watch(&path, mode)
            .map_err(|e| Error::Other(format!("cannot watch {}: {e}", path.display())))?;

        // Catch up with anything that changed since the last sync pass.
        match self.pass(sink.as_ref(), &cancel, false).await {
            Err(Error::Cancelled) => {}
            Err(e) => tracing::warn!(kind = self.kind(), error = %e, "live pass failed"),
            Ok(()) => {}
        }
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                msg = rx.recv() => {
                    let Some(first) = msg else {
                        return Err(Error::Other("file watcher stopped".into()));
                    };
                    let mut batch = vec![first];
                    while let Ok(more) = rx.try_recv() {
                        batch.push(more);
                    }
                    if !relevant(&batch) {
                        continue;
                    }
                    match self.pass(sink.as_ref(), &cancel, false).await {
                        Err(Error::Cancelled) => break,
                        // A half-written export: the next event (or poll) retries.
                        Err(e) => tracing::warn!(kind = self.kind(), error = %e, "live pass failed"),
                        Ok(()) => {}
                    }
                }
            }
        }
        debouncer.stop_nonblocking();
        Err(Error::Cancelled)
    }
}
