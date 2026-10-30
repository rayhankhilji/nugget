//! The runtime half of Double Tap: listener lifecycle, the draft flow from trigger to feedback,
//! and the effects the overlay state machine asks for.
//!
//! # Focus and the insert target
//!
//! The overlay needs the keyboard (Enter, Tab, Esc), so it takes focus while a draft streams;
//! refusals and errors are shown without focus. Before inserting, the overlay is hidden and the
//! target app is brought back to the front, then [`wait_frontmost`] checks (through the
//! accessibility API) that the frontmost app is the one the draft was read from; if another app
//! stays in front, nothing is inserted (`doubletap.error.insert_failed`).
//!
//! - **macOS:** hiding a window leaves Nugget active, so the target is reactivated with
//!   `open -b <bundle id>` (Launch Services; no Apple Events permission needed), or by hiding
//!   Nugget (`NSApp hide`, which activates the previous app) when there is no bundle id.
//! - **Windows:** hiding the active overlay activates the next window in z-order, which is the
//!   window it was opened over. (Forcing the foreground window would need `unsafe` Win32 calls.)
//! - **Linux:** X11 window managers and Wayland compositors return focus to the previously
//!   focused window when the overlay is hidden.

use std::time::Duration;

use nugget_a11y::Accessibility;
use nugget_core::ids::ProviderId;
use nugget_core::ipc::{
    AppInfo, ChatCancel, ChatDelta, ChatDone, DraftContext, DraftRequest, DraftStarted, method,
};
use tauri::{AppHandle, Emitter, Listener as _, Manager, PhysicalPosition};

use super::errors::{self, Failure};
use super::overlay::{Effect, Event, Started};
use super::placement::{self, Area, Screen};
use super::probe;
use super::settings;
use super::{DRAFT_PREWARM, DoubleTap, OVERLAY_EVENT, OVERLAY_LABEL, OVERLAY_SIZE, lock};
use crate::daemon::{self, Daemon};

/// Where the draft was read from: the app to return focus to and the caret to place the
/// overlay at.
#[derive(Debug, Clone)]
pub struct Target {
    pub app: AppInfo,
    pub caret: Option<nugget_a11y::Rect>,
}

// --- the accessibility backend ----------------------------------------------------------------

#[cfg(target_os = "linux")]
type Inner = nugget_a11y::Backend<nugget_a11y::AtspiPlatform>;
#[cfg(target_os = "macos")]
type Inner = nugget_a11y::Backend<nugget_a11y::AxPlatform>;
#[cfg(target_os = "windows")]
type Inner = nugget_a11y::Backend<nugget_a11y::UiaPlatform>;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
type Inner = nugget_a11y::Unsupported;

/// The OS accessibility backend, concrete so the cheap frontmost-app read is available.
pub struct A11y {
    inner: Inner,
}

impl A11y {
    pub fn new() -> Self {
        #[cfg(target_os = "linux")]
        let inner = nugget_a11y::Backend::new(
            nugget_a11y::AtspiPlatform::new(),
            nugget_a11y::A11yConfig::default(),
        );
        #[cfg(target_os = "macos")]
        let inner = nugget_a11y::Backend::new(
            nugget_a11y::AxPlatform::new(),
            nugget_a11y::A11yConfig::default(),
        );
        #[cfg(target_os = "windows")]
        let inner = nugget_a11y::Backend::new(
            nugget_a11y::UiaPlatform::new(),
            nugget_a11y::A11yConfig::default(),
        );
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        let inner = nugget_a11y::Unsupported;
        Self { inner }
    }

    pub fn api(&self) -> &dyn Accessibility {
        &self.inner
    }

    /// The frontmost app (with its page URL), without reading any field.
    pub fn frontmost(&self) -> Option<AppInfo> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            self.inner
                .frontmost_app()
                .ok()
                .flatten()
                .map(|(app, _)| app)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            None
        }
    }
}

impl Default for A11y {
    fn default() -> Self {
        Self::new()
    }
}

// --- listener lifecycle -------------------------------------------------------------------------

