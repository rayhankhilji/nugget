//! Quick ask: a small, Spotlight-style window that opens on a global shortcut, takes a question
//! and streams a cited answer from the user's own data.
//!
//! The window (`quickask.html`) answers with the existing `retrieve_ask` command and its
//! `nugget:chat-delta` / `nugget:chat-done` events, and opens a cited source with `open_url`.
//! This module owns the window, its settings file (`quickask.json` in the app config directory)
//! and the shortcut listener.
//!
//! On a Wayland session no listener is started: `nugget_hotkey::start` would register a
//! GlobalShortcuts portal shortcut under the Double Tap id and clash with it. The settings page
//! explains the fallback instead, a compositor key bound to `nugget-desktop --quick-ask`, which
//! the single-instance handler turns into [`toggle`].

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use nugget_hotkey::{Chord, Platform, Session, Trigger, parse_chord_for};
use serde::{Deserialize, Serialize};
use tauri::{
    App, AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, Window, WindowEvent,
};

use crate::daemon::CmdError;

/// Passed by a compositor key binding: toggle the quick-ask window in the running Nugget.
pub const QUICK_ASK_ARG: &str = "--quick-ask";
/// The window's label.
pub const LABEL: &str = "quickask";
/// The event carrying text to put in the question field (never submitted by itself).
pub const PREFILL_EVENT: &str = "quickask:prefill";
/// The window's size in logical pixels.
pub const SIZE: (f64, f64) = (640.0, 420.0);
/// The longest prefill, in characters.
pub const PREFILL_MAX_CHARS: usize = 500;
/// Pages the window may open in the main window.
const MAIN_PAGES: [&str; 2] = ["chat", "search"];
/// Focus lost this soon after showing is the show itself settling (some window managers hand
/// focus around once), not the user leaving.
const FOCUS_GRACE: Duration = Duration::from_millis(300);

/// Shortcuts that belong to something else: Double Tap's Wayland fallback and the kill switch.
const RESERVED: [&str; 2] = ["Ctrl+Alt+Space", "Ctrl+Alt+Shift+Escape"];

// --- settings ------------------------------------------------------------------------------------

/// `quickask.json`. Missing fields take their defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct QuickAskSettings {
    pub enabled: bool,
    /// An accelerator such as `Ctrl+Shift+Space` (see `nugget_hotkey::parse_chord`).
    pub shortcut: String,
}

impl Default for QuickAskSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            shortcut: default_shortcut_for(Platform::current()).into(),
        }
    }
}

/// The default shortcut: Shift+Command+Space on macOS (written in Apple's modifier order, as
/// `Chord::accelerator_for` gives it back), Ctrl+Shift+Space elsewhere.
pub fn default_shortcut_for(platform: Platform) -> &'static str {
    match platform {
        Platform::MacOs => "Shift+Cmd+Space",
        Platform::Windows | Platform::Linux => "Ctrl+Shift+Space",
    }
}

/// Parse `shortcut` for `platform` and refuse the chords other features use. Returns the chord.
pub fn parse_shortcut(shortcut: &str, platform: Platform) -> Result<Chord, String> {
    let text = shortcut.trim();
    if text.is_empty() {
        return Err("the shortcut is empty".into());
    }
    let chord = parse_chord_for(text, platform).map_err(|e| e.to_string())?;
    for reserved in RESERVED {
        if parse_chord_for(reserved, platform).is_ok_and(|r| r == chord) {
            return Err(format!(
                "{} is already used by Nugget; pick another shortcut",
                chord.accelerator_for(platform)
            ));
        }
    }
    Ok(chord)
}

impl QuickAskSettings {
    /// The settings with the shortcut checked and written in the platform's conventional form.
    pub fn validated_for(self, platform: Platform) -> Result<Self, String> {
        let chord = parse_shortcut(&self.shortcut, platform)?;
        Ok(Self {
            enabled: self.enabled,
            shortcut: chord.accelerator_for(platform),
        })
    }
}

pub fn file_in(config_dir: &Path) -> PathBuf {
    config_dir.join("quickask.json")
}

/// Parse a settings file; garbage or an invalid shortcut gives the defaults.
pub fn parse_settings(bytes: &[u8], platform: Platform) -> QuickAskSettings {
    serde_json::from_slice::<QuickAskSettings>(bytes)
        .map_err(|e| e.to_string())
        .and_then(|s| s.validated_for(platform))
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "ignoring invalid quick-ask settings");
            QuickAskSettings::default()
        })
}

