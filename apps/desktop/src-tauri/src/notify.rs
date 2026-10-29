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
pub fn on_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
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
            Ok(handle) => handle.wait_for_action(|action| on_action(&app, action)),
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

/// The Linux notification's action callback: the `default` action is a click on the notification
/// (closing it reports `__closed`).
#[cfg(target_os = "linux")]
fn on_action<R: Runtime>(app: &AppHandle<R>, action: &str) {
    if action == "default" {
        clicked(app);
    }
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
                Ok(response) => on_response(&app, &response),
                Err(e) => tracing::warn!(error = %e, "notification not shown"),
            }
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The macOS notification's outcome: a click on its contents opens Today, a dismissal does not.
#[cfg(target_os = "macos")]
fn on_response<R: Runtime>(
    app: &AppHandle<R>,
    response: &mac_notification_sys::NotificationResponse,
) {
    if matches!(response, mac_notification_sys::NotificationResponse::Click) {
        clicked(app);
    }
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
        .on_activated(move |action| {
            on_activated(&app, action.as_deref());
            Ok(())
        })
        .show()
        .map_err(|e| format!("{e:?}"))
}

/// The toast's `Activated` handler: the toast has no buttons, so any activation is a click on it.
#[cfg(windows)]
fn on_activated<R: Runtime>(app: &AppHandle<R>, _action: Option<&str>) {
    clicked(app);
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn show_clickable<R: Runtime>(_: &AppHandle<R>, _: &str, _: &str) -> Result<(), String> {
    Err("no click callback on this platform".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use tauri::test::{MockRuntime, mock_builder, mock_context, noop_assets};
    use tauri::{App, Listener, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

    /// `nugget:navigate` with Today, as the main window receives it.
    const TODAY_JSON: &str = "\"today\"";

    /// A mock app shaped like the real one: the shipped identifier (the Windows toast's
    /// AppUserModelID outside a dev build), [`PendingToday`] and a `main` window. The receiver gets
    /// every `nugget:navigate` payload delivered to `main`.
    fn app() -> (
        App<MockRuntime>,
        WebviewWindow<MockRuntime>,
        mpsc::Receiver<String>,
    ) {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        let mut ctx = mock_context(noop_assets());
        ctx.config_mut().identifier = conf["identifier"].as_str().expect("identifier").to_owned();
        let app = mock_builder()
            .manage(PendingToday::default())
            .build(ctx)
            .expect("mock app");
        let main = WebviewWindowBuilder::new(&app, "main", WebviewUrl::default())
            .build()
            .expect("main window");
        let (tx, rx) = mpsc::channel();
        main.listen(crate::daemon::event::NAVIGATE, move |e| {
            let _ = tx.send(e.payload().to_owned());
        });
        (app, main, rx)
    }

    fn navigations(rx: &mpsc::Receiver<String>) -> Vec<String> {
        rx.try_iter().collect()
    }

    #[test]
    fn a_click_opens_today_on_the_main_window() {
        let (app, _main, rx) = app();
        let pending = app.state::<PendingToday>();
        pending.arm(Instant::now());
        clicked(app.handle());
        assert_eq!(navigations(&rx), [TODAY_JSON]);
        assert!(
            !pending.take(Instant::now()),
            "a click consumes the focus fallback"
        );
    }

    #[test]
    fn focusing_main_after_a_notification_without_click_callback_opens_today_once() {
        let (app, main, rx) = app();
        let window = main.as_ref().window();
        window_focus(&window, false);
        window_focus(&window, true);
        assert_eq!(navigations(&rx), Vec::<String>::new(), "nothing pending");
        app.state::<PendingToday>().arm(Instant::now());
        window_focus(&window, false);
        window_focus(&window, true);
        window_focus(&window, true);
        assert_eq!(navigations(&rx), [TODAY_JSON], "once");
    }

    fn window_focus(window: &Window<MockRuntime>, focused: bool) {
        on_window_event(window, &WindowEvent::Focused(focused));
    }

    /// The function `notify-rust`'s `wait_for_action` calls.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_default_action_is_a_click() {
        let (app, _main, rx) = app();
        on_action(app.handle(), "__closed");
        assert_eq!(navigations(&rx), Vec::<String>::new());
        on_action(app.handle(), "default");
        assert_eq!(navigations(&rx), [TODAY_JSON]);
    }

    /// The function the `mac-notification-sys` thread calls with the notification's outcome
    /// (the delegate's `didActivateNotification:` / dismissal on the app's main run loop).
    #[cfg(target_os = "macos")]
    #[test]
    fn a_click_on_the_contents_is_a_click_and_a_dismissal_is_not() {
        use mac_notification_sys::NotificationResponse as N;
        let (app, _main, rx) = app();
        for r in [N::None, N::CloseButton("Close".into())] {
            on_response(app.handle(), &r);
        }
        assert_eq!(navigations(&rx), Vec::<String>::new());
        on_response(app.handle(), &N::Click);
        assert_eq!(navigations(&rx), [TODAY_JSON]);
    }

    /// The toast's `Activated` handler.
    #[cfg(windows)]
    #[test]
    fn an_activation_is_a_click() {
        let (app, _main, rx) = app();
        on_activated(app.handle(), None);
        assert_eq!(navigations(&rx), [TODAY_JSON]);
    }

    /// Real OS (run by `.github/workflows/real-os.yml`, job `notify-*`): the title the workflow
    /// looks for on screen.
    #[cfg(any(windows, target_os = "macos"))]
    const REAL_TITLE: &str = "Nugget notification click test";

    /// Real OS: the ignored tests below run only with this set (`1`); otherwise they say so and
    /// return.
    #[cfg(any(windows, target_os = "macos"))]
    fn real_enabled(test: &str) -> bool {
        let on = std::env::var("NUGGET_REAL_NOTIFY").is_ok_and(|v| v == "1");
        if !on {
            eprintln!("SKIPPED {test}: set NUGGET_REAL_NOTIFY=1 (posts a real notification)");
        }
        on
    }

    /// Posts through [`show`] exactly as `commands::notify` does and asserts the platform path
    /// with a click callback was taken (a failure arms the focus fallback instead).
    #[cfg(any(windows, target_os = "macos"))]
    fn post_real(app: &App<MockRuntime>) {
        show(
            app.handle(),
            REAL_TITLE,
            "Click me: this opens Today.",
            true,
        )
        .expect("notification shown");
        assert!(
            !app.state::<PendingToday>().take(Instant::now()),
            "no click callback: the notification was shown without one (focus fallback armed)"
        );
        eprintln!("NUGGET_NOTIFY_POSTED {REAL_TITLE}");
    }

    /// Windows: a real toast (AppUserModelID = the identifier, registered by the workflow as the
    /// installer's shortcut does), clicked on screen by the workflow through UI Automation; the
    /// toast's `Activated` handler must open Today.
    #[cfg(windows)]
    #[test]
    #[ignore = "posts a real toast and waits for real-os.yml to click it (NUGGET_REAL_NOTIFY=1)"]
    fn real_toast_click_opens_today() {
        if !real_enabled("real_toast_click_opens_today") {
            return;
        }
        let (app, _main, rx) = app();
        post_real(&app);
        let secs = std::env::var("NUGGET_NOTIFY_CLICK_TIMEOUT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120);
        match rx.recv_timeout(Duration::from_secs(secs)) {
            Ok(page) => assert_eq!(page, TODAY_JSON),
            Err(_) => panic!("the toast was not clicked (no Activated callback) within {secs}s"),
        }
        eprintln!("NUGGET_NOTIFY_CLICKED");
    }

    /// macOS: a real notification through the product path. Its click can't be exercised here:
    /// the click reaches `mac-notification-sys` through the notification-center delegate on the
    /// main run loop, which the test harness never runs (tests run on worker threads), so a
    /// click could not be delivered even if the runner could click Notification Center.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "posts a real notification (NUGGET_REAL_NOTIFY=1, real-os.yml)"]
    fn real_notification_posts_with_a_click_callback() {
        if !real_enabled("real_notification_posts_with_a_click_callback") {
            return;
        }
        let (app, _main, _rx) = app();
        post_real(&app);
        // Long enough for the workflow to capture the banner.
        std::thread::sleep(Duration::from_secs(8));
    }

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
