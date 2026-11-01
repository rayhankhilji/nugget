//! Connection to `nuggetd`: lazy connect, reconnect on demand, notification forwarding, and
//! starting the daemon when it is not running.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;
use nugget_core::ipc::{EgressRecordRequest, method, notification};
use nugget_core::paths::Paths;
use nugget_ipc::{Client, IpcError};
use serde::de::DeserializeOwned;
use serde::{Serialize, Serializer};
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

use crate::lifecycle::Ownership;

/// Tauri event names (Tauri allows only `[A-Za-z0-9-/:_]`, so dots become dashes).
pub mod event {
    /// `ConnectionInfo`, whenever the daemon connection comes up or goes down.
    pub const CONNECTION: &str = "nugget:connection";
    /// `nugget_core::ipc::ChatDelta`.
    pub const CHAT_DELTA: &str = "nugget:chat-delta";
    /// `nugget_core::ipc::ChatDone`.
    pub const CHAT_DONE: &str = "nugget:chat-done";
    /// `nugget_core::ipc::ModelProgress`.
    pub const MODELS_PROGRESS: &str = "nugget:models-progress";
    /// `nugget_core::ipc::IngestProgress`.
    pub const INGEST_PROGRESS: &str = "nugget:ingest-progress";
    /// The params of `sources.changed`, as sent (the UI refetches `sources.list` on it).
    pub const SOURCES_CHANGED: &str = "nugget:sources-changed";
    /// `nugget_core::ipc::KnowledgeChanged`: entities whose facts, mentions or pages changed.
    pub const KNOWLEDGE_CHANGED: &str = "nugget:knowledge-changed";
    /// `nugget_core::ipc::AskRestarted`: an Ask stream moved on to another provider.
    pub const ASK_RESTARTED: &str = "nugget:ask-restarted";
    /// Every daemon notification as `{ method, params }`, including unknown ones.
    pub const NOTIFICATION: &str = "nugget:notification";
    /// A page id (`"status"`, ...): the tray asks the window to navigate.
    pub const NAVIGATE: &str = "nugget:navigate";
}

/// Ledger rows the app keeps while `nuggetd` is unreachable; beyond this the oldest are dropped
/// (with a warning). A day of update checks is a handful of rows.
const MAX_QUEUED_EGRESS: usize = 256;

/// How long `start_daemon` waits for a freshly spawned daemon to answer.
const START_TIMEOUT: Duration = Duration::from_secs(20);
const START_POLL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionInfo {
    pub connected: bool,
    /// Socket path or pipe name.
    pub socket: String,
    /// Why the last connection attempt failed.
    pub error: Option<String>,
}

/// Error returned by every command. Serialized as `{ kind, message, code }` for `api.ts`.
#[derive(Debug, thiserror::Error)]
pub enum CmdError {
    #[error("Nugget isn't running: {0}")]
    NotRunning(String),
    #[error("lost the connection to Nugget")]
    Disconnected,
    #[error("{message}")]
    Rpc { code: i64, message: String },
    #[error("unexpected response from Nugget: {0}")]
    Protocol(String),
    #[error("{0}")]
    Spawn(String),
    #[error("{0}")]
    Other(String),
}

impl CmdError {
    fn kind(&self) -> &'static str {
        match self {
            Self::NotRunning(_) => "not_running",
            Self::Disconnected => "disconnected",
            Self::Rpc { .. } => "rpc",
            Self::Protocol(_) => "protocol",
            Self::Spawn(_) => "spawn",
            Self::Other(_) => "other",
        }
    }

    fn from_ipc(err: IpcError, method: &str) -> Self {
        match err {
            IpcError::Connect(e) => Self::NotRunning(e.to_string()),
            IpcError::Disconnected | IpcError::Io(_) => Self::Disconnected,
            IpcError::Rpc(e) => Self::Rpc {
                code: e.code,
                message: e.message,
            },
            IpcError::Serde(e) => Self::Protocol(format!("{method}: {e}")),
            IpcError::Protocol(e) => Self::Protocol(format!("{method}: {e}")),
            other => Self::Other(other.to_string()),
        }
    }
}

