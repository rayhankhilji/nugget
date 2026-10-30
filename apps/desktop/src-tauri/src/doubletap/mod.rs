//! Double Tap on the desktop: the global trigger, the accessibility read, the caret overlay, the
//! insertion with read-back verification, and the feedback to the daemon.
//!
//! - [`settings`]: the preferences file and app matching (providers, excluded apps, terminals).
//! - [`overlay`]: the overlay as a pure state machine; [`placement`]: where its window goes.
//! - [`errors`]: daemon and accessibility errors to i18n keys.
//! - [`permissions`]: per-OS onboarding and the status the settings page shows.
//! - [`flow`]: the runtime glue (listener lifecycle, the draft flow, effects, focus restore).
//! - [`readahead`]: read-ahead, prefilling the local model's cache while a reply field keeps
//!   focus (with its privacy rules).
//! - [`commands`]: the Tauri commands.
//! - `probe`: debug builds only, a step log for the native overlay runs (`apps/desktop/e2e-native`).

pub mod commands;
pub mod errors;
pub mod flow;
pub mod overlay;
pub mod permissions;
pub mod placement;
mod probe;
pub mod readahead;
pub mod settings;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use tauri::{App, Manager, WebviewUrl, WebviewWindowBuilder, Window, WindowEvent};

use flow::{A11y, Target};
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
/// The overlay's size in logical pixels.
pub const OVERLAY_SIZE: (f64, f64) = (440.0, 200.0);

/// Managed state.
pub struct DoubleTap {
    pub(crate) settings_path: Option<PathBuf>,
    pub(crate) settings: Mutex<DoubleTapSettings>,
    pub(crate) listener: Mutex<Option<Box<dyn nugget_hotkey::Listener>>>,
    pub(crate) trigger_error: Mutex<Option<String>>,
    pub(crate) machine: Mutex<Machine>,
    pub(crate) target: Mutex<Option<Target>>,
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
/// destroying the window.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event
        && window.label() == OVERLAY_LABEL
    {
        api.prevent_close();
        flow::apply(window.app_handle(), overlay::Event::Cancel);
        let _ = window.hide();
    }
}
