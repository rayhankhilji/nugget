//! Tray / menu-bar icon: live status (daemon, indexing, model, agents), quick actions (Ask,
//! Double Tap, pause indexing, stop all agents) and Open / Status / Updates / Quit.
//!
//! Per platform:
//! - macOS: a monochrome template image (`icons/tray-template.png`, the nugget silhouette in
//!   black + alpha, 44 px for Retina) that the menu bar tints for light and dark mode. Left click
//!   opens the window, right click the menu.
//! - Windows: the colour app icon in the notification area; left click opens the window, right
//!   click the menu.
//! - Linux: the colour app icon through libayatana-appindicator (or libappindicator), loaded at
//!   runtime, shown by a StatusNotifierItem host. AppIndicator reports no clicks, so everything
//!   (status lines included) lives in the menu. The tray is skipped, with one log line, when no
//!   StatusNotifier host is on the session bus (e.g. GNOME without the AppIndicator extension),
//!   when the library is missing, or when `NUGGET_TRAY=off`; `NUGGET_TRAY=on` forces it
//!   (XEmbed-only trays).
//!
//! `NUGGET_TRAY=off` skips the tray on every platform (the installer smoke tests use it). Without
//! a tray, closing the window quits Heed the way Quit Heed does (`lifecycle`), and the Status
//! page offers the tray's actions instead.
//!
//! The status lines are refreshed from the daemon (`ingest.status`, `status`, `agent.list`) when
//! the connection comes up, shortly after daemon notifications that can change them, every
//! [`POLL`] while connected, and after the machine wakes up. While the daemon is not running only
//! the first line is shown. Labels come from the frontend catalogs (`platform::LABELS`).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nugget_core::ipc::{
    AgentListRequest, AgentTaskState, AgentTaskSummary, IngestStatus, SourceInfo, SourceState,
    StatusResponse, agent_method, agent_notification, method, notification,
};
use serde_json::json;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Listener, Manager, Runtime, Wry};

use crate::daemon::{Daemon, event};
use crate::platform::{Platform, label};

const TRAY_ID: &str = "nugget";
/// How often the status lines are refreshed while connected.
const POLL: Duration = Duration::from_secs(20);
/// Notifications arriving within this window cause one refresh.
const DEBOUNCE: Duration = Duration::from_millis(1500);
/// macOS menu-bar icon: black + alpha only, so the system can tint it.
#[cfg(any(target_os = "macos", test))]
const TEMPLATE_ICON: tauri::image::Image<'static> =
    tauri::include_image!("icons/tray-template.png");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indexing {
    Busy,
    Idle,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agents {
    Idle,
    Running,
    /// Waiting on a confirmation or an answer.
    Waiting,
}

/// What the status lines show. `None`: not known yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub connected: Option<bool>,
    pub indexing: Option<Indexing>,
    pub model_ready: Option<bool>,
    pub agents: Option<Agents>,
}

pub fn indexing_from(status: &IngestStatus, paused: bool) -> Indexing {
    if paused {
        Indexing::Paused
    } else if status.queue_depth > 0
        || status
            .sources
            .iter()
            .any(|s| s.state == SourceState::Syncing)
    {
        Indexing::Busy
    } else {
        Indexing::Idle
    }
}

/// A provider that built without error is assigned to at least one tier.
pub fn model_ready_from(status: &StatusResponse) -> bool {
    status
        .providers
        .iter()
        .any(|p| p.error.is_none() && !p.tiers.is_empty())
}

pub fn agents_from(tasks: &[AgentTaskSummary]) -> Agents {
    let state = |s| tasks.iter().any(|t| t.state == s);
    if state(AgentTaskState::Confirming) || state(AgentTaskState::NeedsInput) {
        Agents::Waiting
    } else if state(AgentTaskState::Running) {
        Agents::Running
    } else {
        Agents::Idle
    }
}

/// Label keys of the status lines, in menu order (the first is always shown).
pub fn line_keys(s: &Snapshot) -> [Option<&'static str>; 4] {
    let connected = match s.connected {
        None => "shell.tray.checking",
        Some(true) => "shell.tray.running",
        Some(false) => "shell.tray.notRunning",
    };
    if s.connected != Some(true) {
        return [Some(connected), None, None, None];
    }
    [
        Some(connected),
        s.indexing.map(|i| match i {
            Indexing::Busy => "shell.tray.indexing",
            Indexing::Idle => "shell.tray.indexed",
            Indexing::Paused => "shell.tray.indexPaused",
        }),
        s.model_ready.map(|m| {
            if m {
                "shell.tray.modelReady"
            } else {
                "shell.tray.modelMissing"
            }
        }),
        s.agents.map(|a| match a {
            Agents::Idle => "shell.tray.agentIdle",
            Agents::Running => "shell.tray.agentRunning",
            Agents::Waiting => "shell.tray.agentWaiting",
        }),
    ]
}

