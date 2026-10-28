//! The [`Runtime`] supervisor: one `llama-server` per [`RuntimeRole`].

use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use nugget_core::capability::Tier;
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
use crate::health::{ProcSlot, activity, loopback_client, wait_ready};
use crate::logs::{RollingLog, SharedLog, lock as lock_log};
use crate::orphan::{self, PidRecord};
use crate::process::ServerProcess;
use crate::secrets::{RuntimeSecrets, api_key_ref_gen, random_key};

/// Provider id of the local chat server (`local-chat`).
pub const CHAT_PROVIDER_ID: &str = "local-chat";
/// Provider id of the local embedding server (`local-embedding`).
pub const EMBEDDING_PROVIDER_ID: &str = "local-embedding";

/// Provider id the router knows a role's server by.
pub fn provider_id(role: RuntimeRole) -> ProviderId {
    ProviderId::new(match role {
        RuntimeRole::Chat => CHAT_PROVIDER_ID,
        RuntimeRole::Embedding => EMBEDDING_PROVIDER_ID,
    })
}

fn role_name(role: RuntimeRole) -> &'static str {
    match role {
        RuntimeRole::Chat => "chat",
        RuntimeRole::Embedding => "embedding",
    }
}

const ROLES: [RuntimeRole; 2] = [RuntimeRole::Chat, RuntimeRole::Embedding];

fn idx(role: RuntimeRole) -> usize {
    match role {
        RuntimeRole::Chat => 0,
        RuntimeRole::Embedding => 1,
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
    pub chat: Tuning,
    pub embedding: Tuning,
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
            idle_shutdown: None,
            idle_check_interval: Duration::from_secs(30),
            shutdown_grace: Duration::from_secs(5),
            log_max_bytes: 8 * 1024 * 1024,
            log_keep: 3,
            chat: Tuning::default(),
            embedding: Tuning::default(),
        }
    }
}

