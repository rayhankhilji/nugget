//! The [`Runtime`] supervisor: one `llama-server` per [`RuntimeRole`].

use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use nugget_core::capability::Tier;
use nugget_core::clock::Stopwatch;
use nugget_core::egress::EgressSink;
use nugget_core::ids::ProviderId;
use nugget_core::ipc::{RuntimeInfo, RuntimeRole, RuntimeState};
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use nugget_infer::{ProviderConfig, Secret};
use nugget_models::{
    Hardware, Manifest, ModelEntry, ModelStore, Progress, RuntimeAccel, RuntimeEntry, current_arch,
    current_os, default_manifest, detect_hardware, runtime_candidates_for,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::args::{LaunchPlan, Tuning};
use crate::health::{
    Health, ProcSlot, StartFailure, Target, activity, activity_within, loopback_client,
    probe_within, wait_ready,
};
use crate::logs::{RollingLog, SharedLog, lock as lock_log};
use crate::orphan::{self, PidRecord};
use crate::process::ServerProcess;
use crate::secrets::{RuntimeSecrets, api_key_ref_gen, random_key};
use crate::socket;

/// Provider id of the local chat server (`local-chat`).
pub const CHAT_PROVIDER_ID: &str = "local-chat";
/// Provider id of the local embedding server (`local-embedding`).
pub const EMBEDDING_PROVIDER_ID: &str = "local-embedding";
/// Provider id of the local reranker (`local-rerank`). Not a router provider: retrieval calls
/// the reranker directly.
pub const RERANK_PROVIDER_ID: &str = "local-rerank";
/// Provider id of the local Double Tap model (`local-draft`). A router provider without tiers:
/// only the draft feature asks for it, by this id.
pub const DRAFT_PROVIDER_ID: &str = "local-draft";

/// Provider id the router knows a role's server by.
pub fn provider_id(role: RuntimeRole) -> ProviderId {
    ProviderId::new(match role {
        RuntimeRole::Chat => CHAT_PROVIDER_ID,
        RuntimeRole::Embedding => EMBEDDING_PROVIDER_ID,
        RuntimeRole::Rerank => RERANK_PROVIDER_ID,
        RuntimeRole::Draft => DRAFT_PROVIDER_ID,
    })
}

/// What a role's model is called in messages for the user.
fn role_what(role: RuntimeRole) -> &'static str {
    match role {
        RuntimeRole::Chat => "on-device chat model",
        RuntimeRole::Embedding => "on-device search model",
        RuntimeRole::Rerank => "on-device reranker",
        RuntimeRole::Draft => "on-device Double Tap model",
    }
}

/// Seconds, rounded up, for messages.
fn secs_up(d: Duration) -> u64 {
    d.as_millis().div_ceil(1000).max(1) as u64
}

fn role_name(role: RuntimeRole) -> &'static str {
    match role {
        RuntimeRole::Chat => "chat",
        RuntimeRole::Embedding => "embedding",
        RuntimeRole::Rerank => "rerank",
        RuntimeRole::Draft => "draft",
    }
}

const ROLES: [RuntimeRole; 4] = [
    RuntimeRole::Chat,
    RuntimeRole::Embedding,
    RuntimeRole::Rerank,
    RuntimeRole::Draft,
];

fn idx(role: RuntimeRole) -> usize {
    match role {
        RuntimeRole::Chat => 0,
        RuntimeRole::Embedding => 1,
        RuntimeRole::Rerank => 2,
        RuntimeRole::Draft => 3,
    }
}

/// Supervisor tuning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeOptions {
    /// How long a start may take until `/health` reports ready (model load included).
    pub health_timeout: Duration,
    /// First interval between `/health` polls during start-up (grows to 500 ms).
    pub health_poll: Duration,
    /// How often the supervisor checks whether the server is still alive. (Status reads and
    /// [`Runtime::ensure`] also notice a dead server at once.)
    pub watch_interval: Duration,
    /// First restart delay after a crash; doubles per consecutive failure.
    pub restart_backoff_base: Duration,
    pub restart_backoff_max: Duration,
    /// Consecutive failed (re)starts tolerated before giving up (state `Failed`; the next
    /// [`Runtime::ensure`] tries again from scratch).
    pub max_restarts: u32,
    /// Uptime after which a crash no longer counts as consecutive.
    pub stable_after: Duration,
    /// Hang watchdog: how often a ready server's `/health` is probed while requests are in
    /// flight or were made recently (see [`Runtime::request`]). A server that accepts
    /// connections but stops answering is restarted after `hang_strikes` probes in a row went
    /// unanswered for `hang_probe_timeout` each.
    pub hang_check_interval: Duration,
    /// The watchdog's probe interval while the server has been idle for a while.
    pub hang_check_idle_interval: Duration,
    pub hang_probe_timeout: Duration,
    pub hang_strikes: u32,
    /// Stop a server after this long without requests. `None` (default) keeps it running.
    pub idle_shutdown: Option<Duration>,
    /// How often request activity is sampled (from `/metrics`) when `idle_shutdown` is set.
    pub idle_check_interval: Duration,
    /// Time between SIGTERM and SIGKILL when stopping (Unix).
    pub shutdown_grace: Duration,
    /// Size at which `data/logs/llama-server-<role>.log` is rotated.
    pub log_max_bytes: u64,
    /// Rotated log files kept.
    pub log_keep: usize,
    /// Serve on a Unix socket in a private (0700) directory instead of a loopback TCP port
    /// (default `true`). The directory is `data/run`, or a short per-installation one in
    /// `$XDG_RUNTIME_DIR` / the temp dir when that path is too long for a socket address. Unix
    /// only: elsewhere servers always listen on `127.0.0.1`. Set to `false` to fall back to TCP.
    pub unix_socket: bool,
    pub chat: Tuning,
    pub embedding: Tuning,
    pub rerank: Tuning,
    pub draft: Tuning,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            health_timeout: Duration::from_secs(180),
            health_poll: Duration::from_millis(100),
            watch_interval: Duration::from_millis(250),
            restart_backoff_base: Duration::from_secs(1),
            restart_backoff_max: Duration::from_secs(60),
            max_restarts: 5,
            stable_after: Duration::from_secs(120),
            // Worst case from a hang to the restart: 3 x (1 s + 2 s) = 9 s.
            hang_check_interval: Duration::from_secs(1),
            hang_check_idle_interval: Duration::from_secs(10),
            hang_probe_timeout: Duration::from_secs(2),
            hang_strikes: 3,
            idle_shutdown: None,
            idle_check_interval: Duration::from_secs(30),
            shutdown_grace: Duration::from_secs(5),
            log_max_bytes: 8 * 1024 * 1024,
            log_keep: 3,
            unix_socket: true,
            chat: Tuning::default(),
            embedding: Tuning::default(),
            rerank: Tuning::default(),
            draft: Tuning::default(),
        }
    }
}

impl RuntimeOptions {
    fn tuning(&self, role: RuntimeRole) -> &Tuning {
        match role {
            RuntimeRole::Chat => &self.chat,
            RuntimeRole::Embedding => &self.embedding,
            RuntimeRole::Rerank => &self.rerank,
            RuntimeRole::Draft => &self.draft,
        }
    }

    /// Whether servers listen on Unix sockets on this platform.
    fn use_socket(&self) -> bool {
        cfg!(unix) && self.unix_socket
    }

    fn backoff(&self, failures: u32) -> Duration {
        self.restart_backoff_base
            .saturating_mul(1u32 << failures.saturating_sub(1).min(16))
            .min(self.restart_backoff_max)
    }
}

/// Details about one role's server beyond [`RuntimeInfo`].
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeDetails {
    pub info: RuntimeInfo,
    /// The provider config callers should use (port or socket included), while the server is
    /// ready.
    pub config: Option<ProviderConfig>,
    /// Loopback port, when the server listens on TCP.
    pub port: Option<u16>,
    /// Unix socket, when the server listens on one (see [`RuntimeOptions::unix_socket`]).
    pub socket: Option<PathBuf>,
    /// The server process, from spawn (so also while it is loading) until it is stopped or
    /// found dead.
    pub pid: Option<u32>,
    /// Restarts after crashes since the last successful `ensure`.
    pub restarts: u32,
    /// Hangs the watchdog detected (a server that stopped answering, then was killed) over the
    /// runtime's life: callers compare it around a request to tell a hang from a crash.
    pub hangs: u32,
    /// How long loading this role's model is expected to take (the last load on this
    /// machine, else an estimate), once known.
    pub expected_load: Option<Duration>,
    pub accel: Option<RuntimeAccel>,
    pub binary: Option<PathBuf>,
    pub log_file: PathBuf,
}