/// Present in app state only when the tray was created; the window then hides on close
/// instead of quitting.
pub struct Tray {
    menu: Menu<Wry>,
    /// Status lines 2–4 (indexing, model, agents) and whether each is in the menu.
    lines: [MenuItem<Wry>; 4],
    shown: Mutex<[bool; 4]>,
    /// Every other relabelable item, with its label key.
    items: Vec<(MenuItem<Wry>, &'static str)>,
    pause: MenuItem<Wry>,
    update: MenuItem<Wry>,
    update_version: Mutex<Option<String>>,
    snapshot: Mutex<Snapshot>,
    refresh_scheduled: AtomicBool,
}

/// A pause or resume is running (from the tray or the window); further clicks are ignored until
/// it is done.
static PAUSING: AtomicBool = AtomicBool::new(false);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Create the tray, or log once why there is none. The app works either way (see module docs).
pub fn init(app: &tauri::App) {
    if let Some(why) = skip_reason() {
        tracing::info!(reason = %why, "no tray icon; closing the window quits Heed");
        return;
    }
    // libappindicator-sys panics (rather than erroring) when neither library can be loaded; the
    // panic happens on this thread, in Rust code, before any tray exists.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build(app))) {
        Ok(Ok(())) => follow_daemon(app.handle()),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "tray icon unavailable; closing the window quits Heed")
        }
        Err(_) => tracing::warn!(
            "tray icon unavailable (no AppIndicator library); closing the window quits Heed"
        ),
    }
}

fn build(app: &tauri::App) -> tauri::Result<()> {
    let h = app.handle();
    let item = |id: &str, key: &str, enabled: bool| {
        MenuItem::with_id(app, id, label(h, key), enabled, None::<&str>)
    };
    let lines = [
        item("line-0", "shell.tray.checking", false)?,
        item("line-1", "shell.tray.indexed", false)?,
        item("line-2", "shell.tray.modelMissing", false)?,
        item("line-3", "shell.tray.agentIdle", false)?,
    ];
    let ask = item("ask", "shell.tray.ask", true)?;
    let double_tap = item("double-tap", "shell.tray.doubleTap", true)?;
    let pause_key = pause_key(app.state::<Platform>().settings().paused_sources.is_empty());
    let pause = item("pause", pause_key, false)?;
    let open = item("open", "shell.tray.open", true)?;
    let status = item("status", "shell.tray.status", true)?;
    let update = item("update", "shell.tray.updates", true)?;
    let quit = item("quit", "shell.tray.quit", true)?;
    let agents = crate::agent::tray_item(app)?;
    let menu = Menu::with_items(
        app,
        &[
            &lines[0],
            &PredefinedMenuItem::separator(app)?,
            &ask,
            &double_tap,
            &PredefinedMenuItem::separator(app)?,
            &pause,
            &agents,
            &PredefinedMenuItem::separator(app)?,
            &open,
            &status,
            &update,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Heed")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| on_menu(app, e.id().as_ref()))
        .on_tray_icon_event(|tray, e| {
            // Not emitted on Linux, where the menu is the only interaction.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
            {
                show_main(tray.app_handle(), None);
            }
        });
    #[cfg(target_os = "macos")]
    {
        builder = builder.icon(TEMPLATE_ICON).icon_as_template(true);
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    app.manage(Tray {
        menu,
        lines,
        shown: Mutex::new([true, false, false, false]),
        items: vec![
            (ask, "shell.tray.ask"),
            (double_tap, "shell.tray.doubleTap"),
            (open, "shell.tray.open"),
            (status, "shell.tray.status"),
            (quit, "shell.tray.quit"),
        ],
        pause,
        update,
        update_version: Mutex::new(None),
        snapshot: Mutex::new(Snapshot::default()),
        refresh_scheduled: AtomicBool::new(false),
    });
    Ok(())
}

fn on_menu(app: &AppHandle, id: &str) {
    match id {
        "open" => show_main(app, None),
        // The update settings panel lives on the Status page.
        "status" | "update" => show_main(app, Some("status")),
        "ask" => crate::quickask::toggle(app),
        "double-tap" => {
            tauri::async_runtime::spawn(crate::doubletap::flow::fire(app.clone()));
        }
        "pause" => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move { toggle_pause(&app).await });
        }
        // Through `RunEvent::ExitRequested`, like every other way to quit (`lifecycle`).
        "quit" => app.exit(0),
        id if id == crate::agent::tray_item_id() => crate::agent::tray_clicked(app),
        _ => {}
    }
}

fn pause_key(nothing_paused: bool) -> &'static str {
    if nothing_paused {
        "shell.tray.pause"
    } else {
        "shell.tray.resume"
    }
}