impl RuntimeOptions {
    fn tuning(&self, role: RuntimeRole) -> &Tuning {
        match role {
            RuntimeRole::Chat => &self.chat,
            RuntimeRole::Embedding => &self.embedding,
        }
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
    /// The provider config callers should use (port included), while the server is ready.
    pub config: Option<ProviderConfig>,
    pub port: Option<u16>,
    /// The server process, from spawn (so also while it is loading) until it is stopped or
    /// found dead.
    pub pid: Option<u32>,
    /// Restarts after crashes since the last successful `ensure`.
    pub restarts: u32,
    pub accel: Option<RuntimeAccel>,
    pub binary: Option<PathBuf>,
    pub log_file: PathBuf,
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

/// Supervises local `llama-server` processes (one per [`RuntimeRole`]) on `127.0.0.1`.
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
    roles: [Arc<RoleState>; 2],
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
    restarts: u32,
    accel: Option<RuntimeAccel>,
    binary: Option<PathBuf>,
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
    activity: Mutex<Instant>,
    /// The supervisor of a ready server.
    task: Mutex<Option<Task>>,
    /// The latest start (running, or finished).
    start: Mutex<Option<Start>>,
    log: Mutex<Option<SharedLog>>,
    log_file: PathBuf,
    pid_file: PathBuf,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl RoleState {
    fn set(&self, f: impl FnOnce(&mut Status)) {
        f(&mut lock(&self.status));
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }

    fn touch(&self) {
        *lock(&self.activity) = Instant::now();
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
            self.set(|st| {
                st.state = Some(RuntimeState::Failed);
                st.config = None;
                st.error = Some(format!("llama-server exited unexpectedly ({status})"));
            });
        }
        drop(g);
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

    /// Withdraw the config (and say `Stopped`) before a server is signalled to stop.
    fn mark_stopping(&self) {
        self.set(|st| {
            if st.state.is_some() {
                st.state = Some(RuntimeState::Stopped);
            }
            st.config = None;
        });
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
        let secrets = RuntimeSecrets::new(random_key()?, random_key()?);
        let mk = |role: RuntimeRole| {
            Arc::new(RoleState {
                role,
                op: tokio::sync::Mutex::new(()),
                proc: Mutex::new(None),
                status: Mutex::new(Status::default()),
                changed: watch::Sender::new(0),
                activity: Mutex::new(Instant::now()),
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
                roles: [mk(RuntimeRole::Chat), mk(RuntimeRole::Embedding)],
            }),
        })
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
                    Some(RuntimeState::Starting | RuntimeState::Failed) => {}
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
                    let err = lock(&rs.status).error.clone().unwrap_or_default();
                    return Err(Error::Provider(format!(
                        "local {} runtime is restarting: {err}",
                        role_name(rs.role)
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
        let previous = lock(&rs.status).port;
        let model_id = launches.first().map(|(_, l)| l.entry.id.clone());
        rs.set(|st| {
            *st = Status {
                port: previous,
                model_id,
                model_sha: Some(model_sha.clone()),
                state: Some(RuntimeState::Starting),
                accel: launches.first().map(|(_, l)| l.accel),
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
            },
            config: if state == RuntimeState::Ready {
                st.config
            } else {
                None
            },
            port: st.port,
            pid: rs.pid(),
            restarts: st.restarts,
            accel: st.accel,
            binary: st.binary,
            log_file: rs.log_file.clone(),
        })
    }

    /// Record activity for the idle timer (e.g. from the code that sends requests).
    pub fn touch(&self, role: RuntimeRole) {
        self.role(role).touch();
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
    let previous = lock(&rs.status).port;
    rs.set(|st| {
        *st = Status {
            port: previous,
            model_id: Some(launch.entry.id.clone()),
            model_sha: Some(model_sha.to_string()),
            state: Some(RuntimeState::Starting),
            accel: Some(launch.accel),
            ..Status::default()
        };
    });
    let res = async {
        if let Some(entry) = &runtime {
            launch.binary = ctx.install_entry(entry, on_progress).await?;
        }
        launch.plan.port = choose_port(previous)?;
        // A fresh key for every server process.
        let (generation, key) = ctx.secrets.rotate(rs.role)?;
        let log = rs.log(opts, &key)?;
        start_once(&ctx.client, opts, rs, &launch, &key, &log).await?;
        Ok::<_, Error>((generation, key))
    }
    .await;
    let (generation, key) = match res {
        Ok(v) => v,
        Err(e) => {
            rs.set(|st| {
                st.state = Some(RuntimeState::Failed);
                st.error = Some(e.to_string());
            });
            return Err(e);
        }
    };
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
        port = launch.plan.port, accel = launch.accel.as_str(), "local runtime ready");
    let binary = launch.binary.clone();
    let port = launch.plan.port;
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
        st.port = Some(port);
        st.config = Some(cfg.clone());
        st.binary = Some(binary);
    });
    Ok(cfg)
}

fn check_role(role: RuntimeRole, model: &ModelEntry) -> Result<()> {
    let embeds = model.tiers.contains(&Tier::Embedding);
    let chats = model.tiers.iter().any(|t| *t != Tier::Embedding);
    match role {
        RuntimeRole::Embedding if !embeds => Err(Error::Invalid(format!(
            "model `{}` does not serve the embedding tier",
            model.id
        ))),
        RuntimeRole::Chat if !chats => Err(Error::Invalid(format!(
            "model `{}` is an embedding-only model",
            model.id
        ))),
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
        RuntimeRole::Chat => model
            .tiers
            .iter()
            .copied()
            .filter(|t| *t != Tier::Embedding)
            .collect(),
    };
    // Built from JSON so fields added to ProviderConfig later take their serde defaults.
    let v = serde_json::json!({
        "id": provider_id(role),
        "kind": "openai_compatible",
        "base_url": format!("http://127.0.0.1:{}/v1", plan.port),
        "model": model.id,
        "api_key": api_key_ref_gen(role, generation).to_string(),
        "tiers": tiers,
        "locality": "on_device",
        "context_tokens": plan.ctx_per_slot,
    });
    let cfg: ProviderConfig = serde_json::from_value(v)?;
    cfg.validate()?;
    Ok(cfg)
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
    lock_log(log).note(&format!(
        "starting {} on 127.0.0.1:{} (ctx {}x{}, ngl {}, accel {})",
        launch.entry.id,
        launch.plan.port,
        launch.plan.parallel,
        launch.plan.ctx_per_slot,
        launch.plan.gpu_layers.as_arg(),
        launch.accel.as_str()
    ));
    let proc = ServerProcess::spawn(&launch.binary, &launch.plan.args(), key, log)?;
    // Dropping a previous (dead) process kills whatever it left in its group.
    let previous = lock(&rs.proc).replace(proc);
    drop(previous);
    match wait_ready(
        client,
        launch.plan.port,
        &rs.proc,
        opts.health_timeout,
        opts.health_poll,
        log,
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(e) => {
            let proc = lock(&rs.proc).take();
            if let Some(mut p) = proc {
                p.shutdown(Duration::from_millis(200)).await;
            }
            if !matches!(e, Error::Cancelled) {
                lock_log(log).note(&format!("start failed: {e}"));
            }
            Err(e)
        }
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

/// Watch one server: restart it with backoff when it dies, stop it when idle.
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
    let Ok(log) = rs.log(opts, &key) else {
        return;
    };
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
        if let Some((status, uptime)) = exited {
            let msg = format!("llama-server exited unexpectedly ({status})");
            // Not ready, and no config for the dead server's port, before anything else.
            rs.set(|st| {
                st.state = Some(RuntimeState::Failed);
                st.error = Some(msg.clone());
                st.config = None;
            });
            if let Some(mut p) = lock(&rs.proc).take() {
                p.kill_stragglers();
            }
            if uptime >= opts.stable_after {
                failures = 0;
            }
            failures += 1;
            tracing::warn!(role = role_name(rs.role), %status, failures, "local runtime crashed");
            lock_log(&log).note(&msg);
            match restart(&rs, &ctx, &mut launch, &log, &cancel, &mut failures).await {
                Some(k) => key = k,
                None => return,
            }
            last_counters = None;
            continue;
        }

        // Idle?
        if let Some(idle) = opts.idle_shutdown
            && last_idle_check.elapsed() >= opts.idle_check_interval
        {
            last_idle_check = Instant::now();
            match activity(&ctx.client, launch.plan.port, &key).await {
                Some((counters, busy)) => {
                    if busy || last_counters.is_some_and(|c| c != counters) {
                        rs.touch();
                    }
                    last_counters = Some(counters);
                }
                // No metrics: cannot tell, so treat as active.
                None => rs.touch(),
            }
            if lock(&rs.activity).elapsed() >= idle {
                tracing::info!(role = role_name(rs.role), "stopping idle local runtime");
                lock_log(&log).note("stopping: idle");
                // Withdraw the config before the server is signalled.
                rs.set(|st| {
                    st.state = Some(RuntimeState::Stopped);
                    st.error = None;
                    st.config = None;
                });
                let proc = lock(&rs.proc).take();
                if let Some(mut p) = proc {
                    p.shutdown(opts.shutdown_grace).await;
                }
                orphan::remove(&rs.pid_file);
                return;
            }
        }
    }
}

/// Restart after a crash, with a new API key. Returns the new key, or `None` if cancelled or
/// given up.
async fn restart(
    rs: &RoleState,
    ctx: &Ctx,
    launch: &mut Launch,
    log: &SharedLog,
    cancel: &CancellationToken,
    failures: &mut u32,
) -> Option<Secret> {
    let opts = &ctx.cfg.opts;
    loop {
        if *failures > opts.max_restarts {
            let msg = format!(
                "gave up after {} consecutive failures; call ensure() to retry",
                *failures
            );
            lock_log(log).note(&msg);
            orphan::remove(&rs.pid_file);
            rs.set(|st| {
                st.state = Some(RuntimeState::Failed);
                st.error = Some(msg);
                st.config = None;
            });
            return None;
        }
        tokio::select! {
            _ = cancel.cancelled() => return None,
            _ = tokio::time::sleep(opts.backoff(*failures)) => {}
        }
        // Keep the port unless something else took it. Whoever holds it now, the new server
        // must turn out to be the listener before it is trusted (see `wait_ready`).
        let port = match choose_port(Some(launch.plan.port)) {
            Ok(p) => p,
            Err(_) => launch.plan.port,
        };
        launch.plan.port = port;
        rs.set(|st| {
            st.state = Some(RuntimeState::Starting);
            st.config = None;
        });
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
                let cfg = provider_config(rs.role, &launch.entry, &launch.plan, generation).ok();
                if let Some(pid) = rs.pid() {
                    orphan::write(
                        &rs.pid_file,
                        &PidRecord {
                            pid,
                            exe: launch.binary.clone(),
                            port,
                        },
                    );
                }
                rs.set(|st| {
                    st.state = Some(RuntimeState::Ready);
                    st.error = None;
                    st.port = Some(port);
                    st.config = cfg;
                    st.restarts += 1;
                });
                tracing::info!(role = role_name(rs.role), port, "local runtime restarted");
                return Some(key);
            }
            Err(e) => {
                *failures += 1;
                rs.set(|st| {
                    st.state = Some(RuntimeState::Failed);
                    st.error = Some(e.to_string());
                });
            }
        }
    }
}