/// Times measured in a role's status, as reported in [`RuntimeInfo`].
fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Clone)]
struct Config {
    paths: Paths,
    store: Arc<ModelStore>,
    egress: Arc<dyn EgressSink>,
    opts: RuntimeOptions,
    manifest: Manifest,
    hardware: Hardware,
    server_override: Option<(PathBuf, RuntimeAccel)>,
}

/// Supervises local `llama-server` processes (one per [`RuntimeRole`]) on private Unix sockets
/// (Unix) or `127.0.0.1`.
///
/// Cheap to clone; clones share the servers. When the last clone is dropped every server's
/// process group (Unix) or job object (Windows) is killed, including one still starting.
/// Prefer [`Runtime::shutdown`] for a graceful stop.
#[derive(Clone)]
pub struct Runtime {
    cfg: Arc<Config>,
    state: Arc<State>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("data", &self.cfg.paths.data)
            .finish_non_exhaustive()
    }
}

struct State {
    /// Serialises runtime installs (both roles usually need the same build).
    install: Arc<tokio::sync::Mutex<()>>,
    secrets: RuntimeSecrets,
    client: reqwest::Client,
    roles: [Arc<RoleState>; 4],
}

impl Drop for State {
    fn drop(&mut self) {
        for rs in &self.roles {
            if let Some(start) = lock(&rs.start).take() {
                start.cancel.cancel();
                start.handle.abort();
            }
            if let Some(task) = lock(&rs.task).take() {
                task.cancel.cancel();
                task.handle.abort();
            }
            // Synchronous: SIGKILL to the group / terminate the job right now. (A start task
            // that is still being aborted holds the role state; when it lets go, any process it
            // left in the slot is killed by `ServerProcess`'s own Drop.)
            drop(lock(&rs.proc).take());
            orphan::remove(&rs.pid_file);
        }
    }
}

/// What a start task (and a supervisor) needs, without keeping [`State`] alive: dropping the
/// last [`Runtime`] must still kill everything.
#[derive(Clone)]
struct Ctx {
    cfg: Arc<Config>,
    install: Arc<tokio::sync::Mutex<()>>,
    secrets: RuntimeSecrets,
    client: reqwest::Client,
}

impl Ctx {
    async fn install_entry(
        &self,
        entry: &RuntimeEntry,
        on_progress: Arc<dyn Fn(Progress) + Send + Sync>,
    ) -> Result<PathBuf> {
        let _install = self.install.lock().await;
        if let Some(bin) = self.cfg.store.runtime_binary(entry)? {
            return Ok(bin);
        }
        tracing::info!(runtime = %entry.id, release = %entry.version, "installing llama-server");
        self.cfg
            .store
            .install_runtime_with_egress(entry, Arc::clone(&self.cfg.egress), move |p| {
                on_progress(p)
            })
            .await
    }
}

struct Task {
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

/// Outcome of a start, shared by every caller waiting for it.
type StartResult = std::result::Result<ProviderConfig, Arc<Error>>;

/// A start running detached from the [`Runtime::ensure`] calls that wait for it, so a caller
/// giving up (a timeout dropping its future) neither kills the loading server nor leaves the
/// status behind.
struct Start {
    model_sha: String,
    cancel: CancellationToken,
    result: watch::Receiver<Option<StartResult>>,
    handle: tokio::task::JoinHandle<()>,
}

#[derive(Debug, Clone, Default)]
struct Status {
    model_id: Option<String>,
    model_sha: Option<String>,
    state: Option<RuntimeState>,
    error: Option<String>,
    config: Option<ProviderConfig>,
    port: Option<u16>,
    socket: Option<PathBuf>,
    restarts: u32,
    hangs: u32,
    accel: Option<RuntimeAccel>,
    binary: Option<PathBuf>,
    /// When `state` last changed (the warm-up clock while `Starting`).
    since: Option<Instant>,
    /// How long loading this model is expected to take (the last successful load, else an
    /// estimate from its size).
    expected_load: Option<Duration>,
    /// While `Failed`: when the next start may happen (an automatic restart, or the earliest
    /// time [`Runtime::ensure`] starts a server again after a failed start). `None` when the
    /// supervisor gave up: only [`Runtime::retry`] clears that.
    retry_at: Option<Instant>,
    /// Gave up after consecutive failures: [`Runtime::ensure`] fails fast until
    /// [`Runtime::retry`] (no relaunch per request).
    gave_up: bool,
    /// Consecutive failed starts begun by [`Runtime::ensure`] (bounded backoff between them).
    start_failures: u32,
    /// The last line the server printed before it failed (details for the log view).
    detail: Option<String>,
}

/// Holds a role busy while a request is in flight (see [`Runtime::request`]).
pub struct RequestGuard {
    rs: Arc<RoleState>,
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.rs.inflight.fetch_sub(1, Ordering::SeqCst);
        self.rs.touch();
    }
}

impl std::fmt::Debug for RequestGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestGuard")
            .field("role", &self.rs.role)
            .finish()
    }
}

struct RoleState {
    role: RuntimeRole,
    /// Serialises the decisions of `ensure` and `stop` for this role (never held while waiting
    /// for a start).
    op: tokio::sync::Mutex<()>,
    /// The server process, from spawn on (also while loading).
    proc: ProcSlot,
    status: Mutex<Status>,
    /// Bumped on every state change (waiters re-check `status`).
    changed: watch::Sender<u64>,
    /// Last request activity; counts time the computer slept, so a server idle overnight is
    /// stopped soon after wake (GG6-29).
    activity: Mutex<Stopwatch>,
    /// The supervisor of a ready server.
    task: Mutex<Option<Task>>,
    /// The latest start (running, or finished).
    start: Mutex<Option<Start>>,
    log: Mutex<Option<SharedLog>>,
    log_file: PathBuf,
    pid_file: PathBuf,
    /// The last successful load time per model (`data/runtime/<role>.load.json`).
    load_file: PathBuf,
    /// Requests in flight ([`Runtime::request`]): the hang watchdog probes often while busy.
    inflight: AtomicUsize,
    /// Cuts a restart backoff short ([`Runtime::retry`]).
    wake: tokio::sync::Notify,
}

/// The last successful load of a role's model, persisted for the warm-up estimate.
#[derive(serde::Serialize, serde::Deserialize)]
struct LoadRecord {
    model_sha: String,
    ms: u64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl RoleState {
    fn set(&self, f: impl FnOnce(&mut Status)) {
        {
            let mut st = lock(&self.status);
            let before = st.state;
            f(&mut st);
            if st.state != before || st.since.is_none() {
                st.since = Some(Instant::now());
            }
            if st.state != Some(RuntimeState::Failed) {
                st.retry_at = None;
                st.gave_up = false;
            }
        }
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }

    /// The expected load time of the model with `sha` (`size` bytes): the last successful load
    /// of that model on this machine, else an estimate (about 150 MB/s plus a few seconds).
    fn expected_load(&self, sha: &str, size: u64) -> Duration {
        let recorded = std::fs::read(&self.load_file)
            .ok()
            .and_then(|b| serde_json::from_slice::<LoadRecord>(&b).ok())
            .filter(|r| r.model_sha == sha)
            .map(|r| Duration::from_millis(r.ms));
        recorded.unwrap_or_else(|| Duration::from_secs(3 + size / 150_000_000))
    }

    /// Remember how long the model with `sha` took to load (best effort).
    fn record_load(&self, sha: &str, took: Duration) {
        let rec = LoadRecord {
            model_sha: sha.to_string(),
            ms: ms(took),
        };
        if let Ok(b) = serde_json::to_vec(&rec) {
            if let Some(dir) = self.load_file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&self.load_file, b);
        }
        self.set(|st| st.expected_load = Some(took));
    }

    /// Whether the hang watchdog should probe at the busy rate: requests in flight, or one
    /// made within the last minute.
    fn busy(&self) -> bool {
        self.inflight.load(Ordering::SeqCst) > 0
            || lock(&self.activity).elapsed() < Duration::from_secs(60)
    }

    fn touch(&self) {
        *lock(&self.activity) = Stopwatch::start();
    }

