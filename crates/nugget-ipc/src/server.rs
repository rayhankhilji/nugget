//! Server side: accept connections and dispatch requests to a [`Handler`].

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

/// Handles every request on every connection. One `call` runs per request, concurrently.
#[async_trait]
pub trait Handler: Send + Sync + 'static {
    /// Answer `method`. `params` is `Value::Null` when the request had none. Use `notify` to
    /// stream notifications to the calling connection, also after returning (clone it into a
    /// spawned task).
    async fn call(&self, method: &str, params: Value, notify: Notifier) -> Result<Value, RpcError>;
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
        let params = serde_json::to_value(params)?;
        let line = wire::encode(&OutRequest {
            jsonrpc: JSONRPC,
            id: None,
            method,
            params: &params,
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
    let mut connections = JoinSet::new();
    let mut consecutive_errors = 0u32;
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok(stream) => {
                    consecutive_errors = 0;
                    connections.spawn(handle_connection(stream, handler.clone()));
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

async fn handle_connection(stream: Stream, handler: Arc<dyn Handler>) {
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
    let mut lines = wire::line_reader(read);
    let mut calls = JoinSet::new();

    loop {
        tokio::select! {
            line = lines.next() => match line {
                Some(Ok(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    dispatch(&line, &handler, &notifier, &tx, &mut calls).await;
                }
                Some(Err(LinesCodecError::MaxLineLengthExceeded)) => {
                    tracing::warn!("closing connection: incoming message too large");
                    let err = RpcError::too_large(crate::MAX_MESSAGE_BYTES + 1);
                    let _ = tx.send(wire::response_line_or_error(&Value::Null, &Err(err))).await;
                    break;
                }
                Some(Err(LinesCodecError::Io(e))) => {
                    if e.kind() == io::ErrorKind::InvalidData {
                        let err = RpcError::parse_error("message is not valid UTF-8");
                        let _ = tx.send(wire::response_line_or_error(&Value::Null, &Err(err))).await;
                    } else {
                        tracing::debug!(error = %e, "connection read failed");
                    }
                    break;
                }
                None => break,
            },
            Some(_) = calls.join_next(), if !calls.is_empty() => {}
            () = closed.cancelled() => break, // writer failed
        }
    }

    // Peer gone or protocol violation: drop in-flight calls, flush what is queued, close.
    calls.shutdown().await;
    closed.cancel();
    drop(tx);
    let _ = tokio::time::timeout(Duration::from_secs(5), writer).await;
}

async fn dispatch(
    line: &str,
    handler: &Arc<dyn Handler>,
    notifier: &Notifier,
    tx: &mpsc::Sender<String>,
    calls: &mut JoinSet<()>,
) {
    match wire::decode(line) {
        Incoming::Request { id, method, params } => {
            let handler = handler.clone();
            let notifier = notifier.clone();
            let tx = tx.clone();
            calls.spawn(async move {
                let call = AssertUnwindSafe(handler.call(&method, params, notifier)).catch_unwind();
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
        Incoming::Invalid { id, error } => {
            let _ = tx
                .send(wire::response_line_or_error(&id, &Err(error)))
                .await;
        }
        Incoming::Response { .. } => {
            tracing::debug!("ignoring response sent to the server");
        }
    }
}

async fn write_loop<W>(mut write: W, mut rx: mpsc::Receiver<String>, closed: CancellationToken)
where
    W: tokio::io::AsyncWrite + Unpin,
{
    loop {
        tokio::select! {
            biased;
            msg = rx.recv() => match msg {
                Some(line) => {
                    if let Err(e) = wire::write_line(&mut write, &line).await {
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
                    if wire::write_line(&mut write, &line).await.is_err() {
                        return;
                    }
                }
                break;
            }
        }
    }
    let _ = write.shutdown().await;
}
