//! `nugget-runtime`: local inference through llama.cpp's `llama-server`.
//!
//! [`Runtime`] supervises one server per [`RuntimeRole`] (chat, embedding):
//!
//! 1. **Install.** The `llama-server` build comes from the signed manifest (`runtimes`), picked
//!    for this OS, CPU and GPU ([`nugget_models::runtime_candidates_for`]), downloaded and
//!    SHA-256-verified through the model store, and unpacked by a hostile-archive-safe extractor
//!    ([`nugget_models::archive`]).
//! 2. **Spawn** on `127.0.0.1:<free port>` with arguments derived from the role and hardware
//!    ([`LaunchPlan`]): context size, GPU layers, parallel slots, `--embedding` for embeddings.
//!    The child gets a scrubbed environment, runs in its own process group (Unix) or a
//!    kill-on-close job object (Windows), and its stdout/stderr go to a rolling log under
//!    `data/logs/`.
//! 3. **Ready** once `/health` answers `ok`; [`Runtime::ensure`] then returns an
//!    `openai_compatible` [`nugget_infer::ProviderConfig`] (`http://127.0.0.1:<port>/v1`, the
//!    model id as alias, tiers by role, locality `on_device`).
//! 4. **Supervise.** Restart with exponential backoff after a crash (same port, so handed-out
//!    configs stay valid), optional idle stop, [`Runtime::stop`], [`Runtime::status`]. Dropping
//!    the last [`Runtime`] kills every server's process group / job.
//!
//! ## Security
//! - The server binds **only** to `127.0.0.1`; `--host`/`--port` and other dangerous flags cannot
//!   be overridden through [`Tuning::extra_args`].
//! - Every server requires a **random 256-bit API key**, generated per [`Runtime`], so other local
//!   users cannot use the port (only `/health` is unauthenticated). The key reaches the child
//!   through the `LLAMA_API_KEY` environment variable (not argv, so not in `ps`), is redacted
//!   from captured logs, and is never logged. Configs carry only a reference
//!   (`env:NUGGET_RUNTIME_API_KEY_<ROLE>`) that [`RuntimeSecrets`] resolves from memory.
//! - `--offline`, `--no-webui` and `--no-slots`: no downloads by the server, no browser UI, no
//!   slot inspection.

mod args;
mod health;
mod logs;
mod orphan;
mod process;
mod secrets;
mod supervisor;

pub use args::{GpuLayers, LaunchPlan, Pooling, Tuning};
pub use secrets::{RuntimeSecrets, api_key_ref};
pub use supervisor::{
    CHAT_PROVIDER_ID, EMBEDDING_PROVIDER_ID, Runtime, RuntimeDetails, RuntimeOptions, choose_port,
    provider_id,
};

pub use nugget_core::ipc::{RuntimeInfo, RuntimeRole, RuntimeState};
