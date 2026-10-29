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

mod client;
mod error;
mod server;
mod transport;
mod wire;

pub use client::Client;
pub use error::{IpcError, RpcError, code, parse_params, to_result};
pub use server::{
    DEFAULT_MAX_IN_FLIGHT, Handler, Notifier, ServerOptions, serve, serve_until, serve_with,
};
pub use transport::{LOCK_FILE, bind, connect_stream, lock_path, socket_display, socket_name};
pub use wire::Notification;
#[doc(hidden)]
pub use wire::fuzz_frames;

/// Re-exported so dependents name the same listener/stream types without depending on
/// `interprocess` directly.
pub use interprocess::local_socket::tokio::{Listener, Stream};

/// Maximum size of one message (one line, excluding the newline): 16 MiB.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
