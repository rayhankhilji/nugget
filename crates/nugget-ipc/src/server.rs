//! Server side: accept connections and dispatch requests to a [`Handler`].
//!
//! Each request runs as its own task, so one connection's calls run concurrently and answer
//! in completion order. A connection runs at most [`ServerOptions::max_in_flight`] calls at
//! once; past that it keeps reading and answers each extra request with
//! [`code::SERVER_BUSY`](crate::code::SERVER_BUSY) (it never stops reading, which would
//! deadlock a call waiting for a later request on the same connection). Control requests
//! (cancel, confirm, answer, kill; see [`DEFAULT_CONTROL_METHODS`]) get [`CONTROL_RESERVE`]
//! extra slots so they still run when every regular slot is taken. The client does not retry
//! busy calls itself: a busy call never ran, so the caller may retry once one of its
//! earlier calls on that connection has finished. Memory per connection stays bounded: running
//! calls by the caps above, queued output by a fixed-size outbox (when it is full, because the
//! client is not reading, the server stops reading too).

use std::future::Future;
use std::io;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::{FutureExt, StreamExt};
use interprocess::local_socket::tokio::{Listener, Stream};
use interprocess::local_socket::traits::tokio::Listener as _;
use serde::Serialize;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::codec::LinesCodecError;
use tokio_util::sync::CancellationToken;

use crate::error::{IpcError, RpcError};
use crate::wire::{self, EncodeError, Incoming, JSONRPC, OutRequest};

/// Outgoing messages queued per connection before [`Notifier::send`] applies backpressure.
const OUTBOX: usize = 1024;

/// Default for [`ServerOptions::max_in_flight`].
pub const DEFAULT_MAX_IN_FLIGHT: usize = 64;

/// Extra slots per connection that only control requests may use, on top of
/// [`ServerOptions::max_in_flight`].
pub const CONTROL_RESERVE: usize = 16;

/// Default for [`ServerOptions::control_methods`]: requests that release or stop work already
/// running (cancellation, confirmations, answers, the kill switch, browser host replies). Their
/// handlers return promptly, and the calls they unblock may be holding every regular slot.
pub const DEFAULT_CONTROL_METHODS: &[&str] = &[
    nugget_core::ipc::method::CHAT_CANCEL,
    nugget_core::ipc::method::DRAFT_CANCEL,
    nugget_core::ipc::agent_method::AGENT_CANCEL,
    nugget_core::ipc::agent_method::AGENT_CONFIRM,
    nugget_core::ipc::agent_method::AGENT_ANSWER,
    nugget_core::ipc::agent_method::POLICY_KILL,
    nugget_core::ipc::agent_method::BROWSER_HOST_DELIVER,
];

/// Tuning for [`serve_with`]. [`serve`] and [`serve_until`] use [`ServerOptions::default`].
#[derive(Debug, Clone)]
pub struct ServerOptions {
    max_in_flight: usize,
    control: Arc<[Box<str>]>,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            max_in_flight: DEFAULT_MAX_IN_FLIGHT,
            control: DEFAULT_CONTROL_METHODS.iter().map(|&m| m.into()).collect(),
        }
    }
}

impl ServerOptions {
    /// Cap on requests one connection may have running at once (default
    /// [`DEFAULT_MAX_IN_FLIGHT`]; 0 is treated as 1). The server keeps reading at the cap:
    /// a further request is answered at once with [`code::SERVER_BUSY`](crate::code::SERVER_BUSY)
    /// without running (a further client notification is dropped), except that
    /// [control methods](Self::control_methods) may use [`CONTROL_RESERVE`] extra slots.
    /// So at most `max_in_flight + CONTROL_RESERVE` calls run per connection, and a call may
    /// safely wait for a later control request on its own connection.
    #[must_use]
    pub fn max_in_flight(mut self, max: usize) -> Self {
        self.max_in_flight = max.max(1);
        self
    }

    /// Replace the control methods (default [`DEFAULT_CONTROL_METHODS`]). A control request
    /// is still run when the connection is at its in-flight cap, in one of
    /// [`CONTROL_RESERVE`] extra slots, so a call waiting for it can be released. Only list
    /// methods whose handlers return promptly.
    #[must_use]
    pub fn control_methods<I, S>(mut self, methods: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Box<str>>,
    {
        self.control = methods.into_iter().map(Into::into).collect();
        self
    }

    fn is_control(&self, method: &str) -> bool {
        self.control.iter().any(|m| &**m == method)
    }
}

/// Handles every request on every connection. One `call` runs per request, concurrently.
#[async_trait]
pub trait Handler: Send + Sync + 'static {
    /// Answer `method`. `params` is `Value::Null` when the request had none. Use `notify` to
    /// stream notifications to the calling connection, also after returning (clone it into a
    /// spawned task).
    async fn call(&self, method: &str, params: Value, notify: Notifier) -> Result<Value, RpcError>;

