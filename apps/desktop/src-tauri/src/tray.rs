//! Tray / menu-bar icon: Open, Status, Quit, plus a live "running / not running" line.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::daemon::event;

const TRAY_ID: &str = "nugget";

/// Present in app state only when the tray was created; the window then hides on close
/// instead of quitting.
pub struct Tray<R: Runtime> {
    state_line: MenuItem<R>,
}

pub fn build<R: Runtime>(app: &tauri::App<R>) -> tauri::Result<()> {
    let state_line = MenuItem::with_id(app, "state", "Nugget: checking…", false, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open Nugget", true, None::<&str>)?;
    let status = MenuItem::with_id(app, "status", "Status", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Nugget", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &state_line,
            &PredefinedMenuItem::separator(app)?,
            &open,
            &status,
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
            "status" => show_main(app, Some("status")),
            "quit" => app.exit(0),
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
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    app.manage(Tray { state_line });
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
