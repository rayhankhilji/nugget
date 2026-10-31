//! Double Tap on the desktop: the global trigger, the accessibility read, the caret overlay, the
//! insertion with read-back verification, and the feedback to the daemon.
//!
//! - [`settings`]: the preferences file and app matching (providers, excluded apps, terminals).
//! - [`overlay`]: the overlay as a pure state machine; [`placement`]: where its window goes.
//! - [`errors`]: daemon and accessibility errors to i18n keys.
//! - [`permissions`]: per-OS onboarding and the status the settings page shows.
//! - [`flow`]: the runtime glue (listener lifecycle, the draft flow, effects, focus restore).
//! - [`readahead`]: read-ahead, prefilling the local model's cache while a reply field keeps
//!   focus (with its privacy rules); [`focus`]: the OS focus change notifications it sleeps on;
//!   [`power`]: battery and low power mode, which pause it.
//! - [`history`]: the last drafts, in memory only, to insert one again.
//! - [`practice`]: the Double Tap page's practice field, a sandbox target for the real trigger.
//! - [`commands`]: the Tauri commands.
//! - `probe`: debug builds only, a step log for the native overlay runs (`apps/desktop/e2e-native`).

pub mod commands;
pub mod errors;
pub mod flow;
pub mod focus;
pub mod history;
pub mod overlay;
pub mod permissions;
pub mod placement;
pub mod power;
pub mod practice;
mod probe;
pub mod readahead;
pub mod settings;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use tauri::{App, Manager, WebviewUrl, WebviewWindowBuilder, Window, WindowEvent};

use flow::{A11y, Layout, Target};
use nugget_core::ipc::DraftRequest;
use overlay::Machine;
use settings::DoubleTapSettings;

/// The overlay window's label.
pub const OVERLAY_LABEL: &str = "doubletap-overlay";
/// The event carrying an `OverlayView` to the overlay window.
pub const OVERLAY_EVENT: &str = "doubletap:overlay";
/// The event carrying the saved `DoubleTapSettings` to every window after a change, so the
/// settings page and onboarding show what the running trigger is.
pub const SETTINGS_EVENT: &str = "doubletap:settings";
pub use nugget_core::ipc::method::DRAFT_PREWARM;
/// The overlay's size in logical pixels: its width and the height it starts at before its
/// content is measured.
pub const OVERLAY_SIZE: (f64, f64) = (440.0, 200.0);
/// The overlay's height limits in logical pixels: it resizes to fit its content within them
/// (and within the room beside the caret), and scrolls past the maximum.
pub const OVERLAY_MIN_HEIGHT: f64 = 120.0;
pub const OVERLAY_MAX_HEIGHT: f64 = 420.0;
/// The event carrying the draft history (newest first) to every window after a change.
pub const HISTORY_EVENT: &str = "doubletap:history";
/// The event carrying a `practice::Action` to the main window.
pub const PRACTICE_EVENT: &str = "doubletap:practice";
/// The main window's label (the practice field lives there).
pub const MAIN_LABEL: &str = "main";

/// Managed state.
pub struct DoubleTap {
    pub(crate) settings_path: Option<PathBuf>,
    pub(crate) settings: Mutex<DoubleTapSettings>,
    pub(crate) listener: Mutex<Option<Box<dyn nugget_hotkey::Listener>>>,
    pub(crate) trigger_error: Mutex<Option<String>>,
    pub(crate) machine: Mutex<Machine>,
    pub(crate) target: Mutex<Option<Target>>,
    /// The request of the session's last draft, for Retry and the refinements.
    pub(crate) request: Mutex<Option<DraftRequest>>,
    /// The practice field while it has focus.
    pub(crate) practice: Mutex<Option<practice::PracticeField>>,
    pub(crate) history: Mutex<history::History>,
    pub(crate) layout: Mutex<Layout>,
    pub(crate) a11y: Arc<A11y>,
    pub(crate) read_ahead: readahead::Runner,
}

/// Lock, recovering from a poisoned mutex (the data is plain state).
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Call from `tauri::Builder::setup` (the main thread): load the settings, create the hidden
/// overlay window, follow draft streams and start the trigger listener.
pub fn setup(app: &App) {
    let handle = app.handle();
    let settings_path = handle
        .path()
        .app_config_dir()
        .map(|d| settings::file_in(&d))
        .map_err(|e| tracing::warn!(error = %e, "no config directory; Double Tap settings won't persist"))
        .ok();
    let loaded = settings_path
        .as_deref()
        .map(settings::load)
        .unwrap_or_default();
    app.manage(DoubleTap {
        settings_path,
        settings: Mutex::new(loaded),
        listener: Mutex::new(None),
        trigger_error: Mutex::new(None),
        machine: Mutex::new(Machine::new()),
        target: Mutex::new(None),
        request: Mutex::new(None),
        practice: Mutex::new(None),
        history: Mutex::new(history::History::default()),
        layout: Mutex::new(Layout::default()),
        a11y: Arc::new(A11y::new()),
        read_ahead: readahead::Runner::default(),
    });
    if let Err(e) = create_overlay(app) {
        tracing::warn!(error = %e, "cannot create the Double Tap overlay window");
    }
    flow::follow_streams(handle);
    readahead::restart(handle);
    // macOS: install on the main thread so errors are reported synchronously. Elsewhere start
    // off the main thread (the Wayland portal can take seconds to answer).
    #[cfg(target_os = "macos")]
    flow::restart_listener(handle);
    #[cfg(not(target_os = "macos"))]
    {
        let h = handle.clone();
        std::thread::spawn(move || flow::restart_listener(&h));
    }
}

/// The overlay: frameless, always on top, out of the taskbar, hidden and unfocusable until a
/// draft starts (see `flow::show_overlay`).
fn create_overlay(app: &App) -> tauri::Result<()> {
    WebviewWindowBuilder::new(app, OVERLAY_LABEL, WebviewUrl::App("overlay.html".into()))
        .title("Nugget Double Tap")
        .inner_size(OVERLAY_SIZE.0, OVERLAY_SIZE.1)
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .visible(false)
        .focused(false)
        .focusable(false)
        .build()?;
    Ok(())
}

/// Call from `on_window_event`: closing the overlay (Alt+F4, Cmd+W) cancels instead of
/// destroying the window; the main window losing focus forgets the practice field.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    // The practice field only counts while the main window has the keyboard (the page clears it
    // on blur too; this covers a page that could not).
    if let WindowEvent::Focused(false) = event
        && window.label() == MAIN_LABEL
        && let Some(state) = window.app_handle().try_state::<DoubleTap>()
    {
        *lock(&state.practice) = None;
    }
    if let WindowEvent::CloseRequested { api, .. } = event
        && window.label() == OVERLAY_LABEL
    {
        api.prevent_close();
        flow::apply(window.app_handle(), overlay::Event::Cancel);
        let _ = window.hide();
    }
}
