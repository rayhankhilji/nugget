//! Connector API: how sources (files, mail, calendars, ...) feed the ingest pipeline.
//!
//! A [`Connector`] turns one configured source into a stream of [`Change`]s. The ingest pipeline
//! owns everything after that: extraction, chunking, embedding, storage, scheduling and cursor
//! persistence. Connectors never touch the store directly.
//!
//! ## Contract
//! - [`Connector::sync`] runs one pass starting from the last committed cursor (`None` on the
//!   first run) and returns the cursor to resume from next time. Long passes call
//!   [`ChangeSink::checkpoint`] periodically; the pipeline commits a checkpoint only after every
//!   change sent before it is durably applied, so a crash never skips data.
//! - Changes for one source are applied in the order sent. [`ChangeSink::send`] applies
//!   backpressure; just `.await` it.
//! - Every network exchange is recorded through [`ConnectorContext::egress`] with feature
//!   `sync:<kind>` (use [`http::MeteredHttp`] for HTTP). Cross-host redirects are not followed.
//! - Credentials live only in [`ConnectorContext::secrets`] (the OS keychain), under the account
//!   names in [`nugget_core::secrets`]. Missing or rejected credentials → `Error::Unauthorized`.
//! - Honour `cancel` promptly: return `Error::Cancelled`.

pub mod health;
pub mod http;
mod registry;
pub mod retry;
#[cfg(feature = "testkit")]
pub mod testkit;
mod types;

pub use registry::{AuthKind, ConnectorFactory, KindInfo, Registry};
pub use types::{
    AuthFlow, Change, ChangeSink, Connector, ConnectorContext, Content, Document, KnownItems,
    SecretStore,
};

pub use tokio_util::sync::CancellationToken;

#[cfg(test)]
mod tests;