impl Serialize for CmdError {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("CmdError", 3)?;
        st.serialize_field("kind", self.kind())?;
        st.serialize_field("message", &self.to_string())?;
        st.serialize_field(
            "code",
            &match self {
                Self::Rpc { code, .. } => Some(*code),
                _ => None,
            },
        )?;
        st.end()
    }
}

/// Waits before each retry of a call the daemon turned away as busy (`SERVER_BUSY`: the
/// connection is at its in-flight cap and the call did not run), about a second in all.
pub const BUSY_BACKOFF: [Duration; 3] = [
    Duration::from_millis(100),
    Duration::from_millis(300),
    Duration::from_millis(600),
];

/// Run `call`, retrying with [`BUSY_BACKOFF`] while the daemon answers `SERVER_BUSY`. Any other
/// outcome (and the last busy answer) is returned as is.
pub async fn retry_busy<T, F, Fut>(mut call: F) -> Result<T, IpcError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, IpcError>>,
{
    for wait in BUSY_BACKOFF {
        match call().await {
            Err(e) if e.is_busy() => tokio::time::sleep(wait).await,
            other => return other,
        }
    }
    call().await
}

pub struct Daemon {
    paths: Result<Paths, String>,
    client: Mutex<Option<Client>>,
    /// Bumped on every new connection, so a stale forwarder never reports a newer
    /// connection as down.
    generation: AtomicU64,
    start_lock: Mutex<()>,
    /// The pid of a `nuggetd` this app started that still runs (0: none).
    started_pid: std::sync::Arc<std::sync::atomic::AtomicU32>,
    /// Requests the app made itself (update checks), waiting to go to the daemon's ledger.
    egress: std::sync::Mutex<EgressQueue>,
}

impl Default for Daemon {
    fn default() -> Self {
        Self::new()
    }
}

impl Daemon {
    pub fn new() -> Self {
        Self {
            paths: Paths::discover().map_err(|e| e.to_string()),
            client: Mutex::new(None),
            generation: AtomicU64::new(0),
            start_lock: Mutex::new(()),
            started_pid: Default::default(),
            egress: std::sync::Mutex::new(EgressQueue::default()),
        }
    }

    /// The `nuggetd` this app started, while it runs (never one run as a service or by hand).
    pub fn started_pid(&self) -> Option<u32> {
        Some(self.started_pid.load(Ordering::SeqCst)).filter(|p| *p != 0)
    }

    /// The data directories, if they could be found.
    pub fn paths_opt(&self) -> Option<&Paths> {
        self.paths.as_ref().ok()
    }

    fn record_path(&self) -> Option<PathBuf> {
        self.paths.as_ref().ok().map(crate::lifecycle::record_path)
    }

    /// Whether the daemon that reports `reported_pid` / `service` in `status` is the app's own:
    /// started this session, or by an earlier one with the same process still running (see
    /// `lifecycle`). A stale record (its process gone) is removed.
    pub fn ownership(&self, reported_pid: Option<u32>, service: Option<&str>) -> Ownership {
        if service.is_some() {
            return Ownership::Service;
        }
        if let Some(p) = self.started_pid()
            && reported_pid.is_none_or(|r| r == p)
        {
            return Ownership::Owned;
        }
        let path = self.record_path();
        let rec = path.as_deref().and_then(crate::lifecycle::read_record);
        let live = rec
            .as_ref()
            .and_then(|r| crate::lifecycle::process_start_time(r.pid));
        if let (Some(r), Some(path)) = (&rec, &path)
            && live != Some(r.start_time)
        {
            crate::lifecycle::remove_record(path);
        }
        crate::lifecycle::ownership(rec.as_ref(), live, reported_pid, service)
    }

    /// The pid and OS start time of the app's own daemon, if one still runs.
    pub fn owned_target(&self, reported_pid: Option<u32>) -> Option<(u32, u64)> {
        let rec = self
            .record_path()
            .and_then(|p| crate::lifecycle::read_record(&p));
        if let Some(r) = rec
            && reported_pid.is_none_or(|p| p == r.pid)
            && crate::lifecycle::process_start_time(r.pid) == Some(r.start_time)
        {
            return Some((r.pid, r.start_time));
        }
        let pid = self.started_pid()?;
        crate::lifecycle::process_start_time(pid).map(|t| (pid, t))
    }

