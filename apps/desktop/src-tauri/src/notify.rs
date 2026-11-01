//! OS notifications, shown from Rust only, and opening the right page when the user clicks one:
//! the proactive layer's morning brief and due reminders (`commands::notify`, opens Today) and an
//! agent waiting for a confirmation or an answer ([`follow_agent_events`], opens Agent).
//!
//! Every notification is gated by the user's choices in `platform.json` ([`allowed`]): off until
//! the user opts in, per kind, and never during quiet hours (dropped, not queued). Agent
//! notifications carry fixed, localized text only (never the goal, the action or screen text), and
//! are skipped while the main window has focus.
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
//! A click shows and focuses the main window and emits `nugget:navigate` with the page
//! ([`crate::tray::show_main`]). When that path fails, the plugin shows the notification instead
//! and a fallback is armed: the next time the main window gains focus within [`FOCUS_WINDOW`]
//! counts as opening the notification ([`PendingOpen`], once).

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Listener, Manager, Runtime, Window, WindowEvent};

use crate::platform::{Minute, NotifySettings, Platform};

/// The page a clicked proactive notification opens (a `PageId` in the frontend).
pub const TODAY_PAGE: &str = "today";
/// The page a clicked agent notification opens.
pub const AGENT_PAGE: &str = "agent";
/// One agent notification per task within this long.
const AGENT_REPEAT: Duration = Duration::from_secs(120);
/// Fallback only: focusing the main window this soon after a notification opens Today.
pub const FOCUS_WINDOW: Duration = Duration::from_secs(10 * 60);
/// Clipped lengths (characters).
const MAX_TITLE_LEN: usize = 200;
const MAX_BODY_LEN: usize = 2_000;

