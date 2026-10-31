//! Broadcast notifications: every connection subscribes when it opens
//! ([`nugget_ipc::Handler::connected`]) and receives every broadcast from then on.
//!
//! Ingest events are forwarded as:
//! - `ingest.progress`: an [`IngestProgress`](nugget_core::ipc::IngestProgress);
//! - `sources.changed`: a single [`SourceInfo`](nugget_core::ipc::SourceInfo) when a source was
//!   added or changed; `{"id": "<source id>", "removed": true}` when one was removed; and
//!   `{"lagged": <n>}` when events were dropped for a slow daemon-side reader, meaning "refetch
//!   `sources.list`". Clients should treat anything but a `SourceInfo` as "refetch".
//!
//! Each connection has its own bounded queue and sender task, so a slow client never delays the
//! others; if its queue is full, it misses that notification (logged).

use std::sync::{Mutex, PoisonError};

use nugget_core::ipc::notification;
use nugget_ingest::{Ingest, IngestEvent};
use nugget_ipc::Notifier;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

/// Notifications queued per connection before new ones are dropped for it.
const QUEUE: usize = 1024;

#[derive(Default)]
pub(crate) struct Hub {
    subs: Mutex<Vec<mpsc::Sender<(&'static str, Value)>>>,
}

impl Hub {
    /// Deliver every later broadcast to this connection, in order, until it closes.
    pub fn subscribe(&self, notify: &Notifier) {
        let (tx, mut rx) = mpsc::channel::<(&'static str, Value)>(QUEUE);
        let notify = notify.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = notify.closed() => break,
                    next = rx.recv() => match next {
                        Some((method, params)) => {
                            if notify.send(method, &params).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                }
            }
        });
        let mut subs = self.subs.lock().unwrap_or_else(PoisonError::into_inner);
        subs.retain(|s| !s.is_closed());
        subs.push(tx);
    }

    /// Queue `params` for every open connection.
    pub fn broadcast(&self, method: &'static str, params: Value) {
        let mut subs = self.subs.lock().unwrap_or_else(PoisonError::into_inner);
        subs.retain(|s| !s.is_closed());
        for s in subs.iter() {
            if let Err(mpsc::error::TrySendError::Full(_)) = s.try_send((method, params.clone())) {
                tracing::warn!(method, "client is not reading notifications; dropped one");
            }
        }
    }
}

/// The notification for one ingest event.
pub(crate) fn ingest_notification(ev: IngestEvent) -> (&'static str, Value) {
    match ev {
        IngestEvent::Progress(p) => (
            notification::INGEST_PROGRESS,
            serde_json::to_value(p).unwrap_or(Value::Null),
        ),
        IngestEvent::SourceChanged(info) => (
            notification::SOURCES_CHANGED,
            serde_json::to_value(info).unwrap_or(Value::Null),
        ),
        IngestEvent::SourceRemoved(id) => (
            notification::SOURCES_CHANGED,
            json!({"id": id, "removed": true}),
        ),
    }
}

/// Forward ingest events to every client until `stop`.
pub(crate) async fn forward_ingest_events(
    hub: std::sync::Arc<Hub>,
    mut events: broadcast::Receiver<IngestEvent>,
    stop: CancellationToken,
) {
    loop {
        let ev = tokio::select! {
            () = stop.cancelled() => return,
            ev = events.recv() => ev,
        };
        match ev {
            Ok(ev) => {
                let (method, params) = ingest_notification(ev);
                hub.broadcast(method, params);
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(
                    missed = n,
                    "ingest events lagged; asking clients to refetch"
                );
                hub.broadcast(notification::SOURCES_CHANGED, json!({"lagged": n}));
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// Subscribe to `ingest` now (so nothing is missed) and forward from a task.
pub(crate) fn spawn_forwarder(
    hub: std::sync::Arc<Hub>,
    ingest: &Ingest,
    stop: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(forward_ingest_events(hub, ingest.subscribe(), stop))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_core::ids::SourceId;
    use nugget_core::ipc::{IngestPhase, IngestProgress, SourceInfo, SourceState};

    #[test]
    fn event_payloads() {
        let (m, v) = ingest_notification(IngestEvent::SourceRemoved(SourceId::new("files-1")));
        assert_eq!(m, notification::SOURCES_CHANGED);
        assert_eq!(v, json!({"id": "files-1", "removed": true}));

        let info = SourceInfo {
            id: SourceId::new("imap-1"),
            kind: "imap".into(),
            name: "Mail".into(),
            enabled: true,
            state: SourceState::NeedsAuth,
            items: 0,
            chunks: 0,
            last_sync: None,
            last_error: None,
            settings: json!({"host": "h"}),
            health: None,
        };
        let (m, v) = ingest_notification(IngestEvent::SourceChanged(info.clone()));
        assert_eq!(m, notification::SOURCES_CHANGED);
        assert_eq!(serde_json::from_value::<SourceInfo>(v).unwrap(), info);

        let p = IngestProgress {
            source: SourceId::new("files-1"),
            phase: IngestPhase::Indexing,
            done: 3,
            total: Some(9),
            error: None,
        };
        let (m, v) = ingest_notification(IngestEvent::Progress(p.clone()));
        assert_eq!(m, notification::INGEST_PROGRESS);
        assert_eq!(serde_json::from_value::<IngestProgress>(v).unwrap(), p);
    }
}