    /// The app's own daemon has stopped.
    pub fn forget_owned(&self) {
        if let Some(p) = self.record_path() {
            crate::lifecycle::remove_record(&p);
        }
    }

    fn paths(&self) -> Result<&Paths, CmdError> {
        self.paths.as_ref().map_err(|e| CmdError::Other(e.clone()))
    }

    pub fn socket(&self) -> String {
        self.paths
            .as_ref()
            .map(nugget_ipc::socket_display)
            .unwrap_or_default()
    }

    /// The live client, if the app is connected now (never connects).
    pub async fn existing_client(&self) -> Option<Client> {
        self.client
            .lock()
            .await
            .as_ref()
            .filter(|c| !c.is_closed())
            .cloned()
    }

    /// The live client, connecting first if needed.
    pub async fn client(&self, app: &AppHandle) -> Result<Client, CmdError> {
        let mut slot = self.client.lock().await;
        if let Some(c) = slot.as_ref()
            && !c.is_closed()
        {
            return Ok(c.clone());
        }
        *slot = None;
        let client = Client::connect(self.paths()?)
            .await
            .map_err(|e| CmdError::from_ipc(e, "connect"))?;
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        forward_notifications(app.clone(), &client, generation, self.socket());
        *slot = Some(client.clone());
        drop(slot);
        // Rows recorded while the daemon was away go out as soon as it answers.
        if !self.egress_queue().is_empty() {
            let (app, client) = (app.clone(), client.clone());
            tauri::async_runtime::spawn(async move {
                use tauri::Manager;
                app.state::<Daemon>().flush_egress(&client).await;
            });
        }
        tracing::info!(socket = %self.socket(), "connected to nuggetd");
        crate::tray::set_connected(app, true);
        let _ = app.emit(event::CONNECTION, self.info(None));
        // A daemon left over from before an update is replaced or reported.
        crate::lifecycle::connected(app);
        Ok(client)
    }

    fn info(&self, error: Option<String>) -> ConnectionInfo {
        ConnectionInfo {
            connected: error.is_none(),
            socket: self.socket(),
            error,
        }
    }

    pub async fn connection_info(&self, app: &AppHandle) -> ConnectionInfo {
        match self.client(app).await {
            Ok(_) => self.info(None),
            Err(e) => {
                crate::tray::set_connected(app, false);
                self.info(Some(e.to_string()))
            }
        }
    }