    /// The role's log, now also redacting `key`.
    fn log(&self, opts: &RuntimeOptions, key: &Secret) -> Result<SharedLog> {
        let mut g = lock(&self.log);
        let l = match g.as_ref() {
            Some(l) => l.clone(),
            None => {
                let log =
                    RollingLog::open(self.log_file.clone(), opts.log_max_bytes, opts.log_keep)?;
                let l = Arc::new(Mutex::new(log));
                *g = Some(l.clone());
                l
            }
        };
        lock_log(&l).add_secret(key.expose());
        Ok(l)
    }

    /// The live server process's pid (`None` once it exited, even before it is reaped).
    fn pid(&self) -> Option<u32> {
        let mut g = lock(&self.proc);
        let p = g.as_mut()?;
        if p.try_wait().is_some() {
            return None;
        }
        p.pid()
    }

    /// If the server died while reported ready, say so now (no config for a dead server's port,
    /// which anybody could bind), without waiting for the supervisor's next tick.
    fn refresh(&self) {
        let mut g = lock(&self.proc);
        let Some(status) = g.as_mut().and_then(ServerProcess::try_wait) else {
            return;
        };
        // Under the process lock, so a restart cannot slip in between check and update.
        if lock(&self.status).state == Some(RuntimeState::Ready) {
            tracing::warn!(role = role_name(self.role), %status, "llama-server exited unexpectedly");
            let what = role_what(self.role);
            self.set(|st| {
                st.state = Some(RuntimeState::Failed);
                st.config = None;
                st.error = Some(format!("The {what} stopped unexpectedly; restarting it."));
            });
        }
        drop(g);
    }

    /// While a failed start of this model (or a give-up) holds off new starts: the error to
    /// return at once. `None` once a new start may begin.
    fn cooling_down(&self, model_sha: &str) -> Option<Error> {
        let st = lock(&self.status);
        if st.state != Some(RuntimeState::Failed) || st.model_sha.as_deref() != Some(model_sha) {
            return None;
        }
        let what = role_what(self.role);
        let error = st
            .error
            .clone()
            .unwrap_or_else(|| format!("The {what} couldn't start."));
        if st.gave_up {
            return Some(Error::Provider(error));
        }
        let left = st.retry_at?.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        Some(Error::Provider(error))
    }

    /// A start for this model that is still running.
    fn joinable_start(&self, model_sha: &str) -> Option<watch::Receiver<Option<StartResult>>> {
        let g = lock(&self.start);
        let s = g.as_ref()?;
        (s.model_sha == model_sha && !s.handle.is_finished()).then(|| s.result.clone())
    }

    /// A supervised server for this model (ready, or restarting after a crash).
    fn serving(&self, model_sha: &str) -> bool {
        let task_alive = lock(&self.task)
            .as_ref()
            .is_some_and(|t| !t.handle.is_finished());
        task_alive && lock(&self.status).model_sha.as_deref() == Some(model_sha)
    }

    /// Withdraw the config (and say `Stopping`) before a server is signalled to stop.
    fn mark_stopping(&self) {
        self.set(|st| {
            if st.state.is_some() {
                st.state = Some(RuntimeState::Stopping);
            }
            st.config = None;
        });
    }

    /// The server is gone: `Stopping` becomes `Stopped`, and its socket file is removed.
    fn mark_stopped(&self) {
        let sock = lock(&self.status).socket.clone();
        if let Some(sock) = sock {
            socket::remove_stale(&sock);
        }
        self.set(|st| {
            if st.state == Some(RuntimeState::Stopping) {
                st.state = Some(RuntimeState::Stopped);
            }
            st.config = None;
        });
    }
}

/// An error as the one sentence the status shows (without the error kind's prefix).
fn plain(role: RuntimeRole, e: &Error) -> String {
    let what = role_what(role);
    match e {
        Error::Provider(m) => m.clone(),
        Error::Network(_) => format!(
            "The {what} couldn't start: the model server could not be downloaded. Check your \
             connection, then open Status to retry."
        ),
        Error::InsufficientSpace { .. } => format!(
            "The {what} couldn't start: there is not enough disk space for the model server."
        ),
        Error::NotFound(_) | Error::Integrity(_) | Error::Io(_) | Error::Unsupported(_) => format!(
            "The {what} couldn't start: the model server is missing or damaged. Open Status to \
             retry or to see its log."
        ),
        _ => format!("The {what} couldn't start. Open Status to retry or to see its log."),
    }
}

/// What a request is told while the supervisor backs off before restarting a crashed or hung
/// server.
fn backing_off_message(role: RuntimeRole, st: &Status) -> String {
    let what = role_what(role);
    let error = st
        .error
        .clone()
        .unwrap_or_else(|| format!("The {what} stopped unexpectedly; restarting it."));
    match st.retry_at {
        Some(t) => format!(
            "{error} Try again in {} s.",
            secs_up(t.saturating_duration_since(Instant::now()))
        ),
        None => format!("{error} Try again in a moment."),
    }
}

/// Everything needed to (re)start one server.
#[derive(Clone)]
struct Launch {
    binary: PathBuf,
    accel: RuntimeAccel,
    plan: LaunchPlan,
    entry: ModelEntry,
}

type Launches = Vec<(Option<RuntimeEntry>, Launch)>;

fn clone_err(e: &Error) -> Error {
    match e {
        Error::NotFound(m) => Error::NotFound(m.clone()),
        Error::Invalid(m) => Error::Invalid(m.clone()),
        Error::Integrity(m) => Error::Integrity(m.clone()),
        Error::Unsupported(m) => Error::Unsupported(m.clone()),
        Error::Provider(m) => Error::Provider(m.clone()),
        Error::Storage(m) => Error::Storage(m.clone()),
        Error::Network(m) => Error::Network(m.clone()),
        Error::RateLimited { retry_after_ms } => Error::RateLimited {
            retry_after_ms: *retry_after_ms,
        },
        Error::Unauthorized(m) => Error::Unauthorized(m.clone()),
        Error::InsufficientSpace { needed, available } => Error::InsufficientSpace {
            needed: *needed,
            available: *available,
        },
        Error::Io(io) => Error::Io(std::io::Error::new(io.kind(), io.to_string())),
        Error::Cancelled => Error::Cancelled,
        other => Error::Other(other.to_string()),
    }
}

/// Wait for a start's outcome. A start that was abandoned (its runtime dropped) is `Cancelled`.
async fn await_start(mut rx: watch::Receiver<Option<StartResult>>) -> Result<ProviderConfig> {
    let res = match rx.wait_for(Option::is_some).await {
        Ok(v) => v.clone(),
        Err(_) => return Err(Error::Cancelled),
    };
    match res {
        Some(Ok(cfg)) => Ok(cfg),
        Some(Err(e)) => Err(clone_err(&e)),
        None => Err(Error::Cancelled),
    }
}

impl Runtime {
    /// A supervisor for servers under `paths.data` (logs in `data/logs`), running models from
    /// `model_store` and installing runtimes into it (egress recorded to `egress` as
    /// `runtime_download`). Uses the bundled signed manifest and the detected hardware; kills
    /// servers orphaned by a previous daemon that died without cleaning up.
    pub fn new(
        paths: &Paths,
        model_store: Arc<ModelStore>,
        egress: Arc<dyn EgressSink>,
    ) -> Result<Self> {
        let killed = orphan::reap(&paths.data.join("runtime"));
        if killed > 0 {
            tracing::warn!(killed, "cleaned up orphaned local runtime servers");
        }
        let secrets =
            RuntimeSecrets::new(random_key()?, random_key()?, random_key()?, random_key()?);
        let mk = |role: RuntimeRole| {
            Arc::new(RoleState {
                role,
                op: tokio::sync::Mutex::new(()),
                proc: Mutex::new(None),
                status: Mutex::new(Status::default()),
                changed: watch::Sender::new(0),
                activity: Mutex::new(Stopwatch::start()),
                task: Mutex::new(None),
                start: Mutex::new(None),
                log: Mutex::new(None),
                log_file: paths
                    .data
                    .join("logs")
                    .join(format!("llama-server-{}.log", role_name(role))),
                pid_file: paths
                    .data
                    .join("runtime")
                    .join(format!("{}.pid", role_name(role))),
                load_file: paths
                    .data
                    .join("runtime")
                    .join(format!("{}.load.json", role_name(role))),
                inflight: AtomicUsize::new(0),
                wake: tokio::sync::Notify::new(),
            })
        };
        Ok(Self {
            cfg: Arc::new(Config {
                paths: paths.clone(),
                store: model_store,
                egress,
                opts: RuntimeOptions::default(),
                manifest: default_manifest()?,
                hardware: detect_hardware(),
                server_override: None,
            }),
            state: Arc::new(State {
                install: Arc::new(tokio::sync::Mutex::new(())),
                secrets,
                client: loopback_client()?,
                roles: [
                    mk(RuntimeRole::Chat),
                    mk(RuntimeRole::Embedding),
                    mk(RuntimeRole::Rerank),
                    mk(RuntimeRole::Draft),
                ],
            }),
        })
    }

