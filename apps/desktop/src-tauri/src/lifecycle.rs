//! The daemon's life around the app's: what quitting does to `nuggetd` (GG6-8), keeping the
//! daemon's version in step with the app's after an update (GG6-10), and the window's stand-ins
//! for the tray when there is none (GG6-20).
//!
//! **Which daemon the app owns.** Only a `nuggetd` this app started, proven by its process id
//! *and* the OS's start time for that process (`<data>/ipc/desktop-owned.json`, written at
//! spawn; a reused pid has another start time). This survives the app quitting while the daemon
//! keeps running, so a later session still knows it. A daemon a service manager runs (it says
//! so in `status`) or one started by hand is never stopped; when the app can't prove it started
//! the daemon, it leaves it and logs why.
//!
//! **Quitting.** Every way out (tray Quit, the app menu's Quit and ⌘Q, closing the window when
//! there is no tray) ends in `RunEvent::ExitRequested` and then `RunEvent::Exit`.
//! `QuitChoice::Ask` (not answered yet) stops the daemon the app started: quitting is expected
//! to quit, and a background service that holds gigabytes of memory for a model nobody uses
//! after the app is gone is the surprise (GG6-8), not the other way round. The first such quit
//! asks once, in the window ("Stop everything" / "Keep running in the background"), and Settings
//! offers the same choice. Where the OS quits without asking the app first (the Dock's Quit,
//! logging out) the unanswered default applies. Stopping is graceful: `daemon.shutdown` over
//! IPC (ingest, the local model servers, the store), a bounded wait for the process to exit,
//! then the old signal (SIGTERM, or ending the process tree on Windows) as a fallback, only
//! after re-checking it is still the same process.
//!
//! **Versions.** On every new connection the app compares the daemon's `status.version` with
//! its own. A daemon it owns is replaced by the bundled one; a login service is restarted
//! (`nuggetd restart-service`); anything else, or a fix that did not work, is reported on the
//! Status page with what to do. One automatic attempt per session; "Check again" retries.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use nugget_core::ipc::{DaemonShutdownRequest, DaemonShutdownResponse, StatusResponse, method};
use nugget_core::paths::Paths;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::daemon::{CmdError, Daemon};
use crate::platform::{Platform, QuitChoice, main_only};

/// The app's version, which a daemon it talks to should match (both come from the workspace).
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod event {
    /// No payload: the window should ask what quitting does ([`super::quit_answer`]).
    pub const QUIT_ASK: &str = "nugget:quit-ask";
    /// [`super::VersionReport`], whenever it changes.
    pub const DAEMON_VERSION: &str = "nugget:daemon-version";
}

/// How long the daemon gets to answer `status` / `daemon.shutdown`.
const CALL_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a stop waits for the process to exit after `daemon.shutdown` (its own grace for
/// background tasks is 5 s, then the model servers and the store).
const STOP_WAIT: Duration = Duration::from_secs(15);
/// After the fallback signal.
const KILL_WAIT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(100);
/// How long a restarted service gets to answer again.
const SERVICE_START_WAIT: Duration = Duration::from_secs(20);

// ---- ownership ----

/// The `nuggetd` this app started: `<data>/ipc/desktop-owned.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedRecord {
    pub pid: u32,
    /// The OS's start time of that process (seconds since the epoch).
    pub start_time: u64,
    /// For the log only (after an update the path may name the new file or a moved bundle).
    pub exe: PathBuf,
}

pub fn record_path(paths: &Paths) -> PathBuf {
    paths.data.join("ipc").join("desktop-owned.json")
}

pub fn read_record(path: &Path) -> Option<OwnedRecord> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

pub fn write_record(path: &Path, rec: &OwnedRecord) {
    let res = (|| {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec(rec).map_err(std::io::Error::other)?,
        )?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = res {
        tracing::warn!(path = %path.display(), error = %e, "couldn't record the nuggetd this app started");
    }
}

pub fn remove_record(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// The OS start time of a running `pid` (seconds since the epoch), `None` once it has exited.
pub fn process_start_time(pid: u32) -> Option<u64> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};
    let pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    sys.process(pid)
        .filter(|p| !matches!(p.status(), ProcessStatus::Zombie | ProcessStatus::Dead))
        .map(|p| p.start_time())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// This app started it (this session or an earlier one).
    Owned,
    /// A service manager runs it (systemd, launchd).
    Service,
    /// Started by hand, by another tool, or the app can't prove it started it.
    NotOwned,
}

