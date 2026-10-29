//! Tray / menu-bar icon: Open, Status, Updates, Quit, plus a live "running / not running" line.
//!
//! Per platform:
//! - macOS: a monochrome template image (`icons/tray-template.png`, the nugget silhouette in
//!   black + alpha, 44 px for Retina) that the menu bar tints for light and dark mode. Left click
//!   opens the window, right click the menu.
//! - Windows: the colour app icon; left click opens the window, right click the menu.
//! - Linux: the colour app icon through libayatana-appindicator (or libappindicator), loaded at
//!   runtime. AppIndicator reports no clicks, so everything (state line included) lives in the
//!   menu. The tray is skipped, with one log line, when no StatusNotifier host is on the session
//!   bus (e.g. GNOME without the AppIndicator extension), when the library is missing, or when
//!   `NUGGET_TRAY=off`; `NUGGET_TRAY=on` forces it (XEmbed-only trays).
//!
//! `NUGGET_TRAY=off` skips the tray on every platform (the installer smoke tests use it). Without
//! a tray, closing the window quits Nugget, and launching it again focuses the running window
//! (single instance).

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::daemon::event;

const TRAY_ID: &str = "nugget";
const UPDATES_LABEL: &str = "Updates…";
/// macOS menu-bar icon: black + alpha only, so the system can tint it.
#[cfg(any(target_os = "macos", test))]
const TEMPLATE_ICON: tauri::image::Image<'static> =
    tauri::include_image!("icons/tray-template.png");

/// Present in app state only when the tray was created; the window then hides on close
/// instead of quitting.
pub struct Tray<R: Runtime> {
    state_line: MenuItem<R>,
    update: MenuItem<R>,
}

/// Create the tray, or log once why there is none. The app works either way (see module docs).
pub fn init<R: Runtime>(app: &tauri::App<R>) {
    if let Some(why) = skip_reason() {
        tracing::info!(reason = %why, "no tray icon; closing the window quits Nugget");
        return;
    }
    // libappindicator-sys panics (rather than erroring) when neither library can be loaded; the
    // panic happens on this thread, in Rust code, before any tray exists.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build(app))) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "tray icon unavailable; closing the window quits Nugget")
        }
        Err(_) => tracing::warn!(
            "tray icon unavailable (no AppIndicator library); closing the window quits Nugget"
        ),
    }
}

fn build<R: Runtime>(app: &tauri::App<R>) -> tauri::Result<()> {
    let state_line = MenuItem::with_id(app, "state", "Nugget: checking…", false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open Nugget", true, None::<&str>)?;
    let status = MenuItem::with_id(app, "status", "Status", true, None::<&str>)?;
    let update = MenuItem::with_id(app, "update", UPDATES_LABEL, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Nugget", true, None::<&str>)?;
    let agents = crate::agent::tray_item(app)?;
    let menu = Menu::with_items(
        app,
        &[
            &state_line,
            &PredefinedMenuItem::separator(app)?,
            &open,
            &status,
            &update,
            &PredefinedMenuItem::separator(app)?,
            &agents,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Nugget")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id().as_ref() {
            "open" => show_main(app, None),
            // The update settings panel lives on the Status page.
            "status" | "update" => show_main(app, Some("status")),
            "quit" => app.exit(0),
            id if id == crate::agent::tray_item_id() => crate::agent::tray_clicked(app),
            _ => {}
        })
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
    app.manage(Tray { state_line, update });
    Ok(())
}

pub fn has_tray<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.try_state::<Tray<R>>().is_some()
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
pub fn set_connected<R: Runtime>(app: &AppHandle<R>, connected: bool) {
    let text = if connected {
        "Nugget is running"
    } else {
        "Nugget isn't running"
    };
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(text));
    }
    if let Some(t) = app.try_state::<Tray<R>>() {
        let _ = t.state_line.set_text(text);
    }
}

/// Show "Update available: vX" in the menu (it opens the panel with the release notes), or the
/// plain "Updates…" item.
pub fn set_update<R: Runtime>(app: &AppHandle<R>, version: Option<&str>) {
    if let Some(t) = app.try_state::<Tray<R>>() {
        let text = version.map_or_else(
            || UPDATES_LABEL.to_owned(),
            |v| format!("Update available: {v}…"),
        );
        let _ = t.update.set_text(text);
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

    #[test]
    fn template_icon_is_black_and_alpha() {
        let icon = TEMPLATE_ICON;
        assert_eq!((icon.width(), icon.height()), (44, 44));
        let px = icon.rgba().as_chunks::<4>().0.iter();
        assert!(px.clone().all(|p| p[..3] == [0, 0, 0]));
        assert!(px.clone().any(|p| p[3] == 255) && px.clone().any(|p| p[3] == 0));
    }
}