    /// A separate supervisor with these settings whose servers keep their logs, sockets and pid
    /// files under `data/<name>`: a second server for a role, e.g. the new embedding model while
    /// the search index is re-built with it.
    pub fn sibling(&self, name: &str) -> Result<Self> {
        let mut paths = self.cfg.paths.clone();
        paths.data = paths.data.join(name);
        let mut rt = Self::new(&paths, self.cfg.store.clone(), self.cfg.egress.clone())?;
        rt.cfg = Arc::new(Config {
            paths,
            ..(*self.cfg).clone()
        });
        Ok(rt)
    }

    /// Replace the supervisor options. Configure before the first [`Runtime::ensure`].
    pub fn with_options(mut self, opts: RuntimeOptions) -> Self {
        Arc::make_mut(&mut self.cfg).opts = opts;
        self
    }

    /// Take runtime entries from this (already verified) manifest instead of the bundled one.
    pub fn with_manifest(mut self, manifest: Manifest) -> Self {
        Arc::make_mut(&mut self.cfg).manifest = manifest;
        self
    }

    /// Use this hardware description instead of the detected one.
    pub fn with_hardware(mut self, hw: Hardware) -> Self {
        Arc::make_mut(&mut self.cfg).hardware = hw;
        self
    }

    /// Run this `llama-server` executable (built with `accel`) instead of a manifest runtime.
    /// Nothing is downloaded. For user-supplied builds and tests.
    pub fn with_server_binary(mut self, binary: impl Into<PathBuf>, accel: RuntimeAccel) -> Self {
        Arc::make_mut(&mut self.cfg).server_override = Some((binary.into(), accel));
        self
    }

    pub fn options(&self) -> &RuntimeOptions {
        &self.cfg.opts
    }

    pub fn hardware(&self) -> &Hardware {
        &self.cfg.hardware
    }

    /// Resolver for the `api_key` references in the configs this runtime returns. Pass it to
    /// [`nugget_infer::build_provider_with`]; other references go to the OS env/keychain.
    ///
    /// Every server (re)start gets a new key, and the configs name the key generation they were
    /// issued for: a config from before a restart no longer resolves. Call [`Runtime::ensure`]
    /// (or read [`Runtime::details`]) again and rebuild the provider when the config changed.
    pub fn secrets(&self) -> RuntimeSecrets {
        self.state.secrets.clone()
    }

    fn ctx(&self) -> Ctx {
        Ctx {
            cfg: Arc::clone(&self.cfg),
            install: Arc::clone(&self.state.install),
            secrets: self.state.secrets.clone(),
            client: self.state.client.clone(),
        }
    }

    /// Manifest runtime builds usable on this machine, best first.
    pub fn runtime_candidates(&self) -> Vec<&RuntimeEntry> {
        runtime_candidates_for(
            &self.cfg.manifest,
            current_os(),
            current_arch(),
            self.cfg.hardware.gpu,
        )
    }

    /// Install the best runtime build for this machine (no-op if installed). Returns its binary.
    pub async fn install_runtime(
        &self,
        on_progress: impl Fn(Progress) + Send + Sync + 'static,
    ) -> Result<(PathBuf, RuntimeAccel)> {
        let entry = self
            .runtime_candidates()
            .into_iter()
            .next()
            .ok_or_else(|| {
                Error::Unsupported(format!(
                    "no llama-server build for {}/{} in the manifest",
                    current_os(),
                    current_arch()
                ))
            })?;
        let bin = self
            .ctx()
            .install_entry(entry, Arc::new(on_progress))
            .await?;
        Ok((bin, entry.accel))
    }

    /// Make sure a server for `role` is running `model` and ready; returns the provider config to
    /// reach it. Installs the runtime build if needed (see [`Runtime::ensure_with_progress`]).
    /// The model itself must already be installed in the model store.
    ///
    /// Cheap when the server is already up: callers can (and after a connection error, should)
    /// call it before using the config, which also counts as activity for the idle timer.
    ///
    /// The start itself runs detached: dropping this future (a caller's timeout) does not stop
    /// it. Later calls for the same model join the start in progress instead of restarting it.
    pub async fn ensure(&self, role: RuntimeRole, model: &ModelEntry) -> Result<ProviderConfig> {
        self.ensure_with_progress(role, model, |_| {}).await
    }

    /// [`Runtime::ensure`], reporting runtime download progress (of a start this call begins).
    pub async fn ensure_with_progress(
        &self,
        role: RuntimeRole,
        model: &ModelEntry,
        on_progress: impl Fn(Progress) + Send + Sync + 'static,
    ) -> Result<ProviderConfig> {
        let rs = Arc::clone(self.role(role));
        rs.touch();
        let model_path = self.cfg.store.path(&model.id)?;
        let model_sha = self
            .cfg
            .store
            .slot(&model.id)?
            .map(|s| s.active.sha256)
            .unwrap_or_default();

        // Validate everything before touching a running server.
        let launches = self.launches(role, model, model_path)?;
        let on_progress: Arc<dyn Fn(Progress) + Send + Sync> = Arc::new(on_progress);

        let mut reuse = true;
        loop {
            let start = {
                let _op = rs.op.lock().await;
                rs.refresh();
                if let Some(rx) = rs.joinable_start(&model_sha) {
                    Some(rx)
                } else if reuse && rs.serving(&model_sha) {
                    None
                } else if let Some(e) = rs.cooling_down(&model_sha) {
                    // A start of this model failed (or the supervisor gave up) moments ago:
                    // say so at once instead of launching a server per request.
                    return Err(e);
                } else {
                    self.stop_locked(&rs).await;
                    Some(self.spawn_start(
                        &rs,
                        launches.clone(),
                        model_sha.clone(),
                        on_progress.clone(),
                    ))
                }
            };
            match start {
                Some(rx) => {
                    let res = await_start(rx).await;
                    rs.touch();
                    return res;
                }
                // Already serving this model: wait out a restart in progress, if any.
                None => match self.wait_ready_state(&rs).await? {
                    Some(cfg) => return Ok(cfg),
                    None => reuse = false,
                },
            }
        }
    }

    /// While a restart is in progress, wait for it. `Ok(None)` if the server gave up or stopped.
    async fn wait_ready_state(&self, rs: &RoleState) -> Result<Option<ProviderConfig>> {
        let mut rx = rs.changed.subscribe();
        let deadline = tokio::time::Instant::now() + self.cfg.opts.health_timeout;
        loop {
            rs.refresh();
            {
                let st = lock(&rs.status);
                match st.state {
                    Some(RuntimeState::Ready) if st.config.is_some() => {
                        return Ok(st.config.clone());
                    }
                    // Loading (warming up after a restart): wait for it.
                    Some(RuntimeState::Starting) => {}
                    // Crashed or hung and backing off before the next restart: fail fast with
                    // what happened, instead of holding the request through the backoff.
                    Some(RuntimeState::Failed) => {
                        let task_alive = lock(&rs.task)
                            .as_ref()
                            .is_some_and(|t| !t.handle.is_finished());
                        if !task_alive {
                            return Ok(None);
                        }
                        return Err(Error::Provider(backing_off_message(rs.role, &st)));
                    }
                    _ => return Ok(None),
                }
            }
            let finished = lock(&rs.task)
                .as_ref()
                .is_none_or(|t| t.handle.is_finished());
            if finished {
                return Ok(None);
            }
            // Wake on a state change, or poll: a dead server is noticed by `refresh`.
            let wait =
                tokio::time::sleep(self.cfg.opts.watch_interval.min(Duration::from_millis(250)));
            tokio::select! {
                r = rx.changed() => {
                    if r.is_err() {
                        return Ok(None);
                    }
                }
                _ = wait => {}
                _ = tokio::time::sleep_until(deadline) => {
                    return Err(Error::Provider(format!(
                        "The {} is still loading after a restart; try again in a moment.",
                        role_what(rs.role)
                    )));
                }
            }
        }
    }

