//! Connection to `nuggetd`: lazy connect, reconnect on demand, notification forwarding, and
//! starting the daemon when it is not running.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;
use nugget_core::ipc::notification;
use nugget_core::paths::Paths;
use nugget_ipc::{Client, IpcError};
use serde::de::DeserializeOwned;
use serde::{Serialize, Serializer};
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

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
    /// Every daemon notification as `{ method, params }`, including unknown ones.
    pub const NOTIFICATION: &str = "nugget:notification";
    /// A page id (`"status"`, ...): the tray asks the window to navigate.
    pub const NAVIGATE: &str = "nugget:navigate";
}

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
        tracing::info!(socket = %self.socket(), "connected to nuggetd");
        crate::tray::set_connected(app, true);
        let _ = app.emit(event::CONNECTION, self.info(None));
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
        let mut child = spawn_detached(&exe)
            .map_err(|e| CmdError::Spawn(format!("Couldn't start {}: {e}", exe.display())))?;

        let deadline = Instant::now() + START_TIMEOUT;
        let result = loop {
            tokio::time::sleep(START_POLL).await;
            match self.client(app).await {
                Ok(_) => break Ok(self.info(None)),
                Err(CmdError::NotRunning(_)) => {}
                Err(e) => break Err(e),
            }
            match child.try_wait() {
                // Some daemons fork and exit 0; keep polling for the socket in that case.
                Ok(Some(status)) if !status.success() => {
                    break Err(CmdError::Spawn(format!(
                        "nuggetd exited ({status}) before it was ready. Check its log for details."
                    )));
                }
                Ok(_) | Err(_) => {}
            }
            if Instant::now() >= deadline {
                break Err(CmdError::Spawn(format!(
                    "nuggetd started but did not answer within {} s.",
                    START_TIMEOUT.as_secs()
                )));
            }
        };
        // Reap the child whenever it exits, without blocking the runtime.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        result
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
        if daemon.generation.load(Ordering::SeqCst) == generation {
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
        let started = Instant::now();
        let r: Result<(), _> = retry_busy(|| async {
            n.fetch_add(1, Ordering::SeqCst);
            Err(busy())
        })
        .await;
        assert!(r.unwrap_err().is_busy());
        assert_eq!(n.load(Ordering::SeqCst), 4);
        assert!(started.elapsed() >= BUSY_BACKOFF.iter().sum::<Duration>());

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

        let v = serde_json::to_value(CmdError::from_ipc(IpcError::Disconnected, "status")).unwrap();
        assert_eq!(v["kind"], "disconnected");
        assert!(v["code"].is_null());

        let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        let v = serde_json::to_value(CmdError::from_ipc(IpcError::Connect(refused), "connect"))
            .unwrap();
        assert_eq!(v["kind"], "not_running");
    }

    #[test]
    fn notifications_map_to_events() {
        assert_eq!(
            event_for(notification::KNOWLEDGE_CHANGED),
            Some("nugget:knowledge-changed")
        );
        assert_eq!(
            event_for(notification::SOURCES_CHANGED),
            Some(event::SOURCES_CHANGED)
        );
        assert_eq!(event_for("something.new"), None);
        // Tauri event names allow only `[A-Za-z0-9-/:_]`.
        for name in [
            event::CHAT_DELTA,
            event::CHAT_DONE,
            event::MODELS_PROGRESS,
            event::INGEST_PROGRESS,
            event::SOURCES_CHANGED,
            event::KNOWLEDGE_CHANGED,
        ] {
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-/:_".contains(c)),
                "{name}"
            );
        }
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
    fn finds_daemon_on_path() {
        let dir = std::env::temp_dir().join(format!("nugget-desktop-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join(daemon_file_name());
        write_exe(&exe);
        let missing = std::env::temp_dir().join("nugget-desktop-test-nonexistent");
        let path = std::env::join_paths([missing, dir.clone()]).unwrap();
        assert_eq!(find_on_path(&daemon_file_name(), path), Some(exe));
        let _ = std::fs::remove_dir_all(dir);
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
