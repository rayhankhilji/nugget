//! OS notifications for the proactive layer (morning brief, due reminders), shown from Rust only
//! (`commands::notify`), and opening Today when the user clicks one.
//!
//! `tauri-plugin-notification` has no click callback on desktop, so a notification that should
//! open Today is shown with the platform library the plugin itself uses underneath:
//!
//! - **Linux** (`notify-rust`, D-Bus): a `default` action; a thread waits for it
//!   (`wait_for_action`; the server reports a click or the notification closing).
//! - **macOS** (`mac-notification-sys`): `wait_for_click` on a thread (the library resolves it on
//!   click, dismissal or auto-dismissal; callbacks arrive on the app's main run loop).
//! - **Windows** (`tauri-winrt-notification`): the toast's `Activated` handler.
//!
//! A click shows and focuses the main window and emits `nugget:navigate` with `"today"`
//! ([`crate::tray::show_main`]). When that path fails, the plugin shows the notification instead
//! and a fallback is armed: the next time the main window gains focus within [`FOCUS_WINDOW`]
//! counts as opening the notification ([`PendingToday`], once).

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, Runtime, Window, WindowEvent};

/// The page a clicked notification opens (a `PageId` in the frontend).
pub const TODAY_PAGE: &str = "today";
/// Fallback only: focusing the main window this soon after a notification opens Today.
pub const FOCUS_WINDOW: Duration = Duration::from_secs(10 * 60);
/// Clipped lengths (characters).
const MAX_TITLE_LEN: usize = 200;
const MAX_BODY_LEN: usize = 2_000;

/// A notification that opens Today is outstanding until the instant held (fallback path).
#[derive(Debug, Default)]
pub struct PendingToday(Mutex<Option<Instant>>);

impl PendingToday {
    /// A notification was shown at `now`.
    pub fn arm(&self, now: Instant) {
        *self.lock() = Some(now + FOCUS_WINDOW);
    }

    /// Whether the main window gaining focus at `now` should open Today. Consumes the pending
    /// notification either way.
    pub fn take(&self, now: Instant) -> bool {
        self.lock().take().is_some_and(|until| now <= until)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Instant>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Show a notification. With `open_today`, clicking it opens Today.
pub fn show<R: Runtime>(
    app: &AppHandle<R>,
    title: &str,
    body: &str,
    open_today: bool,
) -> Result<(), String> {
    let title: String = title.chars().take(MAX_TITLE_LEN).collect();
    let body: String = body.chars().take(MAX_BODY_LEN).collect();
    if open_today {
        match show_clickable(app, &title, &body) {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::warn!(error = %e, "notification: no click callback; opening Today on focus instead");
                if let Some(p) = app.try_state::<PendingToday>() {
                    p.arm(Instant::now());
                }
            }
        }
    }
    show_plain(app, title, body)
}

/// The plugin's notification (no click callback on desktop).
fn show_plain<R: Runtime>(app: &AppHandle<R>, title: String, body: String) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|e| format!("couldn't show the notification: {e}"))
}

/// The notification was clicked: show the window on Today.
fn clicked<R: Runtime>(app: &AppHandle<R>) {
    if let Some(p) = app.try_state::<PendingToday>() {
        p.take(Instant::now());
    }
    crate::tray::show_main(app, Some(TODAY_PAGE));
}

/// Main window events: the fallback for notifications shown without a click callback.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if window.label() != "main" || !matches!(event, WindowEvent::Focused(true)) {
        return;
    }
    let app = window.app_handle();
    if app
        .try_state::<PendingToday>()
        .is_some_and(|p| p.take(Instant::now()))
    {
        crate::tray::show_main(app, Some(TODAY_PAGE));
    }
}

#[cfg(target_os = "linux")]
fn show_clickable<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str) -> Result<(), String> {
    let mut n = notify_rust::Notification::new();
    n.summary(title)
        .body(body)
        .auto_icon()
        .action("default", "Open Today");
    let app = app.clone();
    // `show` talks to the session bus and `wait_for_action` blocks until the notification is
    // clicked or closed: both on a thread of their own.
    std::thread::Builder::new()
        .name("notification".into())
        .spawn(move || match n.show() {
            Ok(handle) => handle.wait_for_action(|action| {
                if action == "default" {
                    clicked(&app);
                }
            }),
            Err(e) => {
                tracing::warn!(error = %e, "notification: D-Bus failed; using the plugin");
                if let Some(p) = app.try_state::<PendingToday>() {
                    p.arm(Instant::now());
                }
                let (title, body) = (n.summary.clone(), n.body.clone());
                if let Err(e) = show_plain(&app, title, body) {
                    tracing::warn!(error = %e, "notification not shown");
                }
            }
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
fn show_clickable<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str) -> Result<(), String> {
    // As the plugin does: notifications come from the app bundle (Terminal's in `tauri dev`).
    // Set once per process; later calls report "already set", which is fine.
    let bundle = if tauri::is_dev() {
        "com.apple.Terminal".to_owned()
    } else {
        app.config().identifier.clone()
    };
    let _ = mac_notification_sys::set_application(&bundle);
    let (title, body) = (title.to_owned(), body.to_owned());
    let app = app.clone();
    std::thread::Builder::new()
        .name("notification".into())
        .spawn(move || {
            let mut n = mac_notification_sys::Notification::new();
            n.title(&title).message(&body).wait_for_click(true);
            match n.send() {
                Ok(mac_notification_sys::NotificationResponse::Click) => clicked(&app),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "notification not shown"),
            }
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(windows)]
fn show_clickable<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str) -> Result<(), String> {
    use std::path::Path;
    use tauri_winrt_notification::Toast;
    // As the plugin does: the app's own AppUserModelID once installed; a development build has
    // no registered one, so it borrows PowerShell's.
    let exe = tauri::utils::platform::current_exe().map_err(|e| e.to_string())?;
    let dev = exe.parent().is_some_and(|dir| {
        dir.ends_with(Path::new("target").join("debug"))
            || dir.ends_with(Path::new("target").join("release"))
    });
    let app_id = if dev {
        Toast::POWERSHELL_APP_ID.to_owned()
    } else {
        app.config().identifier.clone()
    };
    let app = app.clone();
    Toast::new(&app_id)
        .title(title)
        .text2(body)
        .on_activated(move |_action| {
            clicked(&app);
            Ok(())
        })
        .show()
        .map_err(|e| format!("{e:?}"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn show_clickable<R: Runtime>(_: &AppHandle<R>, _: &str, _: &str) -> Result<(), String> {
    Err("no click callback on this platform".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_soon_after_a_notification_opens_today_once() {
        let p = PendingToday::default();
        let t0 = Instant::now();
        assert!(!p.take(t0), "nothing pending");
        p.arm(t0);
        assert!(p.take(t0 + Duration::from_secs(30)));
        assert!(!p.take(t0 + Duration::from_secs(31)), "consumed");
        // Too late: consumed without opening.
        p.arm(t0);
        assert!(!p.take(t0 + FOCUS_WINDOW + Duration::from_secs(1)));
        assert!(!p.take(t0 + Duration::from_secs(1)));
        // A newer notification restarts the window.
        p.arm(t0);
        p.arm(t0 + FOCUS_WINDOW);
        assert!(p.take(t0 + FOCUS_WINDOW + Duration::from_secs(60)));
    }
}
