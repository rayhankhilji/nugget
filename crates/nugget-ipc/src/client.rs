//! Client side: concurrent, id-correlated calls plus a notification stream.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use futures::stream::{self, BoxStream};
use futures::{Stream, StreamExt};
use interprocess::local_socket::tokio::Stream as LocalStream;
use nugget_core::paths::Paths;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::codec::LinesCodecError;
use tokio_util::sync::CancellationToken;

use crate::error::IpcError;
use crate::wire::{self, EncodeError, Incoming, JSONRPC, Notification, OutRequest};

/// Notifications buffered per subscriber before the oldest are dropped.
const NOTIFICATION_BUFFER: usize = 4096;
/// Requests queued for the writer task.
const OUTBOX: usize = 256;

type Reply = Result<Value, IpcError>;

/// A connection to `nuggetd`. Cheap to clone; all clones share one connection. The
/// connection closes when the last clone is dropped.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    shared: Arc<Shared>,
    outbox: mpsc::Sender<String>,
    next_id: AtomicU64,
    tasks: [JoinHandle<()>; 2],
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.shared.close();
        for t in &self.tasks {
            t.abort();
        }
    }
}

struct Shared {
    pending: Mutex<HashMap<u64, oneshot::Sender<Reply>>>,
    notifications: Mutex<Option<broadcast::Sender<Notification>>>,
    closed: CancellationToken,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Shared {
    /// Mark closed, fail every pending call and end every notification stream.
    fn close(&self) {
        // Order matters: set the flag before draining, so a call that registers after the
        // drain is guaranteed to observe it (see `Client::call_value`).
        self.closed.cancel();
        let pending = std::mem::take(&mut *lock(&self.pending));
        for (_, tx) in pending {
            let _ = tx.send(Err(IpcError::Disconnected));
        }
        lock(&self.notifications).take();
    }
}

/// Removes a pending entry when the call future is dropped (timeout, cancellation).
struct PendingGuard<'a> {
    shared: &'a Shared,
    id: u64,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        lock(&self.shared.pending).remove(&self.id);
    }
}

impl Client {
    /// Connect to the daemon described by `paths`.
    pub async fn connect(paths: &Paths) -> Result<Self, IpcError> {
        let stream = crate::transport::connect_stream(paths)
            .await
            .map_err(IpcError::Connect)?;
        Ok(Self::from_stream(stream))
    }

    /// Wrap an already-connected local socket stream. Must be called inside a Tokio runtime.
    pub fn from_stream(stream: LocalStream) -> Self {
        Self::from_io(stream)
    }