pub fn has_tray<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.try_state::<Tray>().is_some()
}

/// Show, unminimize and focus the main window, optionally switching page.
pub fn show_main<R: Runtime>(app: &AppHandle<R>, page: Option<&str>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
    if let Some(page) = page {
        let _ = app.emit_to("main", event::NAVIGATE, page);
    }
}

/// Reflect the daemon connection in the tray tooltip and menu.
pub fn set_connected(app: &AppHandle, connected: bool) {
    if let Some(t) = app.try_state::<Tray>() {
        let mut s = lock(&t.snapshot);
        if s.connected == Some(connected) {
            return;
        }
        s.connected = Some(connected);
        drop(s);
        render(app, &t);
    }
}

/// Show "Update available: vX" in the menu (it opens the panel with the release notes), or the
/// plain "Updates…" item.
pub fn set_update(app: &AppHandle, version: Option<&str>) {
    if crate::lifecycle::is_exiting(app) {
        return;
    }
    if let Some(t) = app.try_state::<Tray>() {
        *lock(&t.update_version) = version.map(str::to_owned);
        let _ = t.update.set_text(update_text(app, version));
    }
}

fn update_text(app: &AppHandle, version: Option<&str>) -> String {
    match version {
        // The version string is the only part not localized; it reads the same everywhere.
        Some(v) => format!("{} ({v})", label(app, "shell.tray.updates")),
        None => label(app, "shell.tray.updates"),
    }
}

/// Labels changed (the frontend sent its locale's strings): redraw every item.
pub fn relabel(app: &AppHandle) {
    let Some(t) = app.try_state::<Tray>() else {
        return;
    };
    for (item, key) in &t.items {
        let _ = item.set_text(label(app, key));
    }
    let version = lock(&t.update_version).clone();
    let _ = t.update.set_text(update_text(app, version.as_deref()));
    crate::agent::relabel_tray(app);
    render(app, &t);
}

/// A change to the status lines in the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineOp {
    /// Insert line `.0` at menu position `.1`.
    Insert(usize, usize),
    Remove(usize),
}

/// The inserts and removes that take the menu from `shown` to `keys`, in order (each position
/// counts the lines already shown before it). Line 0 is never removed.
pub fn line_ops(keys: &[Option<&str>; 4], shown: &[bool; 4]) -> Vec<LineOp> {
    let mut now = *shown;
    let mut ops = vec![];
    for i in 0..4 {
        match keys[i] {
            Some(_) if !now[i] => {
                ops.push(LineOp::Insert(i, now[..i].iter().filter(|s| **s).count()));
                now[i] = true;
            }
            None if now[i] && i > 0 => {
                ops.push(LineOp::Remove(i));
                now[i] = false;
            }
            _ => {}
        }
    }
    ops
}

/// Redraw on the main thread. Menu calls made elsewhere block until the main thread runs them,
/// so rendering there (never holding a lock while waiting on it) cannot deadlock with a command
/// that runs on the main thread, and renders never interleave.
fn render(app: &AppHandle, _t: &Tray) {
    // Quitting: the main thread may be busy stopping the daemon; nothing to show anyway.
    if crate::lifecycle::is_exiting(app) {
        return;
    }
    let h = app.clone();
    let posted = app.run_on_main_thread(move || {
        if let Some(t) = h.try_state::<Tray>() {
            render_now(&h, &t);
        }
    });
    if let Err(e) = posted {
        tracing::debug!(error = %e, "tray not redrawn");
    }
}