    /// Candidate launches: the override, or each installable manifest build (best first).
    fn launches(
        &self,
        role: RuntimeRole,
        model: &ModelEntry,
        model_path: PathBuf,
    ) -> Result<Launches> {
        let tuning = self.cfg.opts.tuning(role);
        let builds: Vec<(Option<RuntimeEntry>, PathBuf, RuntimeAccel)> =
            match &self.cfg.server_override {
                Some((bin, accel)) => vec![(None, bin.clone(), *accel)],
                None => self
                    .runtime_candidates()
                    .into_iter()
                    .map(|e| (Some(e.clone()), PathBuf::new(), e.accel))
                    .collect(),
            };
        if builds.is_empty() {
            return Err(Error::Unsupported(format!(
                "no llama-server build for {}/{} in the manifest",
                current_os(),
                current_arch()
            )));
        }
        check_role(role, model)?;
        builds
            .into_iter()
            .map(|(entry, binary, accel)| {
                let plan = LaunchPlan::new(
                    role,
                    model_path.clone(),
                    &model.id,
                    0,
                    accel,
                    &self.cfg.hardware,
                    tuning,
                )?;
                Ok((
                    entry,
                    Launch {
                        binary,
                        accel,
                        plan,
                        entry: model.clone(),
                    },
                ))
            })
            .collect()
    }

    /// Begin a detached start (with `op` held and nothing running). Returns its outcome channel.
    fn spawn_start(
        &self,
        rs: &Arc<RoleState>,
        launches: Launches,
        model_sha: String,
        on_progress: Arc<dyn Fn(Progress) + Send + Sync>,
    ) -> watch::Receiver<Option<StartResult>> {
        // `Starting` right away, so the status never lags behind the start.
        let (previous, previous_socket, start_failures) = {
            let st = lock(&rs.status);
            let failures = if st.model_sha.as_deref() == Some(model_sha.as_str()) {
                st.start_failures
            } else {
                0
            };
            (st.port, st.socket.clone(), failures)
        };
        let model_id = launches.first().map(|(_, l)| l.entry.id.clone());
        let expected = launches
            .first()
            .map(|(_, l)| rs.expected_load(&model_sha, l.entry.size));
        let hangs = lock(&rs.status).hangs;
        rs.set(|st| {
            *st = Status {
                port: previous,
                socket: previous_socket,
                model_id,
                model_sha: Some(model_sha.clone()),
                state: Some(RuntimeState::Starting),
                accel: launches.first().map(|(_, l)| l.accel),
                expected_load: expected,
                start_failures,
                hangs,
                ..Status::default()
            };
        });
        let (tx, rx) = watch::channel(None);
        let cancel = CancellationToken::new();
        let handle = tokio::spawn({
            let (ctx, rs, cancel, sha) = (
                self.ctx(),
                Arc::clone(rs),
                cancel.clone(),
                model_sha.clone(),
            );
            async move {
                let res = tokio::select! {
                    // Cancelled by a stop, which also takes care of the process and the status.
                    _ = cancel.cancelled() => Err(Error::Cancelled),
                    r = start_launches(&ctx, &rs, launches, &sha, on_progress) => r,
                };
                let _ = tx.send(Some(res.map_err(Arc::new)));
            }
        });
        *lock(&rs.start) = Some(Start {
            model_sha,
            cancel,
            result: rx.clone(),
            handle,
        });
        rx
    }

    /// Stop the server for `role` (graceful, then forced), including one still starting. No-op
    /// if none is running.
    pub async fn stop(&self, role: RuntimeRole) {
        let rs = self.role(role);
        let _op = rs.op.lock().await;
        self.stop_locked(rs).await;
    }

    async fn stop_locked(&self, rs: &RoleState) {
        // Nobody gets this server's config from now on: withdrawn before anything is signalled.
        rs.mark_stopping();
        let start = lock(&rs.start).take();
        if let Some(s) = start {
            s.cancel.cancel();
            let _ = s.handle.await;
        }
        // Taken only now: a start that completed while being cancelled installed a supervisor.
        let task = lock(&rs.task).take();
        if let Some(t) = task {
            t.cancel.cancel();
            let _ = t.handle.await;
        }
        // Again: a task may have published a config while it was being cancelled.
        rs.mark_stopping();
        let proc = lock(&rs.proc).take();
        if let Some(mut p) = proc {
            p.shutdown(self.cfg.opts.shutdown_grace).await;
            if let Some(log) = lock(&rs.log).as_ref() {
                lock_log(log).note("stopped");
            }
        }
        orphan::remove(&rs.pid_file);
        rs.mark_stopped();
    }

    /// Gracefully stop every server.
    pub async fn shutdown(&self) {
        for role in ROLES {
            self.stop(role).await;
        }
    }

    /// State of every role that has been started at least once.
    pub fn status(&self) -> Vec<RuntimeInfo> {
        ROLES
            .iter()
            .filter_map(|&r| self.details(r).map(|d| d.info))
            .collect()
    }

    /// Detailed state of one role, if it has been started at least once.
    pub fn details(&self, role: RuntimeRole) -> Option<RuntimeDetails> {
        let rs = self.role(role);
        rs.refresh();
        let st = lock(&rs.status).clone();
        let model_id = st.model_id?;
        let state = st.state.unwrap_or(RuntimeState::Stopped);
        Some(RuntimeDetails {
            info: RuntimeInfo {
                model_id,
                role,
                state,
                provider: provider_id(role),
                error: st.error,
                state_ms: st.since.map(|t| ms(t.elapsed())),
                expected_ms: match state {
                    RuntimeState::Starting => st.expected_load.map(ms),
                    _ => None,
                },
                retry_in_ms: match state {
                    RuntimeState::Failed => st
                        .retry_at
                        .map(|t| ms(t.saturating_duration_since(Instant::now()))),
                    _ => None,
                },
                gave_up: state == RuntimeState::Failed && st.gave_up,
                detail: st.detail,
            },
            config: if state == RuntimeState::Ready {
                st.config
            } else {
                None
            },
            port: st.port,
            socket: st.socket,
            pid: rs.pid(),
            restarts: st.restarts,
            hangs: st.hangs,
            expected_load: st.expected_load,
            accel: st.accel,
            binary: st.binary,
            log_file: rs.log_file.clone(),
        })
    }

    /// Record activity for the idle timer (e.g. from the code that sends requests).
    pub fn touch(&self, role: RuntimeRole) {
        self.role(role).touch();
    }

    /// Mark a request to `role`'s server in flight until the guard is dropped: counts as
    /// activity for the idle timer, and the hang watchdog probes the server at its busy rate
    /// ([`RuntimeOptions::hang_check_interval`]) meanwhile.
    pub fn request(&self, role: RuntimeRole) -> RequestGuard {
        let rs = Arc::clone(self.role(role));
        rs.inflight.fetch_add(1, Ordering::SeqCst);
        rs.touch();
        RequestGuard { rs }
    }

    /// The user asked to retry: forget failed starts and a give-up, so the next
    /// [`Runtime::ensure`] starts a server at once. Starts nothing itself.
    pub fn retry(&self, role: RuntimeRole) {
        let rs = self.role(role);
        rs.set(|st| {
            st.retry_at = None;
            st.gave_up = false;
            st.start_failures = 0;
        });
        // A supervisor backing off before a restart restarts now.
        rs.wake.notify_one();
    }