    /// Call a daemon method, decoding the result into `R`.
    pub async fn call<P, R>(&self, app: &AppHandle, method: &str, params: &P) -> Result<R, CmdError>
    where
        P: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let client = self.client(app).await?;
        retry_busy(|| client.call(method, params))
            .await
            .map_err(|e| CmdError::from_ipc(e, method))
    }

    fn egress_queue(&self) -> std::sync::MutexGuard<'_, EgressQueue> {
        self.egress.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Put a request the app made itself into the daemon's egress ledger (`egress.record`). If
    /// the daemon isn't reachable the row waits in memory and goes out when it connects; rows
    /// still waiting when the app quits are lost.
    pub async fn record_egress(&self, app: &AppHandle, row: EgressRecordRequest) {
        tracing::debug!(host = %row.host, feature = %row.feature, received = row.bytes_received, "egress");
        self.egress_queue().push(row);
        if let Ok(client) = self.client(app).await {
            self.flush_egress(&client).await;
        }
    }

    /// Send every queued row, oldest first. A row the daemon refuses for good (an older daemon
    /// without `egress.record`, or invalid) is dropped with a warning; on any other error (lost
    /// connection, still busy, a storage error) the rest wait for the next connection.
    async fn flush_egress(&self, client: &Client) {
        let mut rows = self.egress_queue().take().into_iter();
        while let Some(row) = rows.next() {
            let sent =
                retry_busy(|| client.call::<_, serde_json::Value>(method::EGRESS_RECORD, &row));
            match sent.await {
                Ok(_) => {}
                Err(IpcError::Rpc(e))
                    if e.code == nugget_ipc::code::INVALID_PARAMS
                        || e.code == nugget_ipc::code::METHOD_NOT_FOUND =>
                {
                    tracing::warn!(host = %row.host, feature = %row.feature, error = %e.message, "nuggetd refused an egress row; dropped");
                }
                Err(e) => {
                    tracing::debug!(error = %e, "egress rows wait for the next connection");
                    self.egress_queue()
                        .requeue(std::iter::once(row).chain(rows).collect());
                    return;
                }
            }
        }
    }

    /// Spawn `nuggetd` if it is not answering, then wait until it does.
    pub async fn start(&self, app: &AppHandle) -> Result<ConnectionInfo, CmdError> {
        let _one_at_a_time = self.start_lock.lock().await;
        if self.client(app).await.is_ok() {
            return Ok(self.info(None));
        }
        let exe = find_nuggetd().ok_or_else(|| {
            CmdError::Spawn(
                "Couldn't find nuggetd next to the app or on PATH. Reinstall Nugget, or start nuggetd yourself."
                    .into(),
            )
        })?;
        tracing::info!(exe = %exe.display(), "starting nuggetd");
        let mut child = spawn_detached(&exe).map_err(|e| {
            CmdError::Spawn(format!(
                "Couldn't start {}: {e}.{}",
                exe.display(),
                crate::logs::hint_suffix()
            ))
        })?;
        // Recorded at once, so a later session can still prove this daemon is the app's own.
        let pid = child.id();
        match (
            self.record_path(),
            crate::lifecycle::process_start_time(pid),
        ) {
            (Some(path), Some(start_time)) => crate::lifecycle::write_record(
                &path,
                &crate::lifecycle::OwnedRecord {
                    pid,
                    start_time,
                    exe: exe.clone(),
                },
            ),
            _ => tracing::debug!(pid, "nuggetd already exited, or no data directory"),
        }

        let deadline = Instant::now() + START_TIMEOUT;
        let result = loop {
            tokio::time::sleep(START_POLL).await;
            match self.client(app).await {
                Ok(_) => break Ok(self.info(None)),
                Err(CmdError::NotRunning(_)) => {}
                Err(e) => break Err(e),
            }
            let exited = child.try_wait();
            if let (Ok(Some(_)), Some(path)) = (&exited, self.record_path()) {
                // Reaped: its pid is free for another process now.
                crate::lifecycle::forget_record_of(&path, pid);
            }
            match exited {
                // Some daemons fork and exit 0; keep polling for the socket in that case.
                Ok(Some(status)) if !status.success() => {
                    break Err(CmdError::Spawn(format!(
                        "nuggetd exited ({status}) before it was ready.{}",
                        crate::logs::hint_suffix()
                    )));
                }
                Ok(_) | Err(_) => {}
            }
            if Instant::now() >= deadline {
                break Err(CmdError::Spawn(format!(
                    "nuggetd started but did not answer within {} s.{}",
                    START_TIMEOUT.as_secs(),
                    crate::logs::hint_suffix()
                )));
            }
        };
        self.started_pid.store(pid, Ordering::SeqCst);
        let started = self.started_pid.clone();
        let record = self.record_path();
        // Reap the child whenever it exits, without blocking the runtime. Its record goes with
        // it: once reaped its pid can be reused, and a process that took it within the start
        // time's resolution (a second outside Linux) would pass for this daemon.
        std::thread::spawn(move || {
            let _ = child.wait();
            if let Some(path) = record {
                crate::lifecycle::forget_record_of(&path, pid);
            }
            let _ = started.compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst);
        });
        result
    }
}

/// Ledger rows waiting for the daemon, oldest first, at most [`MAX_QUEUED_EGRESS`].
#[derive(Default)]
struct EgressQueue {
    rows: std::collections::VecDeque<EgressRecordRequest>,
}

impl EgressQueue {
    fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    fn push(&mut self, row: EgressRecordRequest) {
        self.rows.push_back(row);
        self.trim();
    }