fn render_now(app: &AppHandle, t: &Tray) {
    let snapshot = *lock(&t.snapshot);
    let keys = line_keys(&snapshot);
    let mut tooltip = vec![];
    for (i, key) in keys.iter().enumerate() {
        if let Some(key) = key {
            let text = label(app, key);
            let _ = t.lines[i].set_text(&text);
            tooltip.push(text);
        }
    }
    let current = *lock(&t.shown);
    for op in line_ops(&keys, &current) {
        let ok = match op {
            LineOp::Insert(i, at) => t.menu.insert(&t.lines[i], at).is_ok(),
            LineOp::Remove(i) => t.menu.remove(&t.lines[i]).is_ok(),
        };
        if ok {
            let mut shown = lock(&t.shown);
            match op {
                LineOp::Insert(i, _) => shown[i] = true,
                LineOp::Remove(i) => shown[i] = false,
            }
        }
    }
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        // Windows clips tooltips at 127 characters.
        let tip: String = tooltip.join(" · ").chars().take(127).collect();
        let _ = tray.set_tooltip(Some(tip));
    }
    let nothing_paused = app
        .try_state::<Platform>()
        .is_none_or(|p| p.settings().paused_sources.is_empty());
    let _ = t.pause.set_text(label(app, pause_key(nothing_paused)));
    let _ = t.pause.set_enabled(snapshot.connected == Some(true));
}

/// Ask the daemon for the status lines and redraw them. A failed call leaves that line as it was.
pub async fn refresh(app: &AppHandle) {
    let Some(t) = app.try_state::<Tray>() else {
        return;
    };
    if crate::lifecycle::is_exiting(app) {
        return;
    }
    if lock(&t.snapshot).connected != Some(true) {
        render(app, &t);
        return;
    }
    let daemon = app.state::<Daemon>();
    let paused = !app.state::<Platform>().settings().paused_sources.is_empty();
    let ingest = daemon
        .call::<_, IngestStatus>(app, method::INGEST_STATUS, &json!({}))
        .await;
    let status = daemon
        .call::<_, StatusResponse>(app, method::STATUS, &json!({}))
        .await;
    let agents = daemon
        .call::<_, Vec<AgentTaskSummary>>(
            app,
            agent_method::AGENT_LIST,
            &AgentListRequest { limit: Some(20) },
        )
        .await;
    {
        let mut s = lock(&t.snapshot);
        if let Ok(i) = &ingest {
            s.indexing = Some(indexing_from(i, paused));
        }
        if let Ok(st) = &status {
            s.model_ready = Some(model_ready_from(st));
        }
        // Without the agent feature the call fails; the line then stays hidden.
        if let Ok(a) = &agents {
            s.agents = Some(agents_from(a));
        }
    }
    render(app, &t);
}

/// Refresh soon, once for a burst of notifications.
fn schedule_refresh(app: &AppHandle) {
    let Some(t) = app.try_state::<Tray>() else {
        return;
    };
    if t.refresh_scheduled.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(DEBOUNCE).await;
        if let Some(t) = app.try_state::<Tray>() {
            t.refresh_scheduled.store(false, Ordering::SeqCst);
        }
        refresh(&app).await;
    });
}

/// Whether a daemon notification can change a status line.
pub fn affects_status(method: &str) -> bool {
    matches!(
        method,
        notification::INGEST_PROGRESS
            | notification::SOURCES_CHANGED
            | agent_notification::AGENT_EVENT
            | agent_notification::POLICY_KILL_CHANGED
    )
}

fn follow_daemon(app: &AppHandle) {
    let h = app.clone();
    app.listen_any(event::CONNECTION, move |_| schedule_refresh(&h));
    let h = app.clone();
    app.listen_any(event::NOTIFICATION, move |e| {
        #[derive(serde::Deserialize)]
        struct Method {
            method: String,
        }
        if serde_json::from_str::<Method>(e.payload()).is_ok_and(|m| affects_status(&m.method)) {
            schedule_refresh(&h);
        }
    });
    let h = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(POLL).await;
            refresh(&h).await;
        }
    });
}

/// "Pause indexing": turn off every enabled source and remember which; "Resume indexing" turns
/// exactly those back on. Survives restarts (the sources stay off in the daemon's config). Also
/// offered in the window when there is no tray (`lifecycle::indexing_pause_toggle`).
pub async fn toggle_pause(app: &AppHandle) {
    if PAUSING.swap(true, Ordering::SeqCst) {
        return;
    }
    toggle_pause_once(app).await;
    PAUSING.store(false, Ordering::SeqCst);
    refresh(app).await;
}