    /// Called once per connection, when it opens and before any of its requests is read, with
    /// that connection's notifier (for example to subscribe it to broadcast notifications).
    /// The notifier fails with [`IpcError::Disconnected`] once the connection is gone. Default:
    /// nothing.
    fn connected(&self, _notify: &Notifier) {}
}

/// Pushes notifications to one connection. Cheap to clone.
#[derive(Clone)]
pub struct Notifier {
    tx: mpsc::Sender<String>,
    closed: CancellationToken,
}

impl Notifier {
    /// Send a notification. Waits if the client is slow to read. Fails with
    /// [`IpcError::Disconnected`] once the connection is gone, so streaming tasks can stop.
    pub async fn send<P: Serialize + ?Sized>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<(), IpcError> {
        if self.closed.is_cancelled() {
            return Err(IpcError::Disconnected);
        }
        let line = wire::encode(&OutRequest {
            params,
            jsonrpc: JSONRPC,
            id: None,
            method,
        })
        .map_err(|e| match e {
            EncodeError::TooLarge(n) => IpcError::TooLarge(n),
            EncodeError::Serde(e) => IpcError::Serde(e),
        })?;
        tokio::select! {
            res = self.tx.send(line) => res.map_err(|_| IpcError::Disconnected),
            () = self.closed.cancelled() => Err(IpcError::Disconnected),
        }
    }

    /// True once the connection has closed.
    pub fn is_closed(&self) -> bool {
        self.closed.is_cancelled()
    }

    /// Resolves when the connection closes.
    pub async fn closed(&self) {
        self.closed.cancelled().await;
    }
}

/// Serve forever (until the listener fails).
pub async fn serve(listener: Listener, handler: Arc<dyn Handler>) -> io::Result<()> {
    serve_until(listener, handler, std::future::pending()).await
}

/// Serve until `shutdown` resolves. On shutdown every connection is closed and in-flight
/// handler futures are dropped; clients observe [`IpcError::Disconnected`].
pub async fn serve_until<F>(
    listener: Listener,
    handler: Arc<dyn Handler>,
    shutdown: F,
) -> io::Result<()>
where
    F: Future<Output = ()>,
{
    serve_with(listener, handler, ServerOptions::default(), shutdown).await
}