pub fn load(path: &Path) -> QuickAskSettings {
    match std::fs::read(path) {
        Ok(bytes) => parse_settings(&bytes, Platform::current()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => QuickAskSettings::default(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot read quick-ask settings");
            QuickAskSettings::default()
        }
    }
}

/// Write via a temporary file and rename.
pub fn save(path: &Path, settings: &QuickAskSettings) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

// --- prefill and placement -----------------------------------------------------------------------

/// Text for the question field: line breaks and tabs become spaces, other control characters and
/// bidirectional overrides are dropped, blanks are collapsed, and it is clipped to
/// [`PREFILL_MAX_CHARS`]. `None` when nothing is left.
pub fn sanitize_prefill(text: &str) -> Option<String> {
    let bidi = |c: char| matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
    let cleaned: String = text
        .chars()
        .filter_map(|c| {
            if c.is_whitespace() {
                Some(' ')
            } else if c.is_control() || bidi(c) {
                None
            } else {
                Some(c)
            }
        })
        .collect();
    let joined = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let clipped: String = joined.chars().take(PREFILL_MAX_CHARS).collect();
    let clipped = clipped.trim_end();
    (!clipped.is_empty()).then(|| clipped.to_string())
}

/// A rectangle in physical pixels: x, y, width, height.
type Rect = (i32, i32, u32, u32);

/// The top-left corner that centres a `size` (physical pixels) window in `work`, kept inside it.
pub fn center_in(work: Rect, size: (u32, u32)) -> (i32, i32) {
    let (x, y, w, h) = work;
    let dx = (i64::from(w) - i64::from(size.0)).max(0) / 2;
    let dy = (i64::from(h) - i64::from(size.1)).max(0) / 2;
    let clamp = |v: i64| v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    (clamp(i64::from(x) + dx), clamp(i64::from(y) + dy))
}

/// Whether `point` lies inside `r`.
fn contains(r: Rect, point: (f64, f64)) -> bool {
    let (x, y, w, h) = r;
    point.0 >= f64::from(x)
        && point.1 >= f64::from(y)
        && point.0 < f64::from(x) + f64::from(w)
        && point.1 < f64::from(y) + f64::from(h)
}

// --- state ---------------------------------------------------------------------------------------

/// Managed state.
pub struct QuickAsk {
    settings_path: Option<PathBuf>,
    settings: Mutex<QuickAskSettings>,
    listener: Mutex<Option<Box<dyn nugget_hotkey::Listener>>>,
    error: Mutex<Option<String>>,
    shown_at: Mutex<Option<Instant>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Where the shortcut stands, for the settings page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuickAskStatus {
    pub listening: bool,
    pub error: Option<String>,
    /// Wayland: no listener runs; bind a compositor key to `nugget-desktop --quick-ask`.
    pub wayland_fallback: bool,
    /// `listening`, `off`, `error` or `unsupported_wayland`.
    pub state: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuickAskView {
    pub settings: QuickAskSettings,
    pub status: QuickAskStatus,
}

fn is_wayland() -> bool {
    nugget_hotkey::session() == Session::Wayland
}

fn status_of(listening: bool, error: Option<String>, wayland: bool) -> QuickAskStatus {
    let state = if wayland {
        "unsupported_wayland"
    } else if listening {
        "listening"
    } else if error.is_some() {
        "error"
    } else {
        // Turned off, or not started yet (the listener starts off the main thread).
        "off"
    };
    QuickAskStatus {
        listening,
        error,
        wayland_fallback: wayland,
        state,
    }
}

fn view(app: &AppHandle) -> QuickAskView {
    let state = app.state::<QuickAsk>();
    let settings = lock(&state.settings).clone();
    let listening = lock(&state.listener).is_some();
    let error = lock(&state.error).clone();
    QuickAskView {
        status: status_of(listening, error, is_wayland()),
        settings,
    }
}

// --- lifecycle -----------------------------------------------------------------------------------

/// Call from `tauri::Builder::setup` (the main thread): load the settings, create the hidden
/// window and start the shortcut listener.
pub fn setup(app: &App) {
    let handle = app.handle();
    let settings_path = handle
        .path()
        .app_config_dir()
        .map(|d| file_in(&d))
        .map_err(
            |e| tracing::warn!(error = %e, "no config directory; quick-ask settings won't persist"),
        )
        .ok();
    let loaded = settings_path.as_deref().map(load).unwrap_or_default();
    app.manage(QuickAsk {
        settings_path,
        settings: Mutex::new(loaded),
        listener: Mutex::new(None),
        error: Mutex::new(None),
        shown_at: Mutex::new(None),
    });
    if let Err(e) = create_window(app) {
        tracing::warn!(error = %e, "cannot create the quick-ask window");
    }
    // macOS: install on the main thread (we are on it). Elsewhere off it.
    #[cfg(target_os = "macos")]
    restart_now(handle);
    #[cfg(not(target_os = "macos"))]
    restart_listener(handle);
}

fn create_window(app: &App) -> tauri::Result<()> {
    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("quickask.html".into()))
        .title("Nugget Quick Ask")
        .inner_size(SIZE.0, SIZE.1)
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .center()
        .build()?;
    Ok(())
}

/// Call from `on_window_event`: closing the window hides it, and so does leaving it.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if window.label() != LABEL {
        return;
    }
    match event {
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            let _ = window.hide();
        }
        WindowEvent::Focused(false) => {
            let settling = window
                .try_state::<QuickAsk>()
                .and_then(|s| *lock(&s.shown_at))
                .is_some_and(|t| t.elapsed() < FOCUS_GRACE);
            if !settling {
                let _ = window.hide();
            }
        }
        _ => {}
    }
}

