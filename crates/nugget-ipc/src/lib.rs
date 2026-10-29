//! Local IPC between the Nugget app and `nuggetd`.
//!
//! JSON-RPC 2.0, one JSON object per line (newline-delimited), over a local socket: a Unix
//! domain socket in a user-private 0700 directory on macOS/Linux, a per-user named pipe on
//! Windows. Method and notification names, and their payload types, live in
//! [`nugget_core::ipc`].
//!
//! ```no_run
//! # async fn demo() -> Result<(), nugget_ipc::IpcError> {
//! use nugget_core::ipc::{method, StatusResponse};
//! let paths = nugget_core::paths::Paths::discover().expect("paths");
//! let client = nugget_ipc::Client::connect(&paths).await?;
//! let status: StatusResponse = client.call(method::STATUS, &()).await?;
//! # let _ = status; Ok(()) }
//! ```
//!
//! Server side: [`bind`] a listener and [`serve`] it with an `Arc<dyn` [`Handler`]`>`.
//!
//! In-flight semantics: each connection runs at most [`ServerOptions::max_in_flight`]
//! requests at once (default [`DEFAULT_MAX_IN_FLIGHT`]). Further requests are answered right
//! away with [`code::SERVER_BUSY`] (they did not run; [`IpcError::is_busy`]) instead of
//! waiting unread, so no call can deadlock by waiting for a later request on its own
//! connection. Control requests ([`DEFAULT_CONTROL_METHODS`]: cancellations, confirmations,
//! answers, the kill switch) may use [`CONTROL_RESERVE`] extra slots. [`Client`] does not
//! retry busy calls; callers that issue many concurrent long calls on one connection should.

mod client;
mod error;
mod server;
mod transport;
mod wire;

pub use client::Client;
pub use error::{IpcError, RpcError, code, parse_params, to_result};
pub use server::{
    CONTROL_RESERVE, DEFAULT_CONTROL_METHODS, DEFAULT_MAX_IN_FLIGHT, Handler, Notifier,
    ServerOptions, serve, serve_until, serve_with,
};
pub use transport::{LOCK_FILE, bind, connect_stream, lock_path, socket_display, socket_name};
pub use wire::Notification;

/// Re-exported so dependents name the same listener/stream types without depending on
/// `interprocess` directly.
pub use interprocess::local_socket::tokio::{Listener, Stream};

/// Maximum size of one message (one line, excluding the newline): 16 MiB.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