/// Stop the current listener and start one for the current settings (none when disabled),
/// recording why it could not start. On macOS call this on the main thread.
pub fn restart_listener(app: &AppHandle) {
    let state = app.state::<DoubleTap>();
    let mut slot = lock(&state.listener);
    // Drop first, so a chord is released before it is registered again.
    *slot = None;
    let s = lock(&state.settings).clone();
    let mut error = None;
    if s.enabled {
        let handle = app.clone();
        match nugget_hotkey::start((&s.trigger).into(), Box::new(move || on_trigger(&handle))) {
            Ok(l) => *slot = Some(l),
            Err(e) => {
                tracing::warn!(error = %e, "Double Tap trigger unavailable");
                error = Some(e.to_string());
            }
        }
    }
    probe::record(
        "listener",
        || serde_json::json!({ "listening": slot.is_some(), "error": error }),
    );
    *lock(&state.trigger_error) = error;
}

/// [`restart_listener`] on the right thread, waiting until it is done.
pub async fn restart_listener_async(app: &AppHandle) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let h = app.clone();
    #[cfg(target_os = "macos")]
    {
        let r = app.run_on_main_thread(move || {
            restart_listener(&h);
            let _ = tx.send(());
        });
        if let Err(e) = r {
            tracing::warn!(error = %e, "cannot reach the main thread");
            return;
        }
    }
    #[cfg(not(target_os = "macos"))]
    tauri::async_runtime::spawn_blocking(move || {
        restart_listener(&h);
        let _ = tx.send(());
    });
    let _ = rx.await;
}

/// Runs on the hotkey dispatcher thread.
fn on_trigger(app: &AppHandle) {
    probe::record("trigger", || serde_json::Value::Null);
    let app = app.clone();
    tauri::async_runtime::spawn(fire(app));
}

// --- the draft flow ------------------------------------------------------------------------------

fn fail(app: &AppHandle, f: Failure) {
    match f {
        Failure::Refused(k) => apply(app, Event::Refused(k.into())),
        Failure::Error(e) => apply(app, Event::Error(e)),
    };
}

/// Why drafting is refused locally in `app`, if it is.
pub(super) fn local_refusal(
    app: &AppInfo,
    s: &settings::DoubleTapSettings,
) -> Option<&'static str> {
    if settings::is_excluded(app, &s.excluded_apps) {
        Some(errors::REFUSED_EXCLUDED)
    } else if !s.allow_terminals && settings::is_terminal(app) {
        Some(errors::REFUSED_TERMINAL)
    } else {
        None
    }
}

/// The trigger fired (or `doubletap_fire`): read the field, refuse or start a draft.
pub async fn fire(app: AppHandle) {
    let state = app.state::<DoubleTap>();
    {
        let mut m = lock(&state.machine);
        if m.busy() {
            return;
        }
        let step = m.apply(Event::Begin);
        let _ = app.emit_to(OVERLAY_LABEL, OVERLAY_EVENT, m.view());
        probe::record("view", || serde_json::json!(m.view()));
        drop(m);
        run(&app, step.effects);
    }
    *lock(&state.target) = None;
    let s = lock(&state.settings).clone();
    let a11y = state.a11y.clone();

    let h = app.clone();
    let s2 = s.clone();
    let read = tauri::async_runtime::spawn_blocking(move || {
        // Prewarm as early as possible, before the (slower) field and thread read.
        if let Some(front) = a11y.frontmost()
            && local_refusal(&front, &s2).is_none()
        {
            tauri::async_runtime::spawn(prewarm(h, front));
        }
        a11y.api().focused()
    })
    .await;
    let focused = match read {
        Err(e) => return fail(&app, Failure::Error(e.to_string())),
        Ok(Err(e)) => return fail(&app, errors::classify_a11y(&e, false)),
        Ok(Ok(None)) => return fail(&app, Failure::Refused(errors::REFUSED_NO_FIELD)),
        Ok(Ok(Some(f))) => f,
    };
    // A secure field's value is never read (the backend leaves it empty) and never sent.
    if focused.field.secure {
        return fail(&app, Failure::Refused(errors::REFUSED_SECURE));
    }
    if let Some(key) = local_refusal(&focused.app, &s) {
        return fail(&app, Failure::Refused(key));
    }
    *lock(&state.target) = Some(Target {
        app: focused.app.clone(),
        caret: focused.caret,
    });
    let request = draft_request(focused, &s);
    match begin(&app, &request).await {
        Ok(started) => {
            apply(
                &app,
                Event::Started(Started {
                    stream_id: started.stream_id,
                    provider: started.provider.0,
                    locality: started.locality,
                    citations: u32::try_from(started.citations.len()).unwrap_or(u32::MAX),
                    excluded_sensitive: started.excluded_sensitive,
                }),
            );
        }
        Err(f) => fail(&app, f),
    }
}