/// Re-register the shortcut from the current settings, on the right thread (after a settings
/// change, or after sleep and resume). Returns at once.
pub fn restart_listener(app: &AppHandle) {
    let h = app.clone();
    #[cfg(target_os = "macos")]
    if let Err(e) = app.run_on_main_thread(move || restart_now(&h)) {
        tracing::warn!(error = %e, "cannot reach the main thread");
    }
    #[cfg(not(target_os = "macos"))]
    std::thread::spawn(move || restart_now(&h));
}

/// [`restart_listener`], waiting until it is done.
async fn restart_listener_async(app: &AppHandle) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let h = app.clone();
    #[cfg(target_os = "macos")]
    if let Err(e) = app.run_on_main_thread(move || {
        restart_now(&h);
        let _ = tx.send(());
    }) {
        tracing::warn!(error = %e, "cannot reach the main thread");
        return;
    }
    #[cfg(not(target_os = "macos"))]
    tauri::async_runtime::spawn_blocking(move || {
        restart_now(&h);
        let _ = tx.send(());
    });
    let _ = rx.await;
}

fn restart_now(app: &AppHandle) {
    let Some(state) = app.try_state::<QuickAsk>() else {
        return;
    };
    let mut slot = lock(&state.listener);
    // Drop first, so the chord is released before it is registered again.
    *slot = None;
    let s = lock(&state.settings).clone();
    let mut error = None;
    if s.enabled && !is_wayland() {
        let trigger = Trigger::Chord(s.shortcut.clone());
        let handle = app.clone();
        let on_fire = Box::new(move || {
            // The hotkey dispatcher thread: window work goes to the main thread.
            let h = handle.clone();
            if let Err(e) = handle.run_on_main_thread(move || toggle(&h)) {
                tracing::debug!(error = %e, "cannot reach the main thread");
            }
        });
        match nugget_hotkey::start(trigger, on_fire) {
            Ok(l) => *slot = Some(l),
            Err(e) => {
                tracing::warn!(error = %e, "quick-ask shortcut unavailable");
                error = Some(e.to_string());
            }
        }
    }
    *lock(&state.error) = error;
}

// --- showing -------------------------------------------------------------------------------------

fn window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(LABEL)
}

/// Hide the window if it is showing, show it otherwise.
pub fn toggle(app: &AppHandle) {
    let Some(w) = window(app) else {
        return;
    };
    if w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(true) {
        let _ = w.hide();
    } else {
        show_with(app, None);
    }
}

/// Hide the window.
pub fn hide(app: &AppHandle) {
    if let Some(w) = window(app) {
        let _ = w.hide();
    }
}

