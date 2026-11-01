//! Broadcast notifications: every connection subscribes when it opens
//! ([`nugget_ipc::Handler::connected`]) and receives every broadcast from then on.

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

    #[test]
    fn a_removed_source_is_announced_by_id() {
        let (m, v) = ingest_notification(IngestEvent::SourceRemoved(SourceId::new("files-1")));
        assert_eq!(m, notification::SOURCES_CHANGED);
        assert_eq!(v, json!({"id": "files-1", "removed": true}));
    }
}