/// The request `draft.begin` gets for a field read (read-ahead sends the same one, so the
/// daemon prefills exactly the prompt the trigger will ask for).
pub(super) fn draft_request(
    focused: nugget_a11y::Focused,
    s: &settings::DoubleTapSettings,
) -> DraftRequest {
    let provider = settings::provider_for(&focused.app, &s.app_providers);
    DraftRequest {
        context: DraftContext {
            app: focused.app,
            field: focused.field,
            thread: focused.thread,
            recipients: focused.recipients,
            subject: focused.subject,
            screenshot_png_base64: None,
        },
        instruction: None,
        provider: provider.map(ProviderId::new),
        max_tokens: None,
    }
}

/// `draft.begin` on the raw client, so the refusal reason in the JSON-RPC error's `data`
/// (dropped by `CmdError`) can be mapped.
async fn begin(app: &AppHandle, request: &DraftRequest) -> Result<DraftStarted, Failure> {
    let daemon = app.state::<Daemon>();
    let client = daemon
        .client(app)
        .await
        .map_err(|_| Failure::Error(errors::ERROR_OFFLINE.into()))?;
    crate::daemon::retry_busy(|| client.call(method::DRAFT_BEGIN, request))
        .await
        .map_err(|e| errors::classify_ipc(&e))
}

/// Fire and forget: warm the provider for this app while the field is read.
async fn prewarm(app: AppHandle, front: AppInfo) {
    let ctx = DraftContext {
        app: front,
        ..DraftContext::default()
    };
    let daemon = app.state::<Daemon>();
    if let Err(e) = daemon
        .call::<_, serde_json::Value>(&app, DRAFT_PREWARM, &ctx)
        .await
    {
        tracing::debug!(error = %e, "draft.prewarm failed");
    }
}

/// Follow `chat.delta` / `chat.done` (re-emitted by `daemon.rs`) into the overlay.
pub fn follow_streams(app: &AppHandle) {
    let h = app.clone();
    app.listen(daemon::event::CHAT_DELTA, move |e| {
        if let Ok(d) = serde_json::from_str::<ChatDelta>(e.payload()) {
            apply(
                &h,
                Event::Delta {
                    stream_id: d.stream_id,
                    text: d.text,
                },
            );
        }
    });
    let h = app.clone();
    app.listen(daemon::event::CHAT_DONE, move |e| {
        if let Ok(d) = serde_json::from_str::<ChatDone>(e.payload()) {
            apply(
                &h,
                Event::Done {
                    stream_id: d.stream_id,
                    error: d.error,
                },
            );
        }
    });
    // No `chat.done` arrives when the connection drops.
    let h = app.clone();
    app.listen(daemon::event::CONNECTION, move |e| {
        if is_disconnect(e.payload()) {
            apply(&h, Event::Disconnected);
        }
    });
}

/// Whether a `nugget:connection` payload reports the connection down.
fn is_disconnect(payload: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| v.get("connected").and_then(serde_json::Value::as_bool))
        == Some(false)
}

/// Feed an event to the state machine, emit the view if it changed, run the effects.
pub fn apply(app: &AppHandle, event: Event) {
    let Some(state) = app.try_state::<DoubleTap>() else {
        return;
    };
    let effects = {
        let mut m = lock(&state.machine);
        let step = m.apply(event);
        if step.changed {
            // Emitted under the lock so views arrive in order.
            let _ = app.emit_to(OVERLAY_LABEL, OVERLAY_EVENT, m.view());
            probe::record("view", || serde_json::json!(m.view()));
        }
        step.effects
    };
    run(app, effects);
}