/// A notification that opens a page is outstanding until the instant held (fallback path).
#[derive(Debug, Default)]
pub struct PendingOpen(Mutex<Option<(Instant, &'static str)>>);

impl PendingOpen {
    /// A notification opening `page` was shown at `now`.
    pub fn arm(&self, now: Instant, page: &'static str) {
        *self.lock() = Some((now + FOCUS_WINDOW, page));
    }

    /// The page the main window gaining focus at `now` should open, if any. Consumes the pending
    /// notification either way.
    pub fn take(&self, now: Instant) -> Option<&'static str> {
        self.lock()
            .take()
            .filter(|(until, _)| now <= *until)
            .map(|(_, page)| page)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<(Instant, &'static str)>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What a notification is about, for the user's per-kind choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Morning brief, due reminders.
    Proactive,
    /// An agent waiting on the user.
    Agent,
}

/// Whether a notification of `kind` may be shown at `now` (minutes after local midnight;
/// `None` when the local time is unknown, which never silences anything).
pub fn allowed(s: &NotifySettings, kind: Kind, now: Option<Minute>) -> bool {
    let wanted = match kind {
        Kind::Proactive => s.brief,
        Kind::Agent => s.agent,
    };
    let quiet = matches!((&s.quiet, now), (Some(q), Some(n)) if q.contains(n));
    s.enabled && wanted && !quiet
}

/// Minutes after local midnight, in the system time zone.
fn local_minute() -> Option<Minute> {
    let now = jiff::Zoned::now();
    Minute::try_from(i32::from(now.hour()) * 60 + i32::from(now.minute())).ok()
}

fn permitted<R: Runtime>(app: &AppHandle<R>, kind: Kind) -> bool {
    let Some(p) = app.try_state::<Platform>() else {
        return false;
    };
    let ok = allowed(&p.settings().notifications, kind, local_minute());
    if !ok {
        tracing::debug!(?kind, "notification not shown (off or quiet hours)");
    }
    ok
}

/// Show a proactive notification, if the user allows it now. With `open_today`, clicking it
/// opens Today. `Ok` also when it was not shown by choice.
pub fn show<R: Runtime>(
    app: &AppHandle<R>,
    title: &str,
    body: &str,
    open_today: bool,
) -> Result<(), String> {
    if !permitted(app, Kind::Proactive) {
        return Ok(());
    }
    deliver(app, title, body, open_today.then_some(TODAY_PAGE))
}

/// Show a notification (no gating); clicking it opens `page` when given.
fn deliver<R: Runtime>(
    app: &AppHandle<R>,
    title: &str,
    body: &str,
    page: Option<&'static str>,
) -> Result<(), String> {
    let title: String = title.chars().take(MAX_TITLE_LEN).collect();
    let body: String = body.chars().take(MAX_BODY_LEN).collect();
    if let Some(page) = page {
        match show_clickable(app, &title, &body, page) {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::warn!(error = %e, "notification: no click callback; opening the page on focus instead");
                if let Some(p) = app.try_state::<PendingOpen>() {
                    p.arm(Instant::now(), page);
                }
            }
        }
    }
    show_plain(app, title, body)
}

/// Seen `agent.event` notifications that need the user, by task: when each was last notified.
#[derive(Default)]
struct AgentNotified(Mutex<HashMap<String, Instant>>);

/// Whether an `agent.event` notification's params ask for the user: a confirmation or an
/// `ask_user` question. Returns the task id.
pub fn needs_user(params: &serde_json::Value) -> Option<&str> {
    let kind = params.pointer("/event/event")?.as_str()?;
    matches!(kind, "confirmation_needed" | "suspended")
        .then(|| params.get("task_id")?.as_str())
        .flatten()
}

/// Notify when an agent waits on the user and the main window is not in front.
pub fn follow_agent_events(app: &AppHandle) {
    app.manage(AgentNotified::default());
    let h = app.clone();
    app.listen_any(crate::daemon::event::NOTIFICATION, move |e| {
        let Ok(note) = serde_json::from_str::<nugget_ipc::Notification>(e.payload()) else {
            return;
        };
        if note.method != nugget_core::ipc::agent_notification::AGENT_EVENT {
            return;
        }
        let Some(task) = needs_user(&note.params) else {
            return;
        };
        let focused = h
            .get_webview_window("main")
            .and_then(|w| w.is_focused().ok())
            .unwrap_or(false);
        if focused || !permitted(&h, Kind::Agent) {
            return;
        }
        let now = Instant::now();
        {
            let seen = h.state::<AgentNotified>();
            let mut seen = seen.0.lock().unwrap_or_else(PoisonError::into_inner);
            if seen
                .get(task)
                .is_some_and(|t| now.duration_since(*t) < AGENT_REPEAT)
            {
                return;
            }
            seen.retain(|_, t| now.duration_since(*t) < AGENT_REPEAT);
            seen.insert(task.to_owned(), now);
        }
        let title = crate::platform::label(&h, "shell.notify.agentTitle");
        let body = crate::platform::label(&h, "shell.notify.agentBody");
        if let Err(e) = deliver(&h, &title, &body, Some(AGENT_PAGE)) {
            tracing::warn!(error = %e, "agent notification not shown");
        }
    });
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

/// The notification was clicked: show the window on its page.
fn clicked<R: Runtime>(app: &AppHandle<R>, page: &str) {
    if let Some(p) = app.try_state::<PendingOpen>() {
        p.take(Instant::now());
    }
    crate::tray::show_main(app, Some(page));
}

/// Main window events: the fallback for notifications shown without a click callback.
pub fn on_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    if window.label() != "main" || !matches!(event, WindowEvent::Focused(true)) {
        return;
    }
    let app = window.app_handle();
    if let Some(page) = app
        .try_state::<PendingOpen>()
        .and_then(|p| p.take(Instant::now()))
    {
        crate::tray::show_main(app, Some(page));
    }
}

#[cfg(target_os = "linux")]
fn show_clickable<R: Runtime>(
    app: &AppHandle<R>,
    title: &str,
    body: &str,
    page: &'static str,
) -> Result<(), String> {
    let mut n = notify_rust::Notification::new();
    n.summary(title)
        .body(body)
        .auto_icon()
        .action("default", &crate::platform::label(app, "shell.notify.open"));
    let app = app.clone();
    // `show` talks to the session bus and `wait_for_action` blocks until the notification is
    // clicked or closed: both on a thread of their own.
    std::thread::Builder::new()
        .name("notification".into())
        .spawn(move || match n.show() {
            Ok(handle) => handle.wait_for_action(|action| on_action(&app, action, page)),
            Err(e) => {
                tracing::warn!(error = %e, "notification: D-Bus failed; using the plugin");
                if let Some(p) = app.try_state::<PendingOpen>() {
                    p.arm(Instant::now(), page);
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
fn on_action<R: Runtime>(app: &AppHandle<R>, action: &str, page: &str) {
    if action == "default" {
        clicked(app, page);
    }
}

#[cfg(target_os = "macos")]
fn show_clickable<R: Runtime>(
    app: &AppHandle<R>,
    title: &str,
    body: &str,
    page: &'static str,
) -> Result<(), String> {
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
                Ok(response) => on_response(&app, &response, page),
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
    page: &str,
) {
    if matches!(response, mac_notification_sys::NotificationResponse::Click) {
        clicked(app, page);
    }
}

#[cfg(windows)]
fn show_clickable<R: Runtime>(
    app: &AppHandle<R>,
    title: &str,
    body: &str,
    page: &'static str,
) -> Result<(), String> {
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
            on_activated(&app, action.as_deref(), page);
            Ok(())
        })
        .show()
        .map_err(|e| format!("{e:?}"))
}

/// The toast's `Activated` handler: the toast has no buttons, so any activation is a click on it.
#[cfg(windows)]
fn on_activated<R: Runtime>(app: &AppHandle<R>, _action: Option<&str>, page: &str) {
    clicked(app, page);
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn show_clickable<R: Runtime>(
    _: &AppHandle<R>,
    _: &str,
    _: &str,
    _: &'static str,
) -> Result<(), String> {
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
    /// AppUserModelID outside a dev build), [`PendingOpen`], [`Platform`] (notifications on) and a
    /// `main` window. The receiver gets
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
            .manage(PendingOpen::default())
            .manage(Platform::new(None))
            .build(ctx)
            .expect("mock app");
        app.state::<Platform>()
            .update(|s| s.notifications.enabled = true)
            .expect("notifications on");
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
    fn focusing_main_after_a_notification_without_click_callback_opens_today_once() {
        let (app, main, rx) = app();
        let window = main.as_ref().window();
        window_focus(&window, false);
        window_focus(&window, true);
        assert_eq!(navigations(&rx), Vec::<String>::new(), "nothing pending");
        app.state::<PendingOpen>().arm(Instant::now(), TODAY_PAGE);
        window_focus(&window, false);
        window_focus(&window, true);
        window_focus(&window, true);
        assert_eq!(navigations(&rx), [TODAY_JSON], "once");
        app.state::<PendingOpen>().arm(Instant::now(), AGENT_PAGE);
        window_focus(&window, true);
        assert_eq!(
            navigations(&rx),
            ["\"agent\""],
            "the notification's own page"
        );
    }

    fn window_focus(window: &Window<MockRuntime>, focused: bool) {
        on_window_event(window, &WindowEvent::Focused(focused));
    }

    /// The function `notify-rust`'s `wait_for_action` calls.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_default_action_is_a_click() {
        let (app, _main, rx) = app();
        on_action(app.handle(), "__closed", TODAY_PAGE);
        assert_eq!(navigations(&rx), Vec::<String>::new());
        on_action(app.handle(), "default", TODAY_PAGE);
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
            on_response(app.handle(), &r, TODAY_PAGE);
        }
        assert_eq!(navigations(&rx), Vec::<String>::new());
        on_response(app.handle(), &N::Click, TODAY_PAGE);
        assert_eq!(navigations(&rx), [TODAY_JSON]);
    }

    /// The toast's `Activated` handler.
    #[cfg(windows)]
    #[test]
    fn an_activation_is_a_click() {
        let (app, _main, rx) = app();
        on_activated(app.handle(), None, TODAY_PAGE);
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
            app.state::<PendingOpen>().take(Instant::now()).is_none(),
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
    fn focus_soon_after_a_notification_opens_its_page_once() {
        let p = PendingOpen::default();
        let t0 = Instant::now();
        assert_eq!(p.take(t0), None, "nothing pending");
        p.arm(t0, TODAY_PAGE);
        assert_eq!(p.take(t0 + Duration::from_secs(30)), Some(TODAY_PAGE));
        assert_eq!(p.take(t0 + Duration::from_secs(31)), None, "consumed");
        // Too late: consumed without opening.
        p.arm(t0, TODAY_PAGE);
        assert_eq!(p.take(t0 + FOCUS_WINDOW + Duration::from_secs(1)), None);
        assert_eq!(p.take(t0 + Duration::from_secs(1)), None);
        // A newer notification restarts the window and wins.
        p.arm(t0, TODAY_PAGE);
        p.arm(t0 + FOCUS_WINDOW, AGENT_PAGE);
        assert_eq!(
            p.take(t0 + FOCUS_WINDOW + Duration::from_secs(60)),
            Some(AGENT_PAGE)
        );
    }

    #[test]
    fn notifications_are_gated_by_opt_in_kind_and_quiet_hours() {
        use crate::platform::QuietHours;
        let mut s = NotifySettings::default();
        assert!(!allowed(&s, Kind::Proactive, Some(600)), "off by default");
        s.enabled = true;
        assert!(allowed(&s, Kind::Proactive, Some(600)));
        assert!(allowed(&s, Kind::Agent, None));
        s.agent = false;
        assert!(!allowed(&s, Kind::Agent, Some(600)));
        assert!(allowed(&s, Kind::Proactive, Some(600)));
        s.quiet = Some(QuietHours {
            start: 22 * 60,
            end: 7 * 60,
        });
        assert!(!allowed(&s, Kind::Proactive, Some(23 * 60)));
        assert!(allowed(&s, Kind::Proactive, Some(12 * 60)));
        assert!(
            allowed(&s, Kind::Proactive, None),
            "unknown local time never silences"
        );
    }

    #[test]
    fn proactive_notifications_respect_the_opt_in() {
        let (app, _main, _rx) = app();
        app.state::<Platform>()
            .update(|s| s.notifications.enabled = false)
            .unwrap();
        // Not shown, not an error, nothing armed.
        show(app.handle(), "t", "b", true).unwrap();
        assert_eq!(app.state::<PendingOpen>().take(Instant::now()), None);
    }

    #[test]
    fn only_confirmations_and_questions_need_the_user() {
        let ev = |kind: &str| serde_json::json!({"task_id": "t1", "event": {"event": kind, "task_id": "t1"}});
        assert_eq!(needs_user(&ev("confirmation_needed")), Some("t1"));
        assert_eq!(needs_user(&ev("suspended")), Some("t1"));
        assert_eq!(needs_user(&ev("step_started")), None);
        assert_eq!(needs_user(&serde_json::json!({"event": "x"})), None);
    }
}
