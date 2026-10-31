//! Desktop-integration preferences and shell strings (II4): notifications, quit behaviour,
//! paused indexing, the tray's and notifications' localized labels, and sleep/resume detection.
//!
//! The preferences live in `platform.json` under the app's config directory, next to the
//! updater's `settings.json` (which keeps autostart). The daemon never reads either file.
//!
//! Labels: the tray menu and OS notifications are drawn by Rust, but the strings live in the
//! frontend catalogs. The main window sends the current locale's strings with
//! `shell_labels_set` whenever the language changes; until then English defaults are used. Only
//! known keys are accepted, each clipped, so the webview cannot inject arbitrary menu items.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::daemon::CmdError;

/// Minutes since local midnight.
pub type Minute = u16;
const MINUTES_PER_DAY: Minute = 24 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuietHours {
    /// Start, in minutes after local midnight (`0..1440`).
    pub start: Minute,
    /// End (exclusive). An end before the start spans midnight (22:00–07:00).
    pub end: Minute,
}

impl QuietHours {
    /// Whether `now` (minutes after local midnight) falls inside the quiet period.
    pub fn contains(&self, now: Minute) -> bool {
        let (s, e, n) = (self.start, self.end, now % MINUTES_PER_DAY);
        match s.cmp(&e) {
            std::cmp::Ordering::Equal => false,
            std::cmp::Ordering::Less => s <= n && n < e,
            std::cmp::Ordering::Greater => n >= s || n < e,
        }
    }

    fn valid(&self) -> bool {
        self.start < MINUTES_PER_DAY && self.end < MINUTES_PER_DAY
    }
}

/// OS notifications. Off until the user opts in (onboarding or Settings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifySettings {
    pub enabled: bool,
    /// The morning brief and due reminders (opens Today).
    pub brief: bool,
    /// An agent waiting for a confirmation or an answer (opens Agent).
    pub agent: bool,
    /// No notifications during these hours (they are dropped, not queued).
    pub quiet: Option<QuietHours>,
}

impl Default for NotifySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            brief: true,
            agent: true,
            quiet: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlatformSettings {
    pub notifications: NotifySettings,
    /// Quitting Nugget also stops a `nuggetd` this app started. Off: the daemon keeps indexing
    /// in the background (a daemon run as a login service or from a terminal is never stopped).
    pub quit_stops_daemon: bool,
    /// Sources the tray's "Pause indexing" turned off, turned back on by "Resume indexing".
    pub paused_sources: Vec<String>,
}

impl PlatformSettings {
    fn validate(&self) -> Result<(), String> {
        if let Some(q) = &self.notifications.quiet
            && !q.valid()
        {
            return Err("quiet hours must be times of day".into());
        }
        Ok(())
    }
}

pub fn load(path: &Path) -> PlatformSettings {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<PlatformSettings>(&bytes)
            .ok()
            .filter(|s| s.validate().is_ok())
            .unwrap_or_else(|| {
                tracing::warn!(path = %path.display(), "ignoring unreadable platform settings");
                PlatformSettings::default()
            }),
        Err(_) => PlatformSettings::default(),
    }
}

/// Write via a temporary file and rename, so a crash never leaves half a file.
pub fn save(path: &Path, settings: &PlatformSettings) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

// ---- labels ----

/// Every string the shell draws itself, with its English default. Keys match the frontend
/// catalog keys (`shell.*` in `en.json`).
pub const LABELS: &[(&str, &str)] = &[
    ("shell.tray.running", "Nugget is running"),
    ("shell.tray.notRunning", "Nugget isn't running"),
    ("shell.tray.checking", "Nugget: checking…"),
    ("shell.tray.indexing", "Indexing…"),
    ("shell.tray.indexed", "Index up to date"),
    ("shell.tray.indexPaused", "Indexing paused"),
    ("shell.tray.modelReady", "Model ready"),
    ("shell.tray.modelMissing", "No model set up"),
    ("shell.tray.agentRunning", "An agent is running"),
    ("shell.tray.agentWaiting", "An agent needs you"),
    ("shell.tray.agentIdle", "No agents running"),
    ("shell.tray.ask", "Ask…"),
    ("shell.tray.doubleTap", "Double Tap"),
    ("shell.tray.pause", "Pause indexing"),
    ("shell.tray.resume", "Resume indexing"),
    ("shell.tray.stopAgents", "Stop all agents"),
    ("shell.tray.resumeAgents", "Resume agents"),
    ("shell.tray.open", "Open Nugget"),
    ("shell.tray.status", "Status"),
    ("shell.tray.updates", "Updates…"),
    ("shell.tray.quit", "Quit Nugget"),
    ("shell.notify.agentTitle", "An agent needs your attention"),
    (
        "shell.notify.agentBody",
        "Open Nugget to review what it wants to do.",
    ),
    ("shell.notify.open", "Open"),
];

/// Longest accepted label (characters).
const MAX_LABEL_LEN: usize = 120;