/// [`serve_until`] with explicit [`ServerOptions`].
///
/// On Unix every accepted connection whose peer is not running as this process's effective
/// user is logged and dropped before anything is read from it.
pub async fn serve_with<F>(
    listener: Listener,
    handler: Arc<dyn Handler>,
    options: ServerOptions,
    shutdown: F,
) -> io::Result<()>
where
    F: Future<Output = ()>,
{
    let mut connections = JoinSet::new();
    let mut consecutive_errors = 0u32;
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok(stream) => {
                    consecutive_errors = 0;
                    if let Err(e) = crate::transport::verify_client(&stream) {
                        tracing::warn!(error = %e, "rejected connection");
                        continue;
                    }
                    connections.spawn(handle_connection(
                        stream,
                        handler.clone(),
                        options.clone(),
                    ));
                }
                Err(e) => {
                    consecutive_errors += 1;
                    tracing::warn!(error = %e, "accept failed");
                    if consecutive_errors >= 100 {
                        connections.shutdown().await;
                        return Err(e);
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    connections.shutdown().await;
    Ok(())
}

async fn handle_connection(stream: Stream, handler: Arc<dyn Handler>, options: ServerOptions) {
    let (read, write) = tokio::io::split(stream);
    let (tx, rx) = mpsc::channel::<String>(OUTBOX);
    let closed = CancellationToken::new();
    // Dropping the guard (also on abort during server shutdown) marks the connection closed,
    // which stops the writer and fails every outstanding `Notifier`.
    let _close_guard = closed.clone().drop_guard();
    let writer = tokio::spawn(write_loop(write, rx, closed.clone()));
    let notifier = Notifier {
        tx: tx.clone(),
        closed: closed.clone(),
    };
    handler.connected(&notifier);
    let mut lines = wire::line_reader(read);
    let mut conn = Connection {
        handler,
        notifier,
        tx,
        calls: JoinSet::new(),
        options,
    };

    // The reader never pauses for the in-flight cap (see `Connection::dispatch`), so a call
    // that waits for a later request on this connection can always receive it. With nothing
    // arriving and no call running, this loop is parked on the socket and the close token:
    // no timers, no wakeups.
    loop {
        tokio::select! {
            line = lines.next() => match line {
                Some(Ok(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    conn.dispatch(&line).await;
                }
                Some(Err(LinesCodecError::MaxLineLengthExceeded)) => {
                    tracing::warn!("closing connection: incoming message too large");
                    let err = RpcError::too_large(crate::MAX_MESSAGE_BYTES + 1);
                    conn.reply(&Value::Null, &Err(err)).await;
                    break;
                }
                Some(Err(LinesCodecError::Io(e))) => {
                    if e.kind() == io::ErrorKind::InvalidData {
                        let err = RpcError::parse_error("message is not valid UTF-8");
                        conn.reply(&Value::Null, &Err(err)).await;
                    } else {
                        tracing::debug!(error = %e, "connection read failed");
                    }
                    break;
                }
                None => break,
            },
            Some(_) = conn.calls.join_next(), if !conn.calls.is_empty() => {}
            () = closed.cancelled() => break, // writer failed
        }
    }

    // Peer gone or protocol violation: drop in-flight calls, flush what is queued, close.
    conn.calls.shutdown().await;
    closed.cancel();
    drop(conn);
    let _ = tokio::time::timeout(Duration::from_secs(5), writer).await;
}

/// Per-connection dispatch state.
struct Connection {
    handler: Arc<dyn Handler>,
    notifier: Notifier,
    tx: mpsc::Sender<String>,
    calls: JoinSet<()>,
    options: ServerOptions,
}

impl Connection {
    /// Queue a response. Waits only while the outbox is full, i.e. while the client is not
    /// reading its responses; that bounds what a flooding client can make the server buffer.
    async fn reply(&self, id: &Value, result: &Result<Value, RpcError>) {
        let _ = self.tx.send(wire::response_line_or_error(id, result)).await;
    }

    async fn dispatch(&mut self, line: &str) {
        match wire::decode(line) {
            Incoming::Request { id, method, params } => {
                let control = self.options.is_control(&method);
                let limit = if control {
                    self.options.max_in_flight + CONTROL_RESERVE
                } else {
                    self.options.max_in_flight
                };
                if self.calls.len() >= limit {
                    // Calls that finished but were not reaped yet still count in `len`.
                    while self.calls.try_join_next().is_some() {}
                }
                if self.calls.len() >= limit {
                    // Refuse rather than stop reading: a paused reader deadlocks any call
                    // that waits for a later request on this connection.
                    match id {
                        Some(id) => {
                            let max = self.options.max_in_flight;
                            self.reply(&id, &Err(RpcError::busy(max))).await;
                        }
                        None => {
                            tracing::warn!(%method, "dropping notification: too many in flight")
                        }
                    }
                    return;
                }
                let handler = self.handler.clone();
                let notifier = self.notifier.clone();
                let tx = self.tx.clone();
                self.calls.spawn(async move {
                    let call =
                        AssertUnwindSafe(handler.call(&method, params, notifier)).catch_unwind();
                    let result = call.await.unwrap_or_else(|_| {
                        tracing::error!(%method, "handler panicked");
                        Err(RpcError::internal(format!(
                            "handler for `{method}` panicked"
                        )))
                    });
                    // Requests without an id are client notifications: never answered.
                    if let Some(id) = id {
                        let _ = tx.send(wire::response_line_or_error(&id, &result)).await;
                    }
                });
            }
            Incoming::Invalid { id, error } => self.reply(&id, &Err(error)).await,
            Incoming::Response { .. } => {
                tracing::debug!("ignoring response sent to the server");
            }
        }
    }
}

async fn write_loop<W>(mut write: W, mut rx: mpsc::Receiver<String>, closed: CancellationToken)
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut buf = Vec::new();
    loop {
        tokio::select! {
            biased;
            msg = rx.recv() => match msg {
                Some(line) => {
                    if let Err(e) = wire::write_batch(&mut write, &mut buf, line, &mut rx).await {
                        tracing::debug!(error = %e, "connection write failed");
                        closed.cancel();
                        return;
                    }
                }
                None => break,
            },
            () = closed.cancelled() => {
                // Flush whatever was queued before the close (e.g. a final error response).
                while let Ok(line) = rx.try_recv() {
                    if wire::write_batch(&mut write, &mut buf, line, &mut rx).await.is_err() {
                        return;
                    }
                }
                break;
            }
        }
    }
    let _ = write.shutdown().await;
}