    fn take(&mut self) -> Vec<EgressRecordRequest> {
        self.rows.drain(..).collect()
    }

    /// Put rows that couldn't be sent back in front of any queued since.
    fn requeue(&mut self, rows: Vec<EgressRecordRequest>) {
        for row in rows.into_iter().rev() {
            self.rows.push_front(row);
        }
        self.trim();
    }

    fn trim(&mut self) {
        while self.rows.len() > MAX_QUEUED_EGRESS {
            if let Some(old) = self.rows.pop_front() {
                tracing::warn!(host = %old.host, feature = %old.feature, "nuggetd unreachable; egress row dropped");
            }
        }
    }
}

/// The dedicated Tauri event a daemon notification is re-emitted as, if it has one.
fn event_for(method: &str) -> Option<&'static str> {
    match method {
        notification::CHAT_DELTA => Some(event::CHAT_DELTA),
        notification::CHAT_DONE => Some(event::CHAT_DONE),
        notification::MODELS_PROGRESS => Some(event::MODELS_PROGRESS),
        notification::INGEST_PROGRESS => Some(event::INGEST_PROGRESS),
        notification::SOURCES_CHANGED => Some(event::SOURCES_CHANGED),
        notification::KNOWLEDGE_CHANGED => Some(event::KNOWLEDGE_CHANGED),
        notification::ASK_RESTARTED => Some(event::ASK_RESTARTED),
        _ => None,
    }
}

fn forward_notifications(app: AppHandle, client: &Client, generation: u64, socket: String) {
    let mut notes = client.notifications();
    tauri::async_runtime::spawn(async move {
        while let Some(n) = notes.next().await {
            if let Some(name) = event_for(&n.method)
                && let Err(e) = app.emit(name, &n.params)
            {
                tracing::warn!(error = %e, event = name, "emit failed");
            }
            let _ = app.emit(event::NOTIFICATION, &n);
        }
        // The stream ends when the connection closes.
        use tauri::Manager;
        let daemon = app.state::<Daemon>();
        // While quitting nothing is redrawn: the daemon going away is expected.
        if daemon.generation.load(Ordering::SeqCst) == generation
            && !crate::lifecycle::is_exiting(&app)
        {
            tracing::info!("disconnected from nuggetd");
            crate::tray::set_connected(&app, false);
            let _ = app.emit(
                event::CONNECTION,
                ConnectionInfo {
                    connected: false,
                    socket,
                    error: Some("connection closed".into()),
                },
            );
        }
    });
}

fn daemon_file_name() -> String {
    format!("nuggetd{}", std::env::consts::EXE_SUFFIX)
}

/// `nuggetd` next to the app binary (installed bundles, `target/<profile>/` in dev), else on PATH.
pub fn find_nuggetd() -> Option<PathBuf> {
    let name = daemon_file_name();
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&name)));
    sibling
        .filter(|p| is_executable(p))
        .or_else(|| find_on_path(&name, std::env::var_os("PATH")?))
}