/// Whether the answering daemon is the one in `record`.
///
/// - `live_start`: the OS start time of `record.pid` now (`None`: not running).
/// - `reported_pid` / `service`: what the daemon's `status` says (absent from older daemons).
pub fn ownership(
    record: Option<&OwnedRecord>,
    live_start: Option<u64>,
    reported_pid: Option<u32>,
    service: Option<&str>,
) -> Ownership {
    if service.is_some() {
        return Ownership::Service;
    }
    let Some(rec) = record else {
        return Ownership::NotOwned;
    };
    if reported_pid.is_some_and(|p| p != rec.pid) {
        return Ownership::NotOwned;
    }
    match live_start {
        Some(t) if t == rec.start_time => Ownership::Owned,
        // Exited, or the pid now belongs to another process.
        _ => Ownership::NotOwned,
    }
}

/// Why a daemon is left running, for the log.
fn why_not(own: Ownership) -> &'static str {
    match own {
        Ownership::Service => "it runs as a login service",
        _ => {
            "this app can't prove it started it (a login service, started by hand, or by an earlier app it can't verify)"
        }
    }
}

// ---- quitting ----

/// What `RunEvent::ExitRequested` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitRequest {
    Proceed,
    /// Keep the app open and ask what quitting should do first.
    Ask,
}

/// - `code`: the exit code asked for (`Some(RESTART_EXIT_CODE)` for a restart after an update).
/// - `owns_daemon`: there is a daemon quitting would stop.
/// - `asking`: the question is already up; quitting again means "just quit" (the default).
/// - `can_ask`: the main window exists to ask in.
pub fn exit_request(
    code: Option<i32>,
    choice: QuitChoice,
    owns_daemon: bool,
    asking: bool,
    can_ask: bool,
) -> ExitRequest {
    let restart = code == Some(tauri::RESTART_EXIT_CODE);
    if restart || choice != QuitChoice::Ask || !owns_daemon || asking || !can_ask {
        ExitRequest::Proceed
    } else {
        ExitRequest::Ask
    }
}

/// Whether quitting stops the daemon the app started. Unanswered stops it (module docs).
pub fn stops_on_quit(choice: QuitChoice) -> bool {
    choice != QuitChoice::Keep
}