fn run(app: &AppHandle, effects: Vec<Effect>) {
    for effect in effects {
        match effect {
            Effect::Show { focus } => show_overlay(app, focus),
            Effect::Hide => hide_overlay(app),
            Effect::HideAfter { generation, ms } => {
                let h = app.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    apply(&h, Event::Expire { generation });
                });
            }
            Effect::CancelStream(stream_id) => {
                let h = app.clone();
                tauri::async_runtime::spawn(async move {
                    let daemon = h.state::<Daemon>();
                    if let Err(e) = daemon
                        .call::<_, serde_json::Value>(
                            &h,
                            method::DRAFT_CANCEL,
                            &ChatCancel { stream_id },
                        )
                        .await
                    {
                        tracing::debug!(error = %e, "draft.cancel failed");
                    }
                });
            }
            Effect::Feedback(fb) => {
                let h = app.clone();
                tauri::async_runtime::spawn(async move {
                    let daemon = h.state::<Daemon>();
                    if let Err(e) = daemon
                        .call::<_, serde_json::Value>(&h, method::DRAFT_FEEDBACK, &fb)
                        .await
                    {
                        tracing::warn!(error = %e, "draft.feedback failed");
                    }
                });
            }
            Effect::Insert(text) => {
                tauri::async_runtime::spawn(insert(app.clone(), text));
            }
        }
    }
}

// --- the overlay window ----------------------------------------------------------------------------

fn show_overlay(app: &AppHandle, focus: bool) {
    let Some(win) = app.get_webview_window(OVERLAY_LABEL) else {
        return;
    };
    let caret = app
        .state::<DoubleTap>()
        .target
        .lock()
        .ok()
        .and_then(|t| t.as_ref().and_then(|t| t.caret));
    let screens: Vec<Screen> = win
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let (p, s, w) = (m.position(), m.size(), m.work_area());
            Screen {
                bounds: Area::new(
                    f64::from(p.x),
                    f64::from(p.y),
                    f64::from(s.width),
                    f64::from(s.height),
                ),
                work: Area::new(
                    f64::from(w.position.x),
                    f64::from(w.position.y),
                    f64::from(w.size.width),
                    f64::from(w.size.height),
                ),
                scale: m.scale_factor(),
            }
        })
        .collect();
    let fallback = app.cursor_position().ok().map(|p| (p.x, p.y));
    let caret = caret.map(|c| Area::new(c.x, c.y, c.width, c.height));
    if let Some((x, y)) = placement::place(
        caret,
        placement::CARET_UNITS,
        &screens,
        OVERLAY_SIZE,
        fallback,
    ) {
        let _ = win.set_position(PhysicalPosition::new(x, y));
    }
    let _ = win.set_focusable(focus);
    let _ = win.set_always_on_top(true);
    let _ = win.show();
    if focus {
        let _ = win.set_focus();
    }
    probe::window("show", &win);
}

fn hide_overlay(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = win.hide();
        let _ = win.set_focusable(false);
        probe::window("hide", &win);
    }
}

// --- insertion ---------------------------------------------------------------------------------------

async fn insert(app: AppHandle, text: String) {
    let state = app.state::<DoubleTap>();
    let target = lock(&state.target).clone();
    let a11y = state.a11y.clone();
    let h = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        restore_focus(&h, target.as_ref().map(|t| &t.app));
        if let Some(t) = &target
            && !wait_frontmost(&a11y, &t.app)
        {
            return Err(Failure::Error(errors::ERROR_INSERT_FAILED.into()));
        }
        a11y.api()
            .insert(&text)
            .map_err(|e| errors::classify_a11y(&e, true))
    })
    .await;
    probe::record(
        "insert",
        || serde_json::json!({ "ok": matches!(result, Ok(Ok(_))) }),
    );
    match result {
        Ok(Ok(outcome)) => apply(
            &app,
            Event::Inserted {
                verified: outcome.verified,
                method: outcome.method,
            },
        ),
        Ok(Err(Failure::Refused(k))) => apply(&app, Event::Error(k.into())),
        Ok(Err(Failure::Error(e))) => apply(&app, Event::Error(e)),
        Err(e) => apply(&app, Event::Error(e.to_string())),
    }
}