#[derive(Debug, Clone)]
pub struct Labels(BTreeMap<&'static str, String>);

impl Default for Labels {
    fn default() -> Self {
        Self(LABELS.iter().map(|(k, v)| (*k, (*v).to_owned())).collect())
    }
}

impl Labels {
    pub fn get(&self, key: &str) -> String {
        self.0.get(key).cloned().unwrap_or_else(|| key.to_owned())
    }

    /// Take the known keys from `from`, one line each, clipped; unknown keys and empty values
    /// are ignored. Returns how many were applied.
    pub fn apply(&mut self, from: &BTreeMap<String, String>) -> usize {
        let mut n = 0;
        for (key, _) in LABELS {
            let Some(v) = from.get(*key) else { continue };
            let v: String = v
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(MAX_LABEL_LEN)
                .collect();
            let v = v.trim();
            if !v.is_empty() {
                self.0.insert(key, v.to_owned());
                n += 1;
            }
        }
        n
    }
}

// ---- state ----

pub struct Platform {
    path: Option<PathBuf>,
    settings: Mutex<PlatformSettings>,
    labels: Mutex<Labels>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Platform {
    pub fn new(path: Option<PathBuf>) -> Self {
        let settings = path.as_deref().map(load).unwrap_or_default();
        Self {
            path,
            settings: Mutex::new(settings),
            labels: Mutex::new(Labels::default()),
        }
    }

    pub fn settings(&self) -> PlatformSettings {
        lock(&self.settings).clone()
    }

    pub fn update(
        &self,
        f: impl FnOnce(&mut PlatformSettings),
    ) -> Result<PlatformSettings, String> {
        let mut s = lock(&self.settings);
        let mut next = s.clone();
        f(&mut next);
        next.validate()?;
        if let Some(path) = &self.path {
            save(path, &next).map_err(|e| format!("couldn't save {}: {e}", path.display()))?;
        }
        *s = next.clone();
        Ok(next)
    }

    pub fn label(&self, key: &str) -> String {
        lock(&self.labels).get(key)
    }
}

/// The label for `key` (English before the app is set up).
pub fn label<R: Runtime>(app: &AppHandle<R>, key: &str) -> String {
    app.try_state::<Platform>()
        .map(|p| p.label(key))
        .unwrap_or_else(|| Labels::default().get(key))
}

pub fn setup(app: &tauri::App) {
    let path = app
        .path()
        .app_config_dir()
        .map(|d| d.join("platform.json"))
        .map_err(
            |e| tracing::warn!(error = %e, "no config directory; platform settings won't persist"),
        )
        .ok();
    app.manage(Platform::new(path));
    spawn_resume_watch(app.handle().clone());
}

// ---- commands ----

#[tauri::command]
pub fn platform_settings_get(app: AppHandle) -> PlatformSettings {
    app.state::<Platform>().settings()
}

/// Save the notification and quit preferences. `paused_sources` is owned by the tray and kept.
#[tauri::command]
pub fn platform_settings_set(
    app: AppHandle,
    settings: PlatformSettings,
) -> Result<PlatformSettings, CmdError> {
    app.state::<Platform>()
        .update(|s| {
            s.notifications = settings.notifications;
            s.quit_stops_daemon = settings.quit_stops_daemon;
        })
        .map_err(CmdError::Other)
}

/// The main window's current-locale strings for the tray and notifications.
#[tauri::command]
pub fn shell_labels_set(app: AppHandle, labels: BTreeMap<String, String>) -> usize {
    if labels.len() > LABELS.len() * 2 {
        return 0;
    }
    let n = lock(&app.state::<Platform>().labels).apply(&labels);
    crate::tray::relabel(&app);
    n
}

// ---- quit ----

/// On quit: stop the `nuggetd` this app started, when the user chose that. A daemon run as a
/// login service or started by hand is left alone (the service manager would restart it, and it
/// is not ours to stop).
pub fn on_exit(app: &AppHandle) {
    let Some(p) = app.try_state::<Platform>() else {
        return;
    };
    if !p.settings().quit_stops_daemon {
        return;
    }
    match app.state::<crate::daemon::Daemon>().started_pid() {
        Some(pid) => {
            tracing::info!(pid, "stopping the nuggetd this app started");
            if let Err(e) = stop_process(pid) {
                tracing::warn!(error = %e, "couldn't stop nuggetd");
            }
        }
        None => tracing::info!("nuggetd wasn't started by the app; leaving it running"),
    }
}

/// SIGTERM: the daemon finishes in-flight work (up to its shutdown grace) and exits.
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

/// The daemon runs without a console, so there is no Ctrl+Break to send it: it is ended (its
/// SQLite store is crash-safe; an interrupted sync resumes from its cursor next time).
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

// ---- sleep / resume ----

/// Event emitted to every window after the machine wakes up.
pub const RESUMED_EVENT: &str = "nugget:resumed";
const TICK: Duration = Duration::from_secs(5);
/// A tick that took this much longer than [`TICK`] of wall-clock time means the machine slept.
const SLEEP_GAP: Duration = Duration::from_secs(30);

/// Whether the wall clock moved far more than a tick: the thread did not run while the machine
/// was asleep. (A big clock adjustment looks the same; refreshing then is harmless.)
pub fn slept(before: SystemTime, after: SystemTime) -> bool {
    after
        .duration_since(before)
        .is_ok_and(|d| d >= TICK + SLEEP_GAP)
}

/// Watch for sleep/resume and, after a wake-up: reconnect to the daemon (the old socket may be
/// dead), refresh the tray, re-register global shortcuts, and tell the windows.
fn spawn_resume_watch(app: AppHandle) {
    let spawned = std::thread::Builder::new()
        .name("resume-watch".into())
        .spawn(move || {
            let mut last = SystemTime::now();
            loop {
                std::thread::sleep(TICK);
                let now = SystemTime::now();
                if slept(last, now) {
                    tracing::info!("woke from sleep; reconnecting");
                    on_resume(&app);
                }
                last = now;
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "no sleep/resume watch");
    }
}

fn on_resume(app: &AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let daemon = handle.state::<crate::daemon::Daemon>();
        let _ = daemon.connection_info(&handle).await;
        crate::tray::refresh(&handle).await;
        crate::doubletap::flow::restart_listener_async(&handle).await;
        let _ = handle.emit(RESUMED_EVENT, ());
    });
    crate::quickask::restart_listener(app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_hours_within_a_day_and_across_midnight() {
        let day = QuietHours {
            start: 13 * 60,
            end: 14 * 60,
        };
        assert!(!day.contains(13 * 60 - 1));
        assert!(day.contains(13 * 60));
        assert!(!day.contains(14 * 60));
        let night = QuietHours {
            start: 22 * 60,
            end: 7 * 60,
        };
        assert!(night.contains(23 * 60));
        assert!(night.contains(0));
        assert!(night.contains(7 * 60 - 1));
        assert!(!night.contains(7 * 60));
        assert!(!night.contains(12 * 60));
        let none = QuietHours { start: 60, end: 60 };
        assert!(!none.contains(60));
    }

    #[test]
    fn notifications_are_opt_in_and_files_tolerate_garbage() {
        let s = PlatformSettings::default();
        assert!(!s.notifications.enabled);
        assert!(s.notifications.brief && s.notifications.agent);
        assert!(!s.quit_stops_daemon);
        let dir = std::env::temp_dir().join(format!("nugget-platform-{}", std::process::id()));
        let path = dir.join("platform.json");
        assert_eq!(load(&path), s);
        let mut on = s.clone();
        on.notifications.enabled = true;
        on.notifications.quiet = Some(QuietHours {
            start: 1320,
            end: 420,
        });
        save(&path, &on).unwrap();
        assert_eq!(load(&path), on);
        std::fs::write(
            &path,
            br#"{"notifications":{"quiet":{"start":5000,"end":1}}}"#,
        )
        .unwrap();
        assert_eq!(load(&path), s, "an invalid time falls back to the defaults");
        std::fs::write(&path, b"{nope").unwrap();
        assert_eq!(load(&path), s);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn update_rejects_invalid_quiet_hours() {
        let p = Platform::new(None);
        let r = p.update(|s| {
            s.notifications.quiet = Some(QuietHours {
                start: 0,
                end: 1440,
            })
        });
        assert!(r.is_err());
        assert_eq!(p.settings(), PlatformSettings::default());
    }

    #[test]
    fn labels_accept_only_known_keys_on_one_line() {
        let mut l = Labels::default();
        let from: BTreeMap<String, String> = [
            ("shell.tray.quit", "Nugget beenden"),
            ("shell.tray.open", "  Line\nbreak\u{7}  "),
            ("shell.tray.status", "   "),
            ("evil.key", "Click me"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        assert_eq!(l.apply(&from), 2);
        assert_eq!(l.get("shell.tray.quit"), "Nugget beenden");
        assert_eq!(l.get("shell.tray.open"), "Line break");
        assert_eq!(
            l.get("shell.tray.status"),
            "Status",
            "blank keeps the default"
        );
        assert_eq!(l.get("evil.key"), "evil.key");
        let long: BTreeMap<String, String> =
            [("shell.tray.ask".to_owned(), "x".repeat(500))].into();
        l.apply(&long);
        assert_eq!(l.get("shell.tray.ask").chars().count(), MAX_LABEL_LEN);
    }

    #[test]
    fn a_long_wall_clock_gap_is_a_sleep() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert!(!slept(t0, t0 + TICK));
        assert!(!slept(t0, t0 + TICK + Duration::from_secs(5)));
        assert!(slept(t0, t0 + Duration::from_secs(3600)));
        // The clock going backwards is not a sleep.
        assert!(!slept(t0, t0 - Duration::from_secs(3600)));
    }
}