    /// Wait up to `timeout` for `role`'s server to be ready (a restart in progress, say) and
    /// return its config. `None` if it is not ready by then, failed for good, or never started.
    /// Starts nothing.
    pub async fn wait_ready(&self, role: RuntimeRole, timeout: Duration) -> Option<ProviderConfig> {
        let rs = self.role(role);
        let mut rx = rs.changed.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            rs.refresh();
            {
                let st = lock(&rs.status);
                match st.state {
                    Some(RuntimeState::Ready) if st.config.is_some() => return st.config.clone(),
                    Some(RuntimeState::Starting) => {}
                    // Only a live supervisor restarts a failed server; otherwise nothing comes.
                    Some(RuntimeState::Failed) if !st.gave_up => {
                        let supervised = lock(&rs.task)
                            .as_ref()
                            .is_some_and(|t| !t.handle.is_finished());
                        if !supervised {
                            return None;
                        }
                    }
                    _ => return None,
                }
            }
            let wait =
                tokio::time::sleep(self.cfg.opts.watch_interval.min(Duration::from_millis(250)));
            tokio::select! {
                r = rx.changed() => if r.is_err() { return None },
                _ = wait => {}
                _ = tokio::time::sleep_until(deadline) => return None,
            }
        }
    }

    /// The rolling log file of `role`'s server.
    pub fn log_file(&self, role: RuntimeRole) -> PathBuf {
        self.role(role).log_file.clone()
    }

    fn role(&self, role: RuntimeRole) -> &Arc<RoleState> {
        &self.state.roles[idx(role)]
    }
}