/// Show and focus the window, centred on the monitor under the pointer. `prefill` (sanitised,
/// see [`sanitize_prefill`]) goes into the question field and is not submitted.
pub fn show_with(app: &AppHandle, prefill: Option<String>) {
    let Some(w) = window(app) else {
        return;
    };
    if !w.is_visible().unwrap_or(false) {
        place(app, &w);
    }
    if let Some(state) = app.try_state::<QuickAsk>() {
        *lock(&state.shown_at) = Some(Instant::now());
    }
    let _ = w.unminimize();
    let _ = w.show();
    let _ = w.set_focus();
    if let Some(text) = prefill.as_deref().and_then(sanitize_prefill)
        && let Err(e) = app.emit_to(LABEL, PREFILL_EVENT, text)
    {
        tracing::debug!(error = %e, "cannot prefill the quick-ask window");
    }
}

fn place(app: &AppHandle, w: &WebviewWindow) {
    let monitors = w.available_monitors().unwrap_or_default();
    let cursor = app.cursor_position().ok().map(|p| (p.x, p.y));
    let rect = |m: &tauri::Monitor| -> Rect {
        let (p, s) = (m.position(), m.size());
        (p.x, p.y, s.width, s.height)
    };
    let monitor = cursor
        .and_then(|c| monitors.iter().find(|m| contains(rect(m), c)))
        .or_else(|| monitors.first());
    let Some(m) = monitor else {
        let _ = w.center();
        return;
    };
    let wa = m.work_area();
    let work = (wa.position.x, wa.position.y, wa.size.width, wa.size.height);
    let scale = m.scale_factor();
    let size = (
        (SIZE.0 * scale).round() as u32,
        (SIZE.1 * scale).round() as u32,
    );
    let (x, y) = center_in(work, size);
    let _ = w.set_position(PhysicalPosition::new(x, y));
}

// --- commands ------------------------------------------------------------------------------------

#[tauri::command]
pub fn quickask_settings_get(app: AppHandle) -> QuickAskView {
    view(&app)
}

/// Validate, save and re-register the shortcut. Not from the quick-ask window itself.
#[tauri::command]
pub async fn quickask_settings_set(
    app: AppHandle,
    window: WebviewWindow,
    settings: QuickAskSettings,
) -> Result<QuickAskView, CmdError> {
    if window.label() == LABEL {
        return Err(CmdError::Other(
            "not available in the quick-ask window".into(),
        ));
    }
    let settings = settings
        .validated_for(Platform::current())
        .map_err(CmdError::Other)?;
    if settings.enabled && !is_wayland() {
        nugget_hotkey::check(&Trigger::Chord(settings.shortcut.clone()))
            .map_err(|e| CmdError::Other(e.to_string()))?;
    }
    let state = app.state::<QuickAsk>();
    if let Some(path) = &state.settings_path {
        save(path, &settings)
            .map_err(|e| CmdError::Other(format!("cannot save the quick-ask settings: {e}")))?;
    }
    *lock(&state.settings) = settings;
    restart_listener_async(&app).await;
    Ok(view(&app))
}

#[tauri::command]
pub fn quickask_hide(app: AppHandle) {
    hide(&app);
}

