//! `nugget-runtime`: local inference through llama.cpp's `llama-server`.
//!
//! [`Runtime`] supervises one server per [`RuntimeRole`] (chat, embedding, rerank, draft):
//!
//! 1. **Install** the build the signed manifest lists for this OS, CPU and GPU
//!    ([`nugget_models::runtime_candidates_for`]), SHA-256-verified and unpacked by a
//!    hostile-archive-safe extractor.
//! 2. **Spawn** it on a Unix socket in a private directory (`data/run/<role>.sock`, mode 0700)
//!    or on `127.0.0.1:<free port>`, with arguments from the role and hardware ([`LaunchPlan`]),
//!    a scrubbed environment, its own process group (Unix) or kill-on-close job (Windows), and
//!    stdout/stderr in a rolling, redacted log under `data/logs/`.
//! 3. **Ready** once `/health` answers `ok` from the server we spawned; [`Runtime::ensure`]
//!    returns an on-device `openai_compatible` [`nugget_infer::ProviderConfig`]. The start runs
//!    detached: a caller that gives up does not cancel it, and later calls join it.
//! 4. **Supervise**: restart with exponential backoff after a crash or hang (with a new API
//!    key, so configs must be refreshed through [`Runtime::ensure`] / [`Runtime::details`]),
//!    optional idle stop. A dead or stopping server is never reported ready. Dropping the last
//!    [`Runtime`] kills every server's process group / job, including one still starting.
//!
//! ## Security
//! - Listeners are a 0700 Unix socket directory owned by the user, or `127.0.0.1`; `--host`,
//!   `--port` and other dangerous flags cannot be set through [`Tuning::extra_args`].
//! - Every (re)start gets a new random 256-bit API key, passed in `LLAMA_API_KEY` (not argv),
//!   redacted from logs, never logged. Configs carry only a reference to the key generation
//!   (`env:NUGGET_RUNTIME_API_KEY_<ROLE>_<n>`) that [`RuntimeSecrets`] resolves from memory;
//!   older generations fail closed.
//! - A TCP port could be squatted by another local user between picking and binding it: on
//!   Linux every listener reachable on `127.0.0.1:<port>` must belong to the child's process
//!   group (`/proc`), else the start fails. Elsewhere only the child's liveness around the probe
//!   is checked, and key rotation limits what a squatter sees.
//! - `--offline`, `--no-webui` and `--no-slots`: no downloads, no browser UI, no slot inspection.

mod args;
mod health;
mod listener;
mod logs;
mod orphan;
mod process;
mod secrets;
mod socket;
mod supervisor;

pub use args::{GpuLayers, LaunchPlan, Pooling, Tuning};
pub use secrets::{RuntimeSecrets, api_key_ref};
pub use supervisor::{
    CHAT_PROVIDER_ID, DRAFT_PROVIDER_ID, EMBEDDING_PROVIDER_ID, RERANK_PROVIDER_ID, Runtime,
    RuntimeDetails, RuntimeOptions, choose_port, provider_id,
};

pub use nugget_core::ipc::{RuntimeInfo, RuntimeRole, RuntimeState};
