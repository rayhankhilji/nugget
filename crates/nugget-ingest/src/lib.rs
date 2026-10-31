//! `nugget-ingest`: the ingest runtime.
//!
//! ```text
//! sources.toml ─▶ Ingest ─▶ one supervisor per enabled source
//!                              │  sync(cursor) ─▶ watch | sleep(poll_interval)
//!                              │  backoff + jitter on errors, NeedsAuth on Unauthorized
//!                              ▼
//!          connector ──Change──▶ bounded queue ─▶ worker (in order)
//!                                                   extract (blocking, timeout)
//!                                                   PII scan ─▶ chunk ─▶ embed (batched,
//!                                                   local by default)
//!                                                   store: item + chunks + version (atomic)
//!                                                   ─▶ triage queue (searchable already)
//!          triage job (background, batched, local only, yields to interactive model use)
//!                                                   ─▶ junk / sensitive labels (metadata only)
//!                                checkpoint(cursor) ─▶ set_cursor after everything before it
//! ```
//!
//! # Usage
//! [`Ingest::start`] (or [`Ingest::builder`] to inject a [`SecretStore`] or [`PowerPolicy`])
//! loads `<config>/sources.toml` and starts every enabled source. The handle exposes the
//! registry operations behind the `sources.*` IPC methods ([`Ingest::add`],
//! [`Ingest::update`], [`Ingest::remove`], [`Ingest::list`], [`Ingest::sync_now`],
//! [`Ingest::begin_auth`], [`Ingest::set_password`]), [`Ingest::status`] for `ingest.status`, and
//! [`Ingest::subscribe`] for `ingest.progress` / `sources.changed` notifications. Call
//! [`Ingest::shutdown`] before exit.
//!
//! # Guarantees
//! - **Ordering.** Changes of one source are applied in the order sent.
//! - **Cursor after persistence.** A cursor (from `checkpoint` or returned by `sync`) is stored
//!   only after every change sent before it was written. If a change failed to persist during
//!   the pass, the checkpoint fails and the pass is retried from the last stored cursor. A crash
//!   therefore never skips data; at worst changes are re-applied (cheap: version skip).
//! - **Crash-safe items.** An item, its chunks and its version are written in one transaction
//!   (`Store::upsert_item_with_chunks`): a failed or interrupted write leaves the previous item
//!   and chunks intact, never an item without chunks.
//! - **Late embeddings.** Items indexed text-only (no embedder was routable, or embedding
//!   failed) are embedded later by a background job: at start, on [`Ingest::reembed_now`] and
//!   every [`IngestConfig::reembed_interval_ms`], under the same privacy rules. Embedders that
//!   return NaN or infinite values are treated as failed (text-only, retried later).
//! - **Authorization.** An OAuth flow belongs to its source: removing, reconfiguring or
//!   reloading the source (or starting another flow) cancels it first, and a removed source can
//!   no longer write credentials.
//! - **Bounded errors.** `last_error`, item errors and progress errors are capped at
//!   [`MAX_ERROR_CHARS`] characters (see [`cap_error`]).
//! - **Search before triage.** Items are searchable as soon as they are written; triage labels
//!   them later in the background and never removes an item (or its chunks) from search. See
//!   the `triage` module docs and `ingest.status.triage` for progress.
//! - **Privacy.** Items with PII findings (or flagged by triage) are *sensitive*; so is every
//!   item still waiting for triage or whose triage failed (fail closed). Triage never
//!   uses `Cloud` providers; embeddings use `Cloud` providers only when
//!   [`IngestConfig::allow_cloud_embeddings`] is set **and** the item is not sensitive.
//! - **Isolation.** A bad item is recorded (see [`Ingest::errors`]) and skipped; it never stops
//!   its source. Sources never block each other.
//! - **Backpressure.** `ChangeSink::send` waits when the source's queue
//!   ([`IngestConfig::queue_capacity`]) is full.
//! - **Power.** Backfills (passes without a cursor) pause while the [`PowerPolicy`] reports a
//!   constraint; incremental, live and user-requested work always runs.
//!
//! # Item metadata
//! Items are stored with `metadata` = `{ source: <connector metadata>, url?, mime, extracted?,
//! links?, tags?, sensitive, pii?: {card?, ssn?, iban?, secret?} (counts only), junk, triage?:
//! {state: "pending" | "done" | "skipped" | "failed", junk?, sensitive?, provider?, reason?,
//! error?}, truncated?, extract_error?, embed_error?, embedding_provider? }`.

mod ai;
mod config;
mod ingest;
mod known;
mod metrics;
mod pii;
mod pipeline;
mod power;
mod priority;
mod reembed;
mod secrets;
mod sources;
mod supervisor;
mod triage;
mod util;

pub use ai::{FEATURE_TRIAGE, Triage};
pub use config::IngestConfig;
pub use ingest::{AUTH_TIMEOUT, Ingest, IngestBuilder, IngestEvent, ItemError, SettingsResolver};
pub use known::StoreKnownItems;
pub use pii::{PiiFindings, scan as scan_pii};
pub use power::{NoPowerPolicy, PowerPolicy, SystemPowerPolicy};
pub use priority::{Foreground, ForegroundGuard};
pub use reembed::ReembedStats;
pub use secrets::{KeyringSecrets, MemorySecrets, account_belongs_to};
pub use util::{MAX_ERROR_CHARS, cap_error};

pub use nugget_connector::{AuthFlow, SecretStore};

/// File name of the source registry under `Paths::config`.
pub const SOURCES_FILE: &str = sources::FILE_NAME;