/// Try each launch in turn (best build first). Runs in the detached start task.
async fn start_launches(
    ctx: &Ctx,
    rs: &Arc<RoleState>,
    launches: Launches,
    model_sha: &str,
    on_progress: Arc<dyn Fn(Progress) + Send + Sync>,
) -> Result<ProviderConfig> {
    let mut last_err = None;
    let n = launches.len();
    for (i, launch) in launches.into_iter().enumerate() {
        match start_new(ctx, rs, launch, model_sha, on_progress.clone()).await {
            Ok(cfg) => return Ok(cfg),
            Err(e) => {
                if i + 1 < n {
                    tracing::warn!(role = role_name(rs.role), error = %e,
                        "runtime build failed to start; trying the next one");
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| Error::Unsupported("no runtime".into())))
}

async fn start_new(
    ctx: &Ctx,
    rs: &Arc<RoleState>,
    (runtime, mut launch): (Option<RuntimeEntry>, Launch),
    model_sha: &str,
    on_progress: Arc<dyn Fn(Progress) + Send + Sync>,
) -> Result<ProviderConfig> {
    let opts = &ctx.cfg.opts;
    // Reuse the previous port when it is free, so configs handed out before a stop (idle,
    // explicit) point at the right place again (they still need the new key's generation).
    let (previous, previous_socket, start_failures) = {
        let st = lock(&rs.status);
        (st.port, st.socket.clone(), st.start_failures)
    };
    let expected = rs.expected_load(model_sha, launch.entry.size);
    let hangs = lock(&rs.status).hangs;
    rs.set(|st| {
        *st = Status {
            port: previous,
            socket: previous_socket,
            model_id: Some(launch.entry.id.clone()),
            model_sha: Some(model_sha.to_string()),
            state: Some(RuntimeState::Starting),
            accel: Some(launch.accel),
            expected_load: Some(expected),
            start_failures,
            hangs,
            ..Status::default()
        };
    });
    let mut load_started = Instant::now();
    let res = async {
        if let Some(entry) = &runtime {
            launch.binary = ctx.install_entry(entry, on_progress).await?;
        }
        let server_socket = if opts.use_socket() {
            socket::server_socket(&ctx.cfg.paths, role_name(rs.role))?
        } else {
            None
        };
        if let Some(path) = server_socket {
            launch.plan.socket = Some(path);
            launch.plan.port = 0;
        } else {
            launch.plan.socket = None;
            launch.plan.port = choose_port(previous)?;
        }
        // A fresh key for every server process.
        let (generation, key) = ctx.secrets.rotate(rs.role)?;
        let log = rs.log(opts, &key)?;
        load_started = Instant::now();
        start_once(&ctx.client, opts, rs, &launch, &key, &log).await?;
        Ok::<_, Error>((generation, key))
    }
    .await;
    let (generation, key) = match res {
        Ok(v) => v,
        Err(e) => {
            // Hold off further starts of this model for a growing (bounded) while, so requests
            // made meanwhile fail fast instead of each launching a server (see `cooling_down`).
            let backoff = opts.backoff(start_failures + 1);
            rs.set(|st| {
                st.state = Some(RuntimeState::Failed);
                st.error = Some(plain(rs.role, &e));
                if st.detail.is_none() && !matches!(e, Error::Provider(_)) {
                    st.detail = Some(truncate_chars(&e.to_string(), 300));
                }
                st.start_failures = start_failures + 1;
                st.retry_at = Some(Instant::now() + backoff);
            });
            return Err(e);
        }
    };
    rs.record_load(model_sha, load_started.elapsed());
    let cfg = provider_config(rs.role, &launch.entry, &launch.plan, generation)?;
    if let Some(pid) = rs.pid() {
        orphan::write(
            &rs.pid_file,
            &PidRecord {
                pid,
                exe: launch.binary.clone(),
                port: launch.plan.port,
            },
        );
    }
    tracing::info!(role = role_name(rs.role), model = %launch.entry.id,
        listen = %listen_display(&launch.plan), accel = launch.accel.as_str(),
        "local runtime ready");
    let binary = launch.binary.clone();
    let (port, sock) = listen_status(&launch.plan);
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(supervise(
        Arc::clone(rs),
        ctx.clone(),
        launch,
        key,
        cancel.clone(),
    ));
    *lock(&rs.task) = Some(Task { cancel, handle });
    rs.set(|st| {
        st.state = Some(RuntimeState::Ready);
        st.error = None;
        st.detail = None;
        st.start_failures = 0;
        st.port = port;
        st.socket = sock;
        st.config = Some(cfg.clone());
        st.binary = Some(binary);
    });
    Ok(cfg)
}

fn check_role(role: RuntimeRole, model: &ModelEntry) -> Result<()> {
    let embeds = model.tiers.contains(&Tier::Embedding);
    let chats = model.tiers.iter().any(|t| t.is_chat());
    let reranks = model.tiers.contains(&Tier::Rerank);
    let fail = |what: &str| {
        Err(Error::Invalid(format!(
            "model `{}` {what} (tiers: {})",
            model.id,
            model
                .tiers
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )))
    };
    match role {
        // A reranker is a cross-encoder: it neither embeds nor generates.
        RuntimeRole::Embedding | RuntimeRole::Chat | RuntimeRole::Draft if reranks => {
            fail("is a reranker; it serves only the rerank role")
        }
        RuntimeRole::Embedding if !embeds => fail("does not serve the embedding tier"),
        RuntimeRole::Chat | RuntimeRole::Draft if !chats => fail("serves no chat tier"),
        RuntimeRole::Rerank if !reranks => fail("is not a reranker (no rerank tier)"),
        _ => Ok(()),
    }
}

/// The `openai_compatible` provider config for a started server, whose key has `generation`.
pub(crate) fn provider_config(
    role: RuntimeRole,
    model: &ModelEntry,
    plan: &LaunchPlan,
    generation: u64,
) -> Result<ProviderConfig> {
    let tiers: Vec<Tier> = match role {
        RuntimeRole::Embedding => vec![Tier::Embedding],
        // Never routed for a tier (the draft feature asks for `local-draft` by id).
        RuntimeRole::Rerank | RuntimeRole::Draft => Vec::new(),
        RuntimeRole::Chat => model
            .tiers
            .iter()
            .copied()
            .filter(|t| t.is_chat())
            .collect(),
    };
    // On a Unix socket the URL only names the path (and `Host`); the socket is the address.
    let base_url = match &plan.socket {
        Some(_) => "http://localhost/v1".to_string(),
        None => format!("http://127.0.0.1:{}/v1", plan.port),
    };
    // Built from JSON so fields added to ProviderConfig later take their serde defaults.
    let mut v = serde_json::json!({
        "id": provider_id(role),
        "kind": "openai_compatible",
        "base_url": base_url,
        "socket_path": plan.socket,
        "model": model.id,
        "api_key": api_key_ref_gen(role, generation).to_string(),
        "tiers": tiers,
        "locality": "on_device",
        "context_tokens": plan.ctx_per_slot,
    });
    // Hybrid reasoning models (Qwen3, the manifest's chat models) think before they answer
    // unless their chat template is told not to. llama-server returns that as
    // `reasoning_content`, which no feature shows: on a CPU it costs tens of seconds per
    // request and can use up the whole output budget, leaving drafts empty. Templates that do
    // not know the flag ignore it.
    if matches!(role, RuntimeRole::Chat | RuntimeRole::Draft) {
        v["extra_body"] =
            serde_json::json!({ "chat_template_kwargs": { "enable_thinking": false } });
    }
    let cfg: ProviderConfig = serde_json::from_value(v)?;
    cfg.validate()?;
    Ok(cfg)
}

/// Where a plan listens, for logs.
fn listen_display(plan: &LaunchPlan) -> String {
    match &plan.socket {
        Some(p) => p.display().to_string(),
        None => format!("127.0.0.1:{}", plan.port),
    }
}

/// `(port, socket)` for the status of a server started from `plan`.
fn listen_status(plan: &LaunchPlan) -> (Option<u16>, Option<PathBuf>) {
    match &plan.socket {
        Some(p) => (None, Some(p.clone())),
        None => (Some(plan.port), None),
    }
}

/// How the supervisor reaches a server started from `plan`.
fn target(client: &reqwest::Client, plan: &LaunchPlan) -> Result<Target> {
    match &plan.socket {
        Some(p) => Target::unix(p),
        None => Ok(Target::tcp(client, plan.port)),
    }
}

/// A free loopback port: `preferred` if it can still be bound, otherwise one from the OS.
///
/// The port is released again before the server binds it, so somebody else may take it in
/// between; the supervisor therefore checks who answers on it before trusting it (see
/// [`crate`]'s security notes).
pub fn choose_port(preferred: Option<u16>) -> Result<u16> {
    if let Some(p) = preferred
        && p != 0
        && TcpListener::bind((Ipv4Addr::LOCALHOST, p)).is_ok()
    {
        return Ok(p);
    }
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    Ok(l.local_addr()?.port())
}

/// Spawn the server into the role's process slot and wait until it is ready (and verified to
/// be the one listening). On failure the process is gone again.
async fn start_once(
    client: &reqwest::Client,
    opts: &RuntimeOptions,
    rs: &RoleState,
    launch: &Launch,
    key: &Secret,
    log: &SharedLog,
) -> Result<()> {
    check_binary(&launch.binary)?;
    let target = target(client, &launch.plan)?;
    if let Some(sock) = &launch.plan.socket {
        // Left behind by an earlier server of ours that died; the new one binds afresh.
        socket::remove_stale(sock);
    }
    // A new process: what an earlier one printed is not this one's detail.
    lock_log(log).clear_server_tail();
    lock_log(log).note(&format!(
        "starting {} on {} (ctx {}x{}, ngl {}, accel {})",
        launch.entry.id,
        listen_display(&launch.plan),
        launch.plan.parallel,
        launch.plan.ctx_per_slot,
        launch.plan.gpu_layers.as_arg(),
        launch.accel.as_str()
    ));
    let proc = ServerProcess::spawn(&launch.binary, &launch.plan.args(), key, log)?;
    // Dropping a previous (dead) process kills whatever it left in its group.
    let previous = lock(&rs.proc).replace(proc);
    drop(previous);
    match wait_ready(&target, &rs.proc, opts.health_timeout, opts.health_poll).await {
        Ok(()) => Ok(()),
        Err(failure) => {
            let proc = lock(&rs.proc).take();
            if let Some(mut p) = proc {
                p.shutdown(Duration::from_millis(200)).await;
            }
            let what = role_what(rs.role);
            let (note, e) = match failure {
                StartFailure::Cancelled => return Err(Error::Cancelled),
                StartFailure::Exited(status) => (
                    format!("start failed: llama-server exited during start-up ({status})"),
                    Error::Provider(format!(
                        "The {what} couldn't start: the model server quit ({status}). Open \
                         Status to retry or to see its log."
                    )),
                ),
                StartFailure::TimedOut(t) => (
                    format!("start failed: not ready after {} s", t.as_secs()),
                    Error::Provider(format!(
                        "The {what} didn't finish loading within {} s. Open Status to retry \
                         or to see its log.",
                        secs_up(t)
                    )),
                ),
                StartFailure::Foreign(who) => (
                    format!("start failed: another process answered on the port: {who}"),
                    Error::Provider(
                        "Another program answered on the model server's port, so Heed \
                         refused to use it."
                            .into(),
                    ),
                ),
            };
            // The technical account goes to the server's log and the daemon log (once); the
            // error stays one short sentence.
            let detail = lock_log(log).last_server_line();
            tracing::warn!(role = role_name(rs.role), model = %launch.entry.id,
                reason = %note, last_output = detail.as_deref().unwrap_or(""),
                "local runtime did not start");
            lock_log(log).note(&note);
            rs.set(|st| st.detail = detail.map(|d| truncate_chars(&d, 300)));
            Err(e)
        }
    }
}

/// `s` cut to at most `max` characters (with an ellipsis when cut).
fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

fn check_binary(p: &Path) -> Result<()> {
    match std::fs::metadata(p) {
        Ok(m) if m.is_file() => Ok(()),
        _ => Err(Error::NotFound(format!(
            "llama-server binary {} not found",
            p.display()
        ))),
    }
}

enum Event {
    Cancelled,
    Tick,
}

/// Why a ready server has to be restarted.
enum Fault {
    /// The process exited, after this much uptime.
    Exited(std::process::ExitStatus, Duration),
    /// It stopped answering `/health` (accepts connections, never replies), after this much
    /// uptime.
    Hung(Duration),
}

/// Watch one server: restart it with backoff when it dies or hangs, stop it when idle.
async fn supervise(
    rs: Arc<RoleState>,
    ctx: Ctx,
    mut launch: Launch,
    mut key: Secret,
    cancel: CancellationToken,
) {
    let opts = &ctx.cfg.opts;
    let mut failures = 0u32;
    let mut last_idle_check = Instant::now();
    let mut last_counters: Option<f64> = None;
    let mut last_probe = Instant::now();
    let mut strikes = 0u32;
    // The token counters at the watchdog's last look (see the hang check).
    let mut watch_counters: Option<f64> = None;
    let Ok(log) = rs.log(opts, &key) else {
        return;
    };
    let Ok(mut server) = target(&ctx.client, &launch.plan) else {
        return;
    };
    let what = role_what(rs.role);
    loop {
        let ev = tokio::select! {
            _ = cancel.cancelled() => Event::Cancelled,
            _ = tokio::time::sleep(opts.watch_interval) => Event::Tick,
        };
        if matches!(ev, Event::Cancelled) {
            return;
        }

        // Crashed?
        let exited = {
            let mut g = lock(&rs.proc);
            let Some(p) = g.as_mut() else { return };
            p.try_wait().map(|s| (s, p.started.elapsed()))
        };
        let fault = match exited {
            Some((status, uptime)) => Some(Fault::Exited(status, uptime)),
            // Hung? Probed often while requests wait on the server, rarely when idle.
            None => {
                let every = if rs.busy() {
                    opts.hang_check_interval
                } else {
                    opts.hang_check_idle_interval
                };
                if opts.hang_strikes > 0 && last_probe.elapsed() >= every {
                    let health = tokio::select! {
                        _ = cancel.cancelled() => return,
                        h = probe_within(&server, opts.hang_probe_timeout) => h,
                    };
                    last_probe = Instant::now();
                    if health == Health::Ready {
                        strikes = 0;
                    } else if health == Health::Down {
                        // No answer in time. A server that is merely swamped (every HTTP worker
                        // busy, or swapping) still makes progress: its token counters move.
                        let counters = tokio::select! {
                            _ = cancel.cancelled() => return,
                            a = activity_within(&server, &key, opts.hang_probe_timeout) => a,
                        };
                        let progressed = match (counters, watch_counters) {
                            (Some((now, _)), Some(before)) => now != before,
                            _ => false,
                        };
                        if let Some((now, _)) = counters {
                            watch_counters = Some(now);
                        }
                        if progressed {
                            strikes = 0;
                        } else {
                            strikes += 1;
                        }
                        tracing::debug!(
                            role = role_name(rs.role),
                            strikes,
                            ?health,
                            "local runtime missed a health check"
                        );
                    }
                    if strikes >= opts.hang_strikes {
                        let uptime = lock(&rs.proc)
                            .as_ref()
                            .map_or(Duration::ZERO, |p| p.started.elapsed());
                        Some(Fault::Hung(uptime))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        };

        if let Some(fault) = fault {
            strikes = 0;
            let (error, note, uptime) = match &fault {
                Fault::Exited(status, uptime) => (
                    format!("The {what} stopped unexpectedly; restarting it."),
                    format!("llama-server exited unexpectedly ({status})"),
                    *uptime,
                ),
                Fault::Hung(uptime) => (
                    format!("The {what} stopped responding; restarting it."),
                    format!(
                        "llama-server stopped responding ({} health checks in a row went \
                         unanswered); restarting it",
                        opts.hang_strikes
                    ),
                    *uptime,
                ),
            };
            // Not ready, and no config for the server's port, before anything else: requests
            // fail fast from here on (see `wait_ready_state`).
            let hung = matches!(fault, Fault::Hung(_));
            rs.set(|st| {
                st.state = Some(RuntimeState::Failed);
                st.error = Some(error);
                st.config = None;
                if hung {
                    st.hangs += 1;
                }
            });
            let proc = lock(&rs.proc).take();
            if let Some(mut p) = proc {
                match fault {
                    Fault::Exited(..) => p.kill_stragglers(),
                    // A hung (or stopped) process may never act on a polite signal: a short
                    // grace, then it is killed, which also fails its open requests at once.
                    Fault::Hung(_) => p.shutdown(Duration::from_millis(200)).await,
                }
            }
            if uptime >= opts.stable_after {
                failures = 0;
            }
            failures += 1;
            tracing::warn!(role = role_name(rs.role), reason = %note, failures,
                "local runtime failed");
            lock_log(&log).note(&note);
            match restart(&rs, &ctx, &mut launch, &log, &cancel, &mut failures).await {
                Some(k) => key = k,
                None => return,
            }
            match target(&ctx.client, &launch.plan) {
                Ok(t) => server = t,
                Err(_) => return,
            }
            last_counters = None;
            watch_counters = None;
            last_probe = Instant::now();
            continue;
        }

        // Idle?
        if let Some(idle) = opts.idle_shutdown
            && last_idle_check.elapsed() >= opts.idle_check_interval
        {
            last_idle_check = Instant::now();
            match activity(&server, &key).await {
                Some((counters, busy)) => {
                    if busy || last_counters.is_some_and(|c| c != counters) {
                        rs.touch();
                    }
                    last_counters = Some(counters);
                }
                // No metrics: cannot tell, so treat as active.
                None => rs.touch(),
            }
            if rs.inflight.load(Ordering::SeqCst) > 0 {
                rs.touch();
            }
            if lock(&rs.activity).elapsed() >= idle {
                tracing::info!(role = role_name(rs.role), "stopping idle local runtime");
                lock_log(&log).note("stopping: idle");
                // Withdraw the config before the server is signalled.
                rs.set(|st| {
                    st.state = Some(RuntimeState::Stopping);
                    st.error = None;
                    st.config = None;
                });
                let proc = lock(&rs.proc).take();
                if let Some(mut p) = proc {
                    p.shutdown(opts.shutdown_grace).await;
                }
                orphan::remove(&rs.pid_file);
                rs.mark_stopped();
                return;
            }
        }
    }
}

/// Restart after a crash or hang, with a new API key. Returns the new key, or `None` if
/// cancelled or given up.
async fn restart(
    rs: &RoleState,
    ctx: &Ctx,
    launch: &mut Launch,
    log: &SharedLog,
    cancel: &CancellationToken,
    failures: &mut u32,
) -> Option<Secret> {
    let opts = &ctx.cfg.opts;
    let what = role_what(rs.role);
    loop {
        if *failures > opts.max_restarts {
            lock_log(log).note(&format!(
                "gave up after {} consecutive failures; waiting for a retry",
                *failures
            ));
            tracing::warn!(
                role = role_name(rs.role),
                failures = *failures,
                "local runtime keeps failing; gave up until a retry"
            );
            orphan::remove(&rs.pid_file);
            rs.set(|st| {
                st.state = Some(RuntimeState::Failed);
                st.error = Some(format!(
                    "The {what} keeps stopping, so Heed stopped restarting it. Open Status to \
                     retry or to see its log."
                ));
                st.config = None;
                st.retry_at = None;
                st.gave_up = true;
            });
            return None;
        }
        let backoff = opts.backoff(*failures);
        rs.set(|st| st.retry_at = Some(Instant::now() + backoff));
        tokio::select! {
            _ = cancel.cancelled() => return None,
            _ = tokio::time::sleep(backoff) => {}
            // The user pressed Retry: restart now.
            _ = rs.wake.notified() => {}
        }
        // Keep the port unless something else took it. Whoever holds it now, the new server
        // must turn out to be the listener before it is trusted (see `wait_ready`). A socket
        // keeps its path.
        if launch.plan.socket.is_none() {
            launch.plan.port = choose_port(Some(launch.plan.port)).unwrap_or(launch.plan.port);
        }
        let (port, sock) = listen_status(&launch.plan);
        rs.set(|st| {
            st.state = Some(RuntimeState::Starting);
            st.config = None;
        });
        let load_started = Instant::now();
        let started = async {
            let (generation, key) = ctx.secrets.rotate(rs.role)?;
            lock_log(log).add_secret(key.expose());
            start_once(&ctx.client, opts, rs, launch, &key, log).await?;
            Ok::<_, Error>((generation, key))
        };
        let started = tokio::select! {
            _ = cancel.cancelled() => return None,
            r = started => r,
        };
        match started {
            Ok((generation, key)) => {
                let model_sha = lock(&rs.status).model_sha.clone().unwrap_or_default();
                rs.record_load(&model_sha, load_started.elapsed());
                let cfg = provider_config(rs.role, &launch.entry, &launch.plan, generation).ok();
                if let Some(pid) = rs.pid() {
                    orphan::write(
                        &rs.pid_file,
                        &PidRecord {
                            pid,
                            exe: launch.binary.clone(),
                            port: launch.plan.port,
                        },
                    );
                }
                rs.set(|st| {
                    st.state = Some(RuntimeState::Ready);
                    st.error = None;
                    st.detail = None;
                    st.port = port;
                    st.socket = sock;
                    st.config = cfg;
                    st.restarts += 1;
                });
                tracing::info!(role = role_name(rs.role), listen = %listen_display(&launch.plan),
                    "local runtime restarted");
                return Some(key);
            }
            Err(e) => {
                *failures += 1;
                rs.set(|st| {
                    st.state = Some(RuntimeState::Failed);
                    st.error = Some(plain(rs.role, &e));
                    if st.detail.is_none() && !matches!(e, Error::Provider(_)) {
                        st.detail = Some(truncate_chars(&e.to_string(), 300));
                    }
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tiers: Vec<Tier>) -> ModelEntry {
        ModelEntry {
            id: "m".into(),
            family: "test".into(),
            file_name: "m.gguf".into(),
            url: "https://example.org/m.gguf".into(),
            mirrors: vec![],
            revision: "0".repeat(40),
            size: 1,
            sha256: "0".repeat(64),
            min_ram_gb: 1,
            tiers,
            license: "Apache-2.0".into(),
            min_app_version: "0.0.1".into(),
        }
    }

    #[test]
    fn check_role_runs_a_reranker_only_for_the_rerank_role() {
        use RuntimeRole as R;
        use Tier as T;
        // A reranker (even mixed with other tiers, which the manifest refuses) never chats or
        // embeds; the rerank role takes only rerankers; draft takes any chat model.
        for (role, tiers, ok) in [
            (R::Rerank, vec![T::Rerank], true),
            (R::Chat, vec![T::Rerank], false),
            (R::Embedding, vec![T::Rerank], false),
            (R::Draft, vec![T::Rerank], false),
            (R::Chat, vec![T::Triage, T::Embedding, T::Rerank], false),
            (
                R::Embedding,
                vec![T::Triage, T::Embedding, T::Rerank],
                false,
            ),
            (R::Rerank, vec![T::Embedding], false),
            (R::Rerank, vec![T::Triage, T::Synthesis], false),
            (R::Chat, vec![T::Light], true),
            (R::Embedding, vec![T::Embedding], true),
            (R::Chat, vec![T::Embedding], false),
            (R::Embedding, vec![T::Light], false),
            (R::Draft, vec![T::Light], true),
            (R::Draft, vec![T::Synthesis], true),
            (R::Draft, vec![T::Embedding], false),
        ] {
            let res = check_role(role, &entry(tiers.clone()));
            assert_eq!(res.is_ok(), ok, "{role:?} {tiers:?}");
            if let Err(e) = res {
                assert!(matches!(e, Error::Invalid(_)), "{e}");
            }
        }
    }
}