/// Give the keyboard back to the app the draft was read from (see the module docs).
fn restore_focus(app: &AppHandle, target: Option<&AppInfo>) {
    // Let the overlay's hide reach the window system first.
    std::thread::sleep(Duration::from_millis(60));
    #[cfg(target_os = "macos")]
    {
        match target
            .and_then(|t| t.id.as_deref())
            .filter(|id| is_bundle_id(id))
        {
            Some(id) => {
                if let Err(e) = std::process::Command::new("/usr/bin/open")
                    .args(["-b", id])
                    .status()
                {
                    tracing::debug!(error = %e, "cannot reactivate the target app");
                }
            }
            None => {
                let _ = app.hide();
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, target);
}

#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn is_bundle_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && id.contains('.')
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

pub fn same_app(a: &AppInfo, b: &AppInfo) -> bool {
    match (a.id.as_deref(), b.id.as_deref()) {
        (Some(x), Some(y)) if !x.is_empty() && !y.is_empty() => x.eq_ignore_ascii_case(y),
        _ => !a.name.trim().is_empty() && a.name.trim().eq_ignore_ascii_case(b.name.trim()),
    }
}

/// Wait until `target` is frontmost again. `false` when another app stays in front; `true` when
/// it is, or when the frontmost app cannot be read (the backend's own checks still apply).
fn wait_frontmost(a11y: &A11y, target: &AppInfo) -> bool {
    const TIMEOUT: Duration = Duration::from_millis(1_500);
    const POLL: Duration = Duration::from_millis(30);
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        let front = a11y.frontmost();
        match &front {
            Some(f) if same_app(f, target) => return true,
            _ if std::time::Instant::now() >= deadline => return front.is_none(),
            _ => std::thread::sleep(POLL),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: Option<&str>, name: &str) -> AppInfo {
        AppInfo {
            id: id.map(Into::into),
            name: name.into(),
            window_title: None,
            url: None,
        }
    }

    #[test]
    fn same_app_compares_ids_then_names() {
        assert!(same_app(
            &app(Some("com.tinyspeck.slackmacgap"), "Slack"),
            &app(Some("COM.TINYSPECK.SLACKMACGAP"), "Slack Beta")
        ));
        assert!(!same_app(
            &app(Some("com.apple.mail"), "Mail"),
            &app(Some("com.apple.notes"), "Mail")
        ));
        assert!(same_app(
            &app(None, "Thunderbird"),
            &app(Some("x"), "thunderbird")
        ));
        assert!(!same_app(&app(None, ""), &app(None, "")));
    }

    #[test]
    fn connection_payloads() {
        assert!(is_disconnect(
            r#"{"connected":false,"socket":"s","error":"connection closed"}"#
        ));
        assert!(!is_disconnect(
            r#"{"connected":true,"socket":"s","error":null}"#
        ));
        assert!(!is_disconnect("garbage"));
    }

    #[test]
    fn bundle_ids_are_validated_before_use() {
        assert!(is_bundle_id("com.tinyspeck.slackmacgap"));
        assert!(is_bundle_id("org.mozilla.firefox"));
        assert!(!is_bundle_id("-a Calculator"));
        assert!(!is_bundle_id("Slack"));
        assert!(!is_bundle_id("com.example;rm"));
    }

    #[test]
    fn local_refusals() {
        let s = settings::DoubleTapSettings::default();
        assert_eq!(
            local_refusal(&app(Some("com.bitwarden.desktop"), "Bitwarden"), &s),
            Some(errors::REFUSED_EXCLUDED)
        );
        assert_eq!(
            local_refusal(&app(Some("com.apple.Terminal"), "Terminal"), &s),
            Some(errors::REFUSED_TERMINAL)
        );
        assert_eq!(
            local_refusal(&app(Some("com.tinyspeck.slackmacgap"), "Slack"), &s),
            None
        );
        let allow = settings::DoubleTapSettings {
            allow_terminals: true,
            excluded_apps: Vec::new(),
            ..s
        };
        assert_eq!(
            local_refusal(&app(Some("com.apple.Terminal"), "Terminal"), &allow),
            None
        );
        // The accessibility layer's built-in deny list applies even with an empty list.
        assert_eq!(
            local_refusal(
                &app(Some("com.agilebits.onepassword7"), "1Password 7"),
                &allow
            ),
            Some(errors::REFUSED_EXCLUDED)
        );
    }
}
