//! `nugget-ingest`: source registry, per-source supervisors and the indexing pipeline.
//!
//! Privacy: items with PII, items the sensitivity rules flag, items a connector marks
//! `metadata.sensitive = true` and items triage flags are sensitive; unflagged items stay
//! sensitive until local triage clears them (fail closed). Triage never uses `Cloud` providers;
//! embeddings use them only when allowed **and** the item is not sensitive.

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
mod sensitivity;
mod sensitivity_terms;
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
pub use priority::{BACKGROUND_FEATURES, Foreground, ForegroundGuard, is_interactive};
pub use reembed::{OnReindexed, ReembedStats};
pub use secrets::{KeyringSecrets, MemorySecrets, account_belongs_to};
pub use util::{MAX_ERROR_CHARS, cap_error};

pub use nugget_connector::{AuthFlow, SecretStore};

/// File name of the source registry under `Paths::config`.
pub const SOURCES_FILE: &str = sources::FILE_NAME;