async fn toggle_pause_once(app: &AppHandle) {
    let platform = app.state::<Platform>();
    let daemon = app.state::<Daemon>();
    let paused = platform.settings().paused_sources;
    let result = if paused.is_empty() {
        match daemon
            .call::<_, Vec<SourceInfo>>(app, method::SOURCES_LIST, &json!({}))
            .await
        {
            Ok(sources) => {
                let mut done = vec![];
                for s in sources.iter().filter(|s| s.enabled) {
                    let update = json!({ "id": s.id, "enabled": false });
                    match daemon
                        .call::<_, serde_json::Value>(app, method::SOURCES_UPDATE, &update)
                        .await
                    {
                        Ok(_) => done.push(s.id.to_string()),
                        Err(e) => tracing::warn!(error = %e, "couldn't pause a source"),
                    }
                }
                tracing::info!(sources = done.len(), "indexing paused");
                platform.update(|p| {
                    for id in done {
                        if !p.paused_sources.contains(&id) {
                            p.paused_sources.push(id);
                        }
                    }
                })
            }
            Err(e) => Err(e.to_string()),
        }
    } else {
        let known: Option<Vec<String>> = daemon
            .call::<_, Vec<SourceInfo>>(app, method::SOURCES_LIST, &json!({}))
            .await
            .ok()
            .map(|l| l.iter().map(|s| s.id.to_string()).collect());
        let mut left = vec![];
        for id in &paused {
            // A source removed meanwhile: nothing to resume.
            if known.as_ref().is_some_and(|k| !k.contains(id)) {
                continue;
            }
            let update = json!({ "id": id, "enabled": true });
            if let Err(e) = daemon
                .call::<_, serde_json::Value>(app, method::SOURCES_UPDATE, &update)
                .await
            {
                // Kept, so "Resume indexing" can be tried again.
                tracing::warn!(error = %e, "couldn't resume a source");
                left.push(id.clone());
            }
        }
        tracing::info!(
            sources = paused.len() - left.len(),
            left = left.len(),
            "indexing resumed"
        );
        platform.update(|p| p.paused_sources = left)
    };
    if let Err(e) = result {
        tracing::warn!(error = %e, "pause/resume indexing failed");
    }
}

/// `Some(reason)` when the tray should not be created.
fn skip_reason() -> Option<String> {
    match std::env::var("NUGGET_TRAY").ok().as_deref().map(str::trim) {
        Some("off" | "0") => Some("NUGGET_TRAY=off".into()),
        Some("on" | "1") => None,
        _ => platform_skip_reason(),
    }
}

#[cfg(target_os = "linux")]
fn platform_skip_reason() -> Option<String> {
    linux::skip_reason()
}

#[cfg(not(target_os = "linux"))]
fn platform_skip_reason() -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
mod linux {
    /// Desktops show AppIndicator icons through a StatusNotifier host that owns this name.
    const WATCHER: &str = "org.kde.StatusNotifierWatcher";

    /// `Some(reason)` when no StatusNotifier host can show the tray.
    pub fn skip_reason() -> Option<String> {
        match has_watcher() {
            Ok(true) => None,
            Ok(false) => Some(format!(
                "no StatusNotifier host ({WATCHER}) on the session bus"
            )),
            Err(e) => Some(format!(
                "can't ask the session bus for a StatusNotifier host: {e}"
            )),
        }
    }