    /// Wrap any byte stream (useful for in-memory transports in tests).
    pub fn from_io<S>(io: S) -> Self
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (read, write) = tokio::io::split(io);
        let (notif_tx, _) = broadcast::channel(NOTIFICATION_BUFFER);
        let shared = Arc::new(Shared {
            pending: Mutex::new(HashMap::new()),
            notifications: Mutex::new(Some(notif_tx)),
            closed: CancellationToken::new(),
        });
        let (outbox, rx) = mpsc::channel(OUTBOX);
        let reader = tokio::spawn(read_loop(read, shared.clone()));
        let writer = tokio::spawn(write_loop(write, rx, shared.clone()));
        Self {
            inner: Arc::new(Inner {
                shared,
                outbox,
                next_id: AtomicU64::new(1),
                tasks: [reader, writer],
            }),
        }
    }

    /// Call `method` with `params` and decode the result into `R`.
    ///
    /// Calls may be issued concurrently from clones of one client; responses are matched by id.
    /// Use `&()` or `&serde_json::Value::Null` for methods without params.
    pub async fn call<P, R>(&self, method: &str, params: &P) -> Result<R, IpcError>
    where
        P: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let value = self.call_raw(method, params).await?;
        Ok(R::deserialize(value)?)
    }

    /// Untyped variant of [`call`](Self::call).
    pub async fn call_value(&self, method: &str, params: Value) -> Result<Value, IpcError> {
        self.call_raw(method, &params).await
    }

    async fn call_raw<P: Serialize + ?Sized>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<Value, IpcError> {
        let shared = &*self.inner.shared;
        if shared.closed.is_cancelled() {
            return Err(IpcError::Disconnected);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let line = wire::encode(&OutRequest {
            params,
            jsonrpc: JSONRPC,
            id: Some(id),
            method,
        })
        .map_err(|e| match e {
            EncodeError::TooLarge(n) => IpcError::TooLarge(n),
            EncodeError::Serde(e) => IpcError::Serde(e),
        })?;
        if !wire::params_shape_ok(&line) {
            return Err(IpcError::Protocol(
                "params must serialize to an object, an array or null".into(),
            ));
        }

        let (tx, rx) = oneshot::channel();
        lock(&shared.pending).insert(id, tx);
        let _guard = PendingGuard { shared, id };
        if shared.closed.is_cancelled() {
            return Err(IpcError::Disconnected);
        }
        // `send` is cancel-safe: the line is queued whole or not at all.
        self.inner
            .outbox
            .send(line)
            .await
            .map_err(|_| IpcError::Disconnected)?;
        rx.await.unwrap_or(Err(IpcError::Disconnected))
    }

    /// Server notifications received from now on. Every call returns an independent stream;
    /// it ends when the connection closes. A subscriber that falls more than 4096
    /// notifications behind loses the oldest ones (logged).
    pub fn notifications(&self) -> impl Stream<Item = Notification> + Send + Unpin + 'static {
        let rx = lock(&self.inner.shared.notifications)
            .as_ref()
            .map(broadcast::Sender::subscribe);
        let Some(rx) = rx else {
            return stream::empty().boxed();
        };
        let s: BoxStream<'static, Notification> = stream::unfold(rx, |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(n) => return Some((n, rx)),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(dropped = n, "notification subscriber lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        })
        .boxed();
        s
    }

    /// True once the connection is closed. A closed client never reconnects; make a new one.
    pub fn is_closed(&self) -> bool {
        self.inner.shared.closed.is_cancelled()
    }

    /// Resolves when the connection closes.
    pub async fn closed(&self) {
        self.inner.shared.closed.cancelled().await;
    }
}

async fn read_loop<R: AsyncRead + Unpin>(read: R, shared: Arc<Shared>) {
    let mut lines = wire::line_reader(read);
    loop {
        let line = tokio::select! {
            line = lines.next() => line,
            () = shared.closed.cancelled() => break,
        };
        match line {
            Some(Ok(line)) => {
                if !line.trim().is_empty() {
                    handle_incoming(&line, &shared);
                }
            }
            Some(Err(LinesCodecError::MaxLineLengthExceeded)) => {
                tracing::warn!("closing connection: server sent an oversized message");
                break;
            }
            Some(Err(LinesCodecError::Io(e))) => {
                tracing::debug!(error = %e, "connection read failed");
                break;
            }
            None => break,
        }
    }
    shared.close();
}

fn handle_incoming(line: &str, shared: &Shared) {
    match wire::decode(line) {
        Incoming::Response { id, result } => {
            let Some(id) = id.as_u64() else {
                // Errors about unparseable requests carry `id: null`; nothing to correlate.
                if let Err(e) = result {
                    tracing::warn!(error = %e, "server reported an uncorrelated error");
                }
                return;
            };
            if let Some(tx) = lock(&shared.pending).remove(&id) {
                let _ = tx.send(result.map_err(IpcError::Rpc));
            }
        }
        Incoming::Request {
            id: None,
            method,
            params,
        } => {
            if let Some(tx) = lock(&shared.notifications).as_ref() {
                // No subscribers is fine: the notification is simply dropped.
                let _ = tx.send(Notification { method, params });
            }
        }
        Incoming::Request {
            id: Some(_),
            method,
            ..
        } => {
            tracing::debug!(%method, "ignoring server-to-client request");
        }
        Incoming::Invalid { error, .. } => {
            tracing::warn!(error = %error, "ignoring invalid message from server");
        }
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut write: W,
    mut rx: mpsc::Receiver<String>,
    shared: Arc<Shared>,
) {
    let mut buf = Vec::new();
    loop {
        let line = tokio::select! {
            line = rx.recv() => line,
            () = shared.closed.cancelled() => None,
        };
        let Some(line) = line else { break };
        if let Err(e) = wire::write_batch(&mut write, &mut buf, line, &mut rx).await {
            tracing::debug!(error = %e, "connection write failed");
            break;
        }
    }
    shared.close();
    let _ = write.shutdown().await;
}