/// Hide quick ask and show the main window on `page` (`chat` or `search`).
#[tauri::command]
pub fn quickask_open_main(app: AppHandle, page: String) -> Result<(), CmdError> {
    if !MAIN_PAGES.contains(&page.as_str()) {
        return Err(CmdError::Other(format!(
            "cannot open page {page:?} from quick ask"
        )));
    }
    hide(&app);
    crate::tray::show_main(&app, Some(&page));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let s = QuickAskSettings::default();
        assert!(s.enabled);
        assert_eq!(s.shortcut, default_shortcut_for(Platform::current()));
        assert_eq!(default_shortcut_for(Platform::MacOs), "Shift+Cmd+Space");
        assert_eq!(default_shortcut_for(Platform::Windows), "Ctrl+Shift+Space");
        assert_eq!(default_shortcut_for(Platform::Linux), "Ctrl+Shift+Space");
        for p in [Platform::MacOs, Platform::Windows, Platform::Linux] {
            let d = QuickAskSettings {
                enabled: true,
                shortcut: default_shortcut_for(p).into(),
            };
            assert_eq!(d.clone().validated_for(p).unwrap(), d, "{p:?}");
        }
    }

    #[test]
    fn settings_round_trip() {
        let dir = std::env::temp_dir().join(format!("nugget-quickask-{}", std::process::id()));
        let path = file_in(&dir);
        let s = QuickAskSettings {
            enabled: false,
            shortcut: "Ctrl+Alt+K".into(),
        };
        save(&path, &s).unwrap();
        assert_eq!(load(&path), s);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load(&path), QuickAskSettings::default());
    }

    #[test]
    fn garbage_tolerant() {
        let p = Platform::Linux;
        let d = QuickAskSettings::default();
        assert_eq!(parse_settings(b"not json", p), d);
        assert_eq!(parse_settings(b"[1,2]", p), d);
        assert_eq!(parse_settings(br#"{"enabled": "yes"}"#, p), d);
        assert_eq!(parse_settings(br#"{"shortcut": "Banana"}"#, p), d);
        // Missing fields take their defaults; unknown ones are ignored.
        let s = parse_settings(br#"{"enabled": false, "future": 1}"#, p);
        assert!(!s.enabled);
        assert_eq!(s.shortcut, d.shortcut);
        // The shortcut is written in the conventional form.
        let s = parse_settings(br#"{"shortcut": "shift+control+j"}"#, p);
        assert_eq!(s.shortcut, "Ctrl+Shift+J");
    }

    #[test]
    fn shortcut_validation() {
        let p = Platform::Linux;
        assert!(parse_shortcut("", p).is_err());
        assert!(parse_shortcut("   ", p).is_err());
        assert!(parse_shortcut("Space", p).is_err());
        assert!(parse_shortcut("Shift+A", p).is_err());
        assert!(parse_shortcut("Ctrl+Shift", p).is_err());
        assert!(parse_shortcut("Ctrl+Nope", p).is_err());
        // Taken by Double Tap's Wayland fallback and the kill switch, in any spelling.
        assert!(parse_shortcut("Ctrl+Alt+Space", p).is_err());
        assert!(parse_shortcut("alt+ctrl+space", p).is_err());
        assert!(parse_shortcut("Ctrl+Alt+Shift+Escape", p).is_err());
        assert!(parse_shortcut("Ctrl+Shift+Space", p).is_ok());
        assert!(parse_shortcut("F13", p).is_ok());
        let mac = parse_shortcut("Cmd+Shift+Space", Platform::MacOs).unwrap();
        assert_eq!(mac.accelerator_for(Platform::MacOs), "Shift+Cmd+Space");
    }

    #[test]
    fn prefill_is_sanitised() {
        assert_eq!(sanitize_prefill(""), None);
        assert_eq!(sanitize_prefill(" \n\t "), None);
        assert_eq!(sanitize_prefill("\u{0}\u{7}"), None);
        assert_eq!(
            sanitize_prefill("  what did\nAda\tsay?\u{0}\u{1b}[31m "),
            Some("what did Ada say?[31m".into())
        );
        assert_eq!(
            sanitize_prefill("abc\u{202E}def\u{2066}"),
            Some("abcdef".into())
        );
        let long = "é".repeat(PREFILL_MAX_CHARS + 50);
        let out = sanitize_prefill(&long).unwrap();
        assert_eq!(out.chars().count(), PREFILL_MAX_CHARS);
        assert_eq!(
            sanitize_prefill("日本語の質問"),
            Some("日本語の質問".into())
        );
    }

    #[test]
    fn centring() {
        assert_eq!(center_in((0, 0, 1920, 1080), (640, 420)), (640, 330));
        assert_eq!(center_in((1920, 25, 1280, 775), (1280, 840)), (1920, 25));
        assert_eq!(center_in((-1280, 0, 1280, 1024), (640, 420)), (-960, 302));
        assert!(contains((0, 0, 10, 10), (5.0, 5.0)));
        assert!(!contains((0, 0, 10, 10), (10.0, 5.0)));
    }

    #[test]
    fn status_states() {
        assert_eq!(status_of(true, None, false).state, "listening");
        assert_eq!(status_of(false, Some("x".into()), false).state, "error");
        assert_eq!(status_of(false, None, false).state, "off");
        let w = status_of(false, None, true);
        assert_eq!(w.state, "unsupported_wayland");
        assert!(w.wayland_fallback && !w.listening);
    }
}