// ---- versions ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionState {
    /// Not checked yet, or the daemon isn't answering.
    Unknown,
    InStep,
    /// Replacing or restarting the daemon.
    Restarting,
    Mismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MismatchReason {
    /// Started by hand or by something else: the user has to restart it.
    NotOwned,
    /// A login service that could not be restarted.
    ServiceRestartFailed,
    /// The app's own daemon could not be replaced.
    RestartFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VersionReport {
    pub app_version: String,
    pub daemon_version: Option<String>,
    pub state: VersionState,
    pub reason: Option<MismatchReason>,
    /// The error, when a restart failed.
    pub detail: Option<String>,
}

impl Default for VersionReport {
    fn default() -> Self {
        Self {
            app_version: APP_VERSION.into(),
            daemon_version: None,
            state: VersionState::Unknown,
            reason: None,
            detail: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionAction {
    InStep,
    /// Stop the app's own daemon and start the bundled one.
    RestartOwned,
    /// Restart the login service, which runs the updated binary.
    RestartService,
    Tell(MismatchReason),
}

/// - `service_hint`: a login service is installed although the daemon couldn't say it runs
///   under one (Windows' scheduled task leaves no trace in the process).
/// - `tried`: this session already tried to fix it.
pub fn version_action(
    app: &str,
    daemon: &str,
    own: Ownership,
    service_hint: bool,
    tried: bool,
) -> VersionAction {
    if app == daemon {
        return VersionAction::InStep;
    }
    let service = own == Ownership::Service || (own == Ownership::NotOwned && service_hint);
    match (tried, own, service) {
        (true, Ownership::Owned, _) => VersionAction::Tell(MismatchReason::RestartFailed),
        (true, _, true) => VersionAction::Tell(MismatchReason::ServiceRestartFailed),
        (_, Ownership::Owned, _) => VersionAction::RestartOwned,
        (false, _, true) => VersionAction::RestartService,
        _ => VersionAction::Tell(MismatchReason::NotOwned),
    }
}

// ---- state ----

#[derive(Default)]
pub struct Lifecycle {
    /// The quit question is showing.
    asking: AtomicBool,
    /// The connected daemon is the app's own (last check).
    owned: AtomicBool,
    reconciling: AtomicBool,
    /// This session already tried to fix a version mismatch by itself.
    auto_fix_tried: AtomicBool,
    /// The app is quitting: no more version fixes (they could start a daemon again).
    exiting: AtomicBool,
    report: Mutex<VersionReport>,
}

/// Whether the app is quitting.
fn exiting(app: &AppHandle) -> bool {
    app.try_state::<Lifecycle>()
        .is_some_and(|lc| lc.exiting.load(Ordering::SeqCst))
}

impl Lifecycle {
    fn report(&self) -> VersionReport {
        self.report
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn set_report(app: &AppHandle, report: VersionReport) {
    let Some(lc) = app.try_state::<Lifecycle>() else {
        return;
    };
    let changed = {
        let mut r = lc.report.lock().unwrap_or_else(PoisonError::into_inner);
        let changed = *r != report;
        *r = report.clone();
        changed
    };
    if changed {
        let _ = app.emit(event::DAEMON_VERSION, report);
    }
}

fn report(
    daemon_version: Option<String>,
    state: VersionState,
    reason: Option<MismatchReason>,
    detail: Option<String>,
) -> VersionReport {
    VersionReport {
        app_version: APP_VERSION.into(),
        daemon_version,
        state,
        reason,
        detail,
    }
}

/// The daemon's `status`, or `None` if it doesn't answer in time.
async fn status(app: &AppHandle) -> Option<StatusResponse> {
    let daemon = app.state::<Daemon>();
    tokio::time::timeout(
        CALL_TIMEOUT,
        daemon.call::<_, StatusResponse>(app, method::STATUS, &json!({})),
    )
    .await
    .ok()
    .and_then(Result::ok)
}

// ---- stopping ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// Not the app's to stop (or nothing answering that the app started).
    Left(Ownership),
    Stopped,
    /// `daemon.shutdown` didn't work in time; the fallback signal did.
    Killed,
    StillRunning,
}

/// Stop the daemon the app owns, gracefully when it can (module docs). `reason` goes to the
/// daemon's log.
pub async fn stop_owned(app: &AppHandle, reason: &str) -> StopOutcome {
    let daemon = app.state::<Daemon>();
    let st = status(app).await;
    let own = daemon.ownership(
        st.as_ref().and_then(|s| s.pid),
        st.as_ref().and_then(|s| s.service.as_deref()),
    );
    let target = match (own, daemon.owned_target(st.as_ref().and_then(|s| s.pid))) {
        (Ownership::Owned, Some(t)) => t,
        (_, None) if st.is_none() => {
            tracing::debug!("no nuggetd is running");
            return StopOutcome::Left(Ownership::NotOwned);
        }
        (own, _) => {
            tracing::info!(reason = why_not(own), "leaving nuggetd running");
            return StopOutcome::Left(own);
        }
    };
    let (pid, start) = target;
    tracing::info!(pid, reason, "stopping the nuggetd this app started");
    let asked = tokio::time::timeout(
        CALL_TIMEOUT,
        daemon.call::<_, DaemonShutdownResponse>(
            app,
            method::DAEMON_SHUTDOWN,
            &DaemonShutdownRequest {
                reason: Some(reason.into()),
            },
        ),
    )
    .await;
    let graceful = matches!(asked, Ok(Ok(_)));
    if !graceful {
        tracing::info!(?asked, "nuggetd didn't take daemon.shutdown; signalling it");
    }
    let gone = || process_start_time(pid) != Some(start);
    if graceful && wait_until(STOP_WAIT, gone).await {
        daemon.forget_owned();
        tracing::info!(pid, "nuggetd stopped");
        return StopOutcome::Stopped;
    }
    // Still the same process (same start time): only then signal it.
    if !gone() {
        if let Err(e) = stop_process(pid) {
            tracing::warn!(pid, error = %e, "couldn't stop nuggetd");
        }
        if wait_until(KILL_WAIT, gone).await {
            daemon.forget_owned();
            tracing::info!(pid, "nuggetd stopped by signal");
            return StopOutcome::Killed;
        }
        tracing::warn!(pid, "nuggetd is still running");
        return StopOutcome::StillRunning;
    }
    daemon.forget_owned();
    StopOutcome::Stopped
}

async fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// SIGTERM: the daemon winds down as on `daemon.shutdown`.
#[cfg(unix)]
fn stop_process(pid: u32) -> std::io::Result<()> {
    let status = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .status()?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| std::io::Error::other(format!("kill exited with {status}")))
}

/// The daemon runs without a console, so there is no Ctrl+Break to send it: the tree is ended.
/// Its model servers are in a kill-on-close job object (`nugget-runtime`), so they go with it
/// even when only the daemon itself is ended.
#[cfg(windows)]
fn stop_process(pid: u32) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let status = std::process::Command::new("taskkill.exe")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| std::io::Error::other(format!("taskkill exited with {status}")))
}

#[cfg(not(any(unix, windows)))]
fn stop_process(_pid: u32) -> std::io::Result<()> {
    Err(std::io::Error::other("not supported on this platform"))
}

// ---- run events ----

/// `RunEvent::ExitRequested`: ask once before the first quit that would stop the daemon.
pub fn on_exit_requested(app: &AppHandle, code: Option<i32>, api: &tauri::ExitRequestApi) {
    let (Some(lc), Some(platform)) = (app.try_state::<Lifecycle>(), app.try_state::<Platform>())
    else {
        return;
    };
    let owns = app.state::<Daemon>().started_pid().is_some() || lc.owned.load(Ordering::SeqCst);
    let can_ask = app.get_webview_window("main").is_some();
    let choice = platform.settings().quit_daemon;
    match exit_request(
        code,
        choice,
        owns,
        lc.asking.load(Ordering::SeqCst),
        can_ask,
    ) {
        ExitRequest::Proceed => {}
        ExitRequest::Ask => {
            api.prevent_exit();
            lc.asking.store(true, Ordering::SeqCst);
            tracing::info!("asking what quitting should do with the background service");
            crate::tray::show_main(app, None);
            let _ = app.emit_to("main", event::QUIT_ASK, ());
        }
    }
}

/// `RunEvent::Exit`: install a staged update (stopping the app's daemon first on macOS and
/// Linux, so the new version starts next time), then stop the daemon unless the user chose to
/// keep it.
pub fn on_exit(app: &AppHandle) {
    if let Some(lc) = app.try_state::<Lifecycle>() {
        lc.exiting.store(true, Ordering::SeqCst);
    }
    for w in app.webview_windows().values() {
        let _ = w.hide();
    }
    let mut stopped = false;
    if let Some(updates) = app.try_state::<crate::updater::Updates>() {
        if cfg!(not(windows)) && updates.has_staged() {
            stopped = !matches!(
                tauri::async_runtime::block_on(stop_owned(app, "update")),
                StopOutcome::Left(_)
            );
        }
        updates.install_staged();
    }
    if stopped {
        return;
    }
    let choice = app
        .try_state::<Platform>()
        .map(|p| p.settings().quit_daemon)
        .unwrap_or_default();
    if stops_on_quit(choice) {
        tauri::async_runtime::block_on(stop_owned(app, "app_quit"));
    } else {
        tracing::info!("leaving nuggetd running, as the user chose");
    }
}

/// A new daemon connection came up: check its version (once at a time).
pub fn connected(app: &AppHandle) {
    if exiting(app) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move { reconcile(&app, false).await });
}

/// GG6-10: compare versions and fix or report a mismatch (module docs).
pub async fn reconcile(app: &AppHandle, user_asked: bool) {
    let Some(lc) = app.try_state::<Lifecycle>() else {
        return;
    };
    if lc.exiting.load(Ordering::SeqCst) || lc.reconciling.swap(true, Ordering::SeqCst) {
        return;
    }
    reconcile_once(app, &lc, user_asked).await;
    lc.reconciling.store(false, Ordering::SeqCst);
}

async fn reconcile_once(app: &AppHandle, lc: &Lifecycle, user_asked: bool) {
    let Some(st) = status(app).await else {
        set_report(app, report(None, VersionState::Unknown, None, None));
        return;
    };
    let daemon = app.state::<Daemon>();
    let own = daemon.ownership(st.pid, st.service.as_deref());
    lc.owned.store(own == Ownership::Owned, Ordering::SeqCst);
    let service_hint =
        cfg!(windows) && app.state::<crate::updater::Updates>().start_at_login() == Some(true);
    let tried = !user_asked && lc.auto_fix_tried.load(Ordering::SeqCst);
    let action = version_action(APP_VERSION, &st.version, own, service_hint, tried);
    let theirs = Some(st.version.clone());
    match action {
        VersionAction::InStep => {
            set_report(app, report(theirs, VersionState::InStep, None, None));
        }
        VersionAction::Tell(reason) => {
            tracing::warn!(app = APP_VERSION, daemon = %st.version, ?reason, "nuggetd is another version than the app");
            set_report(
                app,
                report(theirs, VersionState::Mismatch, Some(reason), None),
            );
        }
        VersionAction::RestartOwned | VersionAction::RestartService => {
            lc.auto_fix_tried.store(true, Ordering::SeqCst);
            set_report(
                app,
                report(theirs.clone(), VersionState::Restarting, None, None),
            );
            tracing::info!(app = APP_VERSION, daemon = %st.version, ?action, "nuggetd is another version than the app; restarting it");
            let failed = if action == VersionAction::RestartOwned {
                restart_owned(app).await.err()
            } else {
                restart_service(app).await.err()
            };
            let now = status(app).await;
            let version = now.as_ref().map(|s| s.version.clone());
            if version.as_deref() == Some(APP_VERSION) {
                tracing::info!(version = APP_VERSION, "nuggetd now matches the app");
                let own = daemon.ownership(
                    now.as_ref().and_then(|s| s.pid),
                    now.as_ref().and_then(|s| s.service.as_deref()),
                );
                lc.owned.store(own == Ownership::Owned, Ordering::SeqCst);
                set_report(app, report(version, VersionState::InStep, None, None));
            } else {
                let reason = if action == VersionAction::RestartOwned {
                    MismatchReason::RestartFailed
                } else {
                    MismatchReason::ServiceRestartFailed
                };
                let detail = failed.or_else(|| {
                    Some(match &version {
                        Some(v) => format!("the restarted service is still version {v}"),
                        None => "the background service didn't come back".into(),
                    })
                });
                tracing::warn!(?reason, detail = ?detail, "couldn't bring nuggetd in step with the app");
                set_report(
                    app,
                    report(
                        version.or(theirs),
                        VersionState::Mismatch,
                        Some(reason),
                        detail,
                    ),
                );
            }
        }
    }
}

/// Replace the app's own daemon with the bundled one.
async fn restart_owned(app: &AppHandle) -> Result<(), String> {
    match stop_owned(app, "version_mismatch").await {
        StopOutcome::Stopped | StopOutcome::Killed => {}
        StopOutcome::Left(_) => return Err("it is no longer the app's own".into()),
        StopOutcome::StillRunning => return Err("it didn't stop".into()),
    }
    if exiting(app) {
        return Err("Nugget is quitting".into());
    }
    app.state::<Daemon>()
        .start(app)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Stop the service's daemon cleanly, then have the service manager start it again (the
/// updated binary). If the service can't be restarted, the bundled daemon is started instead,
/// so Nugget keeps working.
async fn restart_service(app: &AppHandle) -> Result<(), String> {
    let daemon = app.state::<Daemon>();
    let _ = tokio::time::timeout(
        CALL_TIMEOUT,
        daemon.call::<_, DaemonShutdownResponse>(
            app,
            method::DAEMON_SHUTDOWN,
            &DaemonShutdownRequest {
                reason: Some("version_mismatch".into()),
            },
        ),
    )
    .await;
    let paths = Paths::discover().ok();
    let answering = |paths: &Option<Paths>| {
        let paths = paths.clone();
        async move {
            match paths {
                Some(p) => nugget_ipc::connect_stream(&p).await.is_ok(),
                None => false,
            }
        }
    };
    let deadline = Instant::now() + STOP_WAIT;
    while answering(&paths).await && Instant::now() < deadline {
        tokio::time::sleep(POLL).await;
    }
    let restarted = tauri::async_runtime::spawn_blocking(crate::login::restart_service)
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.map_err(|e| e.to_string()));
    if let Err(e) = &restarted {
        tracing::warn!(error = %e, "couldn't restart the login service; starting the bundled nuggetd");
        let _ = daemon.start(app).await;
        return Err(e.clone());
    }
    let deadline = Instant::now() + SERVICE_START_WAIT;
    while Instant::now() < deadline {
        if daemon.client(app).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err("the service didn't answer after the restart".into())
}

// ---- commands ----

/// The user's answer to the quit question (or a choice in Settings with `quit: false`).
#[tauri::command]
pub fn quit_answer(
    app: AppHandle,
    window: tauri::Window,
    choice: QuitChoice,
    quit: bool,
) -> Result<(), CmdError> {
    main_only(&window)?;
    app.state::<Platform>()
        .update(|s| s.quit_daemon = choice)
        .map_err(CmdError::Other)?;
    app.state::<Lifecycle>()
        .asking
        .store(false, Ordering::SeqCst);
    if quit {
        app.exit(0);
    }
    Ok(())
}

/// The quit question was dismissed: Nugget stays open.
#[tauri::command]
pub fn quit_cancel(app: AppHandle, window: tauri::Window) -> Result<(), CmdError> {
    main_only(&window)?;
    app.state::<Lifecycle>()
        .asking
        .store(false, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
pub fn daemon_version(app: AppHandle) -> VersionReport {
    app.state::<Lifecycle>().report()
}

/// "Check again" on the Status page: re-check, and try to fix a mismatch once more.
#[tauri::command]
pub async fn daemon_version_recheck(app: AppHandle) -> VersionReport {
    reconcile(&app, true).await;
    app.state::<Lifecycle>().report()
}

/// What the window needs to stand in for the tray (GG6-20) and point at the logs (GG6-32).
#[derive(Debug, Clone, Serialize)]
pub struct ShellInfo {
    pub has_tray: bool,
    /// "Pause indexing" is in effect.
    pub indexing_paused: bool,
    pub logs: Option<crate::logs::LogPaths>,
}

fn shell_info_now<R: Runtime>(app: &AppHandle<R>) -> ShellInfo {
    ShellInfo {
        has_tray: crate::tray::has_tray(app),
        indexing_paused: app
            .try_state::<Platform>()
            .is_some_and(|p| !p.settings().paused_sources.is_empty()),
        logs: crate::logs::paths(),
    }
}

#[tauri::command]
pub fn shell_info(app: AppHandle) -> ShellInfo {
    shell_info_now(&app)
}

/// The tray's "Pause indexing" / "Resume indexing", from the window.
#[tauri::command]
pub async fn indexing_pause_toggle(
    app: AppHandle,
    window: tauri::Window,
) -> Result<ShellInfo, CmdError> {
    main_only(&window)?;
    crate::tray::toggle_pause(&app).await;
    Ok(shell_info_now(&app))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(pid: u32, start_time: u64) -> OwnedRecord {
        OwnedRecord {
            pid,
            start_time,
            exe: PathBuf::from("/usr/bin/nuggetd"),
        }
    }

    #[test]
    fn only_the_recorded_process_with_its_start_time_is_owned() {
        let r = rec(4242, 1_700_000_000);
        assert_eq!(
            ownership(Some(&r), Some(1_700_000_000), Some(4242), None),
            Ownership::Owned
        );
        // An older daemon doesn't report its pid; the start time still proves it.
        assert_eq!(
            ownership(Some(&r), Some(1_700_000_000), None, None),
            Ownership::Owned
        );
        // The pid was reused by another process.
        assert_eq!(
            ownership(Some(&r), Some(1_700_000_999), Some(4242), None),
            Ownership::NotOwned
        );
        // Ours exited; another daemon answers.
        assert_eq!(
            ownership(Some(&r), None, Some(77), None),
            Ownership::NotOwned
        );
        assert_eq!(
            ownership(Some(&r), Some(1_700_000_000), Some(77), None),
            Ownership::NotOwned
        );
        // Nothing recorded: started by hand or by an app that kept no record.
        assert_eq!(ownership(None, None, Some(4242), None), Ownership::NotOwned);
        // A service manager runs it: never ours, whatever the record says.
        assert_eq!(
            ownership(Some(&r), Some(1_700_000_000), Some(4242), Some("systemd")),
            Ownership::Service
        );
    }

    #[test]
    fn records_round_trip_and_tolerate_garbage() {
        let dir = std::env::temp_dir().join(format!("nugget-owned-{}", std::process::id()));
        let path = record_path(&Paths::rooted(dir.clone()));
        assert_eq!(read_record(&path), None);
        let r = rec(1, 2);
        write_record(&path, &r);
        assert_eq!(read_record(&path), Some(r));
        std::fs::write(&path, b"{nope").unwrap();
        assert_eq!(read_record(&path), None);
        remove_record(&path);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn this_process_has_a_stable_start_time() {
        let me = std::process::id();
        let t = process_start_time(me).expect("running");
        assert_eq!(process_start_time(me), Some(t));
        let r = OwnedRecord {
            pid: me,
            start_time: t,
            exe: std::env::current_exe().unwrap(),
        };
        assert_eq!(
            ownership(Some(&r), process_start_time(me), Some(me), None),
            Ownership::Owned
        );
        assert_eq!(process_start_time(u32::MAX - 1), None);
    }

    #[test]
    fn quitting_asks_once_and_only_when_it_would_stop_something() {
        use ExitRequest as E;
        use QuitChoice as Q;
        // Unanswered, the app started the daemon, the window is there: ask.
        assert_eq!(exit_request(Some(0), Q::Ask, true, false, true), E::Ask);
        assert_eq!(exit_request(None, Q::Ask, true, false, true), E::Ask);
        // Answered either way: just quit.
        assert_eq!(
            exit_request(Some(0), Q::Stop, true, false, true),
            E::Proceed
        );
        assert_eq!(
            exit_request(Some(0), Q::Keep, true, false, true),
            E::Proceed
        );
        // Nothing of ours to stop (a login service, started by hand): nothing to ask.
        assert_eq!(
            exit_request(Some(0), Q::Ask, false, false, true),
            E::Proceed
        );
        // Quitting again while the question is up quits with the default.
        assert_eq!(exit_request(Some(0), Q::Ask, true, true, true), E::Proceed);
        // No window to ask in.
        assert_eq!(
            exit_request(Some(0), Q::Ask, true, false, false),
            E::Proceed
        );
        // Restarting after an update never asks.
        assert_eq!(
            exit_request(Some(tauri::RESTART_EXIT_CODE), Q::Ask, true, false, true),
            E::Proceed
        );
    }

    #[test]
    fn unanswered_and_stop_stop_keep_keeps() {
        assert!(stops_on_quit(QuitChoice::Ask));
        assert!(stops_on_quit(QuitChoice::Stop));
        assert!(!stops_on_quit(QuitChoice::Keep));
    }

    #[test]
    fn version_mismatch_is_fixed_by_whoever_runs_the_daemon() {
        use MismatchReason as M;
        use Ownership as O;
        use VersionAction as A;
        let v = |own, hint, tried| version_action("0.2.0", "0.1.0", own, hint, tried);
        assert_eq!(
            version_action("0.2.0", "0.2.0", O::NotOwned, false, false),
            A::InStep
        );
        assert_eq!(
            version_action("0.2.0", "0.2.0", O::Owned, false, true),
            A::InStep
        );
        // The app's own daemon: replaced by the bundled one.
        assert_eq!(v(O::Owned, false, false), A::RestartOwned);
        assert_eq!(v(O::Owned, true, false), A::RestartOwned);
        // A login service: restarted (Windows: known only from the app's own setting).
        assert_eq!(v(O::Service, false, false), A::RestartService);
        assert_eq!(v(O::NotOwned, true, false), A::RestartService);
        // Started by hand: only the user can restart it.
        assert_eq!(v(O::NotOwned, false, false), A::Tell(M::NotOwned));
        // One automatic attempt per session; after it, say what is wrong.
        assert_eq!(v(O::Owned, false, true), A::Tell(M::RestartFailed));
        assert_eq!(v(O::Service, false, true), A::Tell(M::ServiceRestartFailed));
        assert_eq!(v(O::NotOwned, true, true), A::Tell(M::ServiceRestartFailed));
        assert_eq!(v(O::NotOwned, false, true), A::Tell(M::NotOwned));
        // A newer daemon than the app (an older app opened) is handled the same way.
        assert_eq!(
            version_action("0.1.0", "0.2.0", O::Owned, false, false),
            A::RestartOwned
        );
    }

    #[test]
    fn reports_serialize_for_the_status_page() {
        let r = report(
            Some("0.1.0".into()),
            VersionState::Mismatch,
            Some(MismatchReason::ServiceRestartFailed),
            Some("not installed".into()),
        );
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["app_version"], APP_VERSION);
        assert_eq!(v["state"], "mismatch");
        assert_eq!(v["reason"], "service_restart_failed");
        assert_eq!(v["detail"], "not installed");
        let d = serde_json::to_value(VersionReport::default()).unwrap();
        assert_eq!(d["state"], "unknown");
        assert!(d["reason"].is_null());
    }
}