/// Search `PATH` for `name`. Relative and empty entries (which would resolve against whatever
/// the current directory happens to be) are skipped, as are files that are not executable.
fn find_on_path(name: &str, path: std::ffi::OsString) -> Option<PathBuf> {
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|p| is_executable(p))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// Start the daemon so it outlives the app and never grabs a console or the terminal.
fn spawn_detached(exe: &Path) -> std::io::Result<Child> {
    let mut cmd = Command::new(exe);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group: Ctrl-C in the terminal that launched the app doesn't reach it.
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(n: u64) -> EgressRecordRequest {
        EgressRecordRequest {
            at: None,
            host: "github.com".into(),
            feature: "update_check".into(),
            bytes_sent: 0,
            bytes_received: n,
        }
    }

    #[test]
    fn egress_queue_keeps_order_and_a_bounded_size() {
        let mut q = EgressQueue::default();
        q.push(row(1));
        q.push(row(2));
        let taken = q.take();
        assert!(q.is_empty());
        q.push(row(3));
        // Unsent rows go back in front of the ones queued since.
        q.requeue(taken);
        let order: Vec<_> = q.take().iter().map(|r| r.bytes_received).collect();
        assert_eq!(order, [1, 2, 3]);

        for n in 0..(MAX_QUEUED_EGRESS as u64 + 10) {
            q.push(row(n));
        }
        let kept = q.take();
        assert_eq!(kept.len(), MAX_QUEUED_EGRESS);
        assert_eq!(kept[0].bytes_received, 10, "the oldest are dropped");
    }

    fn busy() -> IpcError {
        IpcError::Rpc(nugget_ipc::RpcError::new(
            nugget_ipc::code::SERVER_BUSY,
            "busy",
        ))
    }

    #[tokio::test]
    async fn busy_calls_are_retried_with_a_bounded_backoff() {
        use std::sync::atomic::AtomicU32;
        // Busy twice, then answered.
        let n = AtomicU32::new(0);
        let r = retry_busy(|| async {
            if n.fetch_add(1, Ordering::SeqCst) < 2 {
                Err(busy())
            } else {
                Ok(7)
            }
        })
        .await;
        assert_eq!(r.unwrap(), 7);
        assert_eq!(n.load(Ordering::SeqCst), 3);

        // Always busy: four tries, then the busy error itself.
        let n = AtomicU32::new(0);
        let r: Result<(), _> = retry_busy(|| async {
            n.fetch_add(1, Ordering::SeqCst);
            Err(busy())
        })
        .await;
        assert!(r.unwrap_err().is_busy());
        assert_eq!(n.load(Ordering::SeqCst), 4);

        // Other errors are not retried.
        let n = AtomicU32::new(0);
        let r: Result<(), _> = retry_busy(|| async {
            n.fetch_add(1, Ordering::SeqCst);
            Err(IpcError::Disconnected)
        })
        .await;
        assert!(matches!(r, Err(IpcError::Disconnected)));
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn errors_serialize_for_the_frontend() {
        let e = CmdError::Rpc {
            code: -32001,
            message: "no such model".into(),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "rpc");
        assert_eq!(v["code"], -32001);
        assert_eq!(v["message"], "no such model");

        let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        let v = serde_json::to_value(CmdError::from_ipc(IpcError::Connect(refused), "connect"))
            .unwrap();
        assert_eq!(v["kind"], "not_running");
    }

    fn write_exe(path: &Path) {
        std::fs::write(path, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn path_search_skips_relative_entries_and_non_executables() {
        let base = std::env::temp_dir().join(format!("nugget-desktop-path-{}", std::process::id()));
        let name = daemon_file_name();

        // A daemon reachable through a relative PATH entry (`../../tmp/...`, resolved against the
        // current directory) or an empty one must not be picked up, even though it exists.
        let rel_dir = base.join("rel");
        std::fs::create_dir_all(&rel_dir).unwrap();
        write_exe(&rel_dir.join(&name));
        let cwd = std::env::current_dir().unwrap();
        let mut rel = PathBuf::new();
        for _ in cwd.components().skip(1) {
            rel.push("..");
        }
        let rel_target: PathBuf = rel_dir.components().skip(1).collect();
        let rel = rel.join(rel_target);
        assert!(rel.is_relative());
        if cfg!(unix) {
            assert!(
                rel.join(&name).is_file(),
                "fixture resolves relative to the cwd"
            );
        }
        let path = std::env::join_paths([rel.clone(), PathBuf::new()]).unwrap();
        assert_eq!(find_on_path(&name, path), None);

        // A non-executable file is skipped in favour of a later executable one (Unix only:
        // elsewhere any file counts).
        let plain = base.join("plain");
        let good = base.join("good");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::create_dir_all(&good).unwrap();
        std::fs::write(plain.join(&name), b"").unwrap();
        write_exe(&good.join(&name));
        let path = std::env::join_paths([rel, plain.clone(), good.clone()]).unwrap();
        let found = find_on_path(&name, path);
        if cfg!(unix) {
            assert_eq!(found, Some(good.join(&name)));
        } else {
            assert_eq!(found, Some(plain.join(&name)));
        }

        let _ = std::fs::remove_dir_all(base);
    }
}
