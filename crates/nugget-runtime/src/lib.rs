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
//! 3. **Ready** once `/health` answers `ok` from the server we spawned: the child must be alive
//!    before and after the probe, and on Linux every listener reachable on `127.0.0.1:<port>`
//!    must belong to the child's process group (see *Security*). [`Runtime::ensure`] then
//!    returns an `openai_compatible` [`nugget_infer::ProviderConfig`]
//!    (`http://127.0.0.1:<port>/v1`, the model id as alias, tiers by role, locality
//!    `on_device`). The start runs as a detached task: a caller that gives up (a timeout
//!    dropping its `ensure` future) does not cancel it, and later calls join it.
//! 4. **Supervise.** Restart with exponential backoff after a crash (same port when still
//!    free, but a new API key, so configs handed out before must be refreshed through
//!    [`Runtime::ensure`] / [`Runtime::details`]), optional idle stop, [`Runtime::stop`],
//!    [`Runtime::status`]. A dead server is reported as not ready as soon as the status is read,
//!    and a stopping one as soon as the stop begins: no config for a port the server no longer
//!    holds. Dropping the last [`Runtime`] kills every server's process group / job, including
//!    one still starting.
//!
//! ## Security
//! - The server binds **only** to `127.0.0.1`; `--host`/`--port` and other dangerous flags cannot
//!   be overridden through [`Tuning::extra_args`].
//! - Every server process requires a **random 256-bit API key**, new for every (re)start, so
//!   other local users cannot use the port (only `/health` is unauthenticated), and a key that
//!   leaked while the port was unguarded does not open the next server. The key reaches the
//!   child through the `LLAMA_API_KEY` environment variable (not argv, so not in `ps`), is
//!   redacted from captured logs, and is never logged. Configs carry only a reference to the key
//!   generation they were issued for (`env:NUGGET_RUNTIME_API_KEY_<ROLE>_<n>`) that
//!   [`RuntimeSecrets`] resolves from memory; older generations fail closed.
//! - **Who is on the port.** The port is picked by binding and releasing it, so another local
//!   user could bind it before the server does and answer `/health` itself, receiving the key
//!   and every prompt. Before trusting a port the supervisor checks the listener: on **Linux**
//!   the listening sockets reachable on `127.0.0.1:<port>` (`/proc/net/tcp{,6}`) must all be
//!   open in the child or its process group (`/proc/<pid>/fd`), otherwise the start fails. On
//!   **macOS and Windows** this is not verified (it needs `libproc` / `GetExtendedTcpTable`
//!   FFI): only the child's liveness around the probe is checked, and key rotation limits what
//!   a squatter can do with what it sees. llama.cpp can listen on a Unix socket (`--host
//!   <path>.sock`), which would remove the race on Unix, but `nugget-infer`'s HTTP clients do
//!   not support Unix-socket endpoints yet.
//! - `--offline`, `--no-webui` and `--no-slots`: no downloads by the server, no browser UI, no
//!   slot inspection.

mod args;
mod health;
mod listener;
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