    fn has_watcher() -> Result<bool, String> {
        let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
        let name = zbus::names::BusName::try_from(WATCHER).map_err(|e| e.to_string())?;
        let proxy = zbus::blocking::fdo::DBusProxy::new(&conn).map_err(|e| e.to_string())?;
        proxy.name_has_owner(name).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::LABELS;

    #[test]
    fn template_icon_is_black_and_alpha() {
        let icon = TEMPLATE_ICON;
        assert_eq!((icon.width(), icon.height()), (44, 44));
        let px = icon.rgba().as_chunks::<4>().0.iter();
        assert!(px.clone().all(|p| p[..3] == [0, 0, 0]));
        assert!(px.clone().any(|p| p[3] == 255) && px.clone().any(|p| p[3] == 0));
    }

    fn source(state: SourceState) -> SourceInfo {
        serde_json::from_value(json!({
            "id": "s1", "kind": "folder", "name": "Notes", "enabled": true,
            "state": state, "items": 1, "chunks": 1, "settings": {},
        }))
        .expect("source")
    }

    fn ingest(queue_depth: u64, states: &[SourceState]) -> IngestStatus {
        IngestStatus {
            queue_depth,
            freshness_p50_ms: None,
            freshness_p95_ms: None,
            embedding_provider: None,
            triage_provider: None,
            triage: None,
            sources: states.iter().map(|s| source(*s)).collect(),
        }
    }

    #[test]
    fn indexing_line_follows_queue_sync_and_pause() {
        assert_eq!(
            indexing_from(&ingest(0, &[SourceState::Watching]), false),
            Indexing::Idle
        );
        assert_eq!(indexing_from(&ingest(3, &[]), false), Indexing::Busy);
        assert_eq!(
            indexing_from(
                &ingest(0, &[SourceState::Idle, SourceState::Syncing]),
                false
            ),
            Indexing::Busy
        );
        assert_eq!(indexing_from(&ingest(3, &[]), true), Indexing::Paused);
    }

    fn task(state: AgentTaskState) -> AgentTaskSummary {
        AgentTaskSummary {
            task_id: "t".into(),
            goal: "g".into(),
            state,
            outcome: None,
            detail: None,
            started_ms: 0,
            finished_ms: None,
            steps: 0,
            verification: Default::default(),
        }
    }

    #[test]
    fn agents_line_prefers_waiting_over_running() {
        use AgentTaskState as S;
        assert_eq!(agents_from(&[]), Agents::Idle);
        assert_eq!(
            agents_from(&[task(S::Finished), task(S::Running)]),
            Agents::Running
        );
        assert_eq!(
            agents_from(&[task(S::Running), task(S::NeedsInput)]),
            Agents::Waiting
        );
    }

    #[test]
    fn model_ready_needs_a_working_assigned_provider() {
        let status = |providers: serde_json::Value| -> StatusResponse {
            serde_json::from_value(json!({
                "protocol": 1, "version": "0", "uptime_secs": 0, "items": 0, "chunks": 0,
                "providers": providers,
            }))
            .expect("status")
        };
        let p = |tiers: serde_json::Value, error: Option<&str>| {
            json!({"id": "p", "kind": "openai_compatible", "model": "m", "locality": "on_device",
                   "tiers": tiers, "capabilities": null, "error": error})
        };
        assert!(!model_ready_from(&status(json!([p(json!([]), None)]))));
        assert!(!model_ready_from(&status(json!([p(
            json!(["synthesis"]),
            Some("bad url")
        )]))));
        assert!(model_ready_from(&status(json!([p(
            json!(["synthesis"]),
            None
        )]))));
    }

    #[test]
    fn only_the_first_line_shows_while_disconnected() {
        let mut s = Snapshot {
            connected: None,
            indexing: Some(Indexing::Busy),
            model_ready: Some(true),
            agents: Some(Agents::Running),
        };
        assert_eq!(
            line_keys(&s),
            [Some("shell.tray.checking"), None, None, None]
        );
        s.connected = Some(true);
        assert_eq!(
            line_keys(&s),
            [
                Some("shell.tray.running"),
                Some("shell.tray.indexing"),
                Some("shell.tray.modelReady"),
                Some("shell.tray.agentRunning")
            ]
        );
        s.agents = None;
        assert_eq!(line_keys(&s)[3], None, "unknown lines stay hidden");
    }

    #[test]
    fn every_tray_label_has_a_default() {
        let keys: Vec<&str> = LABELS.iter().map(|(k, _)| *k).collect();
        for s in [
            Snapshot {
                connected: Some(true),
                indexing: Some(Indexing::Paused),
                model_ready: Some(false),
                agents: Some(Agents::Waiting),
            },
            Snapshot::default(),
        ] {
            for k in line_keys(&s).into_iter().flatten() {
                assert!(keys.contains(&k), "{k}");
            }
        }
        for k in [pause_key(true), pause_key(false)] {
            assert!(keys.contains(&k), "{k}");
        }
    }

    #[test]
    fn line_ops_insert_in_order_and_never_remove_the_first_line() {
        let all = [Some("a"), Some("b"), Some("c"), Some("d")];
        let start = [true, false, false, false];
        assert_eq!(
            line_ops(&all, &start),
            [
                LineOp::Insert(1, 1),
                LineOp::Insert(2, 2),
                LineOp::Insert(3, 3)
            ]
        );
        // A gap: line 2 unknown while 3 shows; 3 goes right after 1.
        let gap = [Some("a"), Some("b"), None, Some("d")];
        assert_eq!(
            line_ops(&gap, &start),
            [LineOp::Insert(1, 1), LineOp::Insert(3, 2)]
        );
        assert_eq!(line_ops(&[None; 4], &start), [], "line 0 stays");
    }
}
