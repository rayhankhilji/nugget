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
//!
//! Esc (and a refusal timing out) hides the overlay and hands the keyboard back the same way
//! ([`Effect::RestoreFocus`]).
//!
//! # Retry, refinements, practice
//!
//! The request of the session's last draft is kept ([`DoubleTap::request`]): Retry sends it again,
//! a refinement sends it with the draft as the field's selected text and the refinement's
//! instruction ([`refined_request`]), so the daemon rewrites that draft (same provider choice,
//! same privacy rules). When the practice field on the Double Tap page has focus, the trigger
//! drafts for it instead of reading another app (see [`super::practice`]).

use std::time::Duration;

use nugget_a11y::Accessibility;
use nugget_core::ids::ProviderId;
use nugget_core::ipc::{
    AppInfo, ChatCancel, ChatDelta, ChatDone, DraftContext, DraftRequest, DraftStarted, method,
};
use tauri::{AppHandle, Emitter, Listener as _, Manager, PhysicalPosition, PhysicalSize};

use super::errors::{self, Failure};
use super::overlay::{Effect, Event, Refinement, Started};
use super::placement::{self, Area, Screen};
use super::practice::{self, PracticeField};
use super::probe;
use super::settings;
use super::{
    DRAFT_PREWARM, DoubleTap, HISTORY_EVENT, MAIN_LABEL, OVERLAY_EVENT, OVERLAY_LABEL,
    OVERLAY_MAX_HEIGHT, OVERLAY_MIN_HEIGHT, OVERLAY_SIZE, PRACTICE_EVENT, lock,
};
use crate::daemon::{self, Daemon};

/// Where the draft was read from: the app to return focus to and the caret to place the
/// overlay at.
#[derive(Debug, Clone)]
pub struct Target {
    pub app: AppInfo,
    pub caret: Option<nugget_a11y::Rect>,
    /// The field's text runs right to left (the overlay lines up with the caret's end).
    pub rtl: bool,
    /// The practice field on the Double Tap page (the caret is in physical pixels).
    pub practice: bool,
    /// The field's label names a secret: its drafts are never kept in the history.
    pub sensitive: bool,
}

/// The overlay's measured height (logical) and the side of the caret it was put on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub height: f64,
    pub above: Option<bool>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            height: OVERLAY_SIZE.1,
            above: None,
        }
    }
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
    *lock(&state.request) = None;
    *lock(&state.layout) = Layout::default();
    let s = lock(&state.settings).clone();
    if let Some(field) = practice_target(&app) {
        apply(&app, Event::Practice);
        return fire_practice(&app, field, &s).await;
    }
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
        rtl: is_rtl_context(&focused),
        practice: false,
        sensitive: focused
            .field
            .label
            .as_deref()
            .is_some_and(nugget_policy::redact::is_secure_label),
    });
    let request = draft_request(focused, &s);
    start(&app, request).await;
}

/// The practice field, when it has focus in Nugget's main window (frontmost).
fn practice_target(app: &AppHandle) -> Option<PracticeField> {
    let field = lock(&app.state::<DoubleTap>().practice).clone()?;
    let main = app.get_webview_window(MAIN_LABEL)?;
    main.is_focused().unwrap_or(false).then_some(field)
}

/// The trigger fired over the practice field: draft for real from its sandbox context.
async fn fire_practice(app: &AppHandle, field: PracticeField, s: &settings::DoubleTapSettings) {
    let state = app.state::<DoubleTap>();
    // The page's viewport rect, in physical pixels on the desktop.
    let caret = app.get_webview_window(MAIN_LABEL).and_then(|w| {
        let origin = w.inner_position().ok()?;
        let k = w.scale_factor().ok()?;
        let r = field.rect;
        Some(nugget_a11y::Rect {
            x: f64::from(origin.x) + r.x * k,
            y: f64::from(origin.y) + r.y * k,
            width: r.width * k,
            height: r.height * k,
        })
    });
    let app_info = PracticeField::app();
    let provider = settings::provider_for(&app_info, &s.app_providers);
    *lock(&state.target) = Some(Target {
        app: app_info,
        caret,
        rtl: field.rtl,
        practice: true,
        sensitive: false,
    });
    probe::record("practice", || serde_json::Value::Null);
    start(app, field.request(provider.map(ProviderId::new))).await;
}

/// Keep the request (for Retry and the refinements) and start the draft.
async fn start(app: &AppHandle, request: DraftRequest) {
    *lock(&app.state::<DoubleTap>().request) = Some(request.clone());
    match begin(app, &request).await {
        Ok(started) => apply(app, Event::Started(started_event(started))),
        Err(f) => fail(app, f),
    }
}

fn started_event(started: DraftStarted) -> Started {
    Started {
        sensitive: started.citations.iter().any(|c| c.sensitive),
        stream_id: started.stream_id,
        provider: started.provider.0,
        locality: started.locality,
        citations: u32::try_from(started.citations.len()).unwrap_or(u32::MAX),
        excluded_sensitive: started.excluded_sensitive,
    }
}

/// Retry or a refinement: the session's request again, or rewriting `base`.
async fn redraft(app: AppHandle, refine: Option<(Refinement, String)>) {
    let Some(request) = lock(&app.state::<DoubleTap>().request).clone() else {
        return fail(&app, Failure::Error(errors::ERROR_DRAFT_FAILED.into()));
    };
    let request = match refine {
        Some((kind, base)) => refined_request(&request, &base, kind),
        None => request,
    };
    // The kept request stays the original: a second refinement rewrites the new draft from the
    // same context, not a rewrite of a rewrite's prompt.
    match begin(&app, &request).await {
        Ok(started) => apply(&app, Event::Started(started_event(started))),
        Err(f) => fail(&app, f),
    }
}

/// The request for a refinement: the original context with `base` as the selected text of the
/// field (where it would be inserted), so the daemon writes its replacement, plus the
/// refinement's instruction. The provider choice is unchanged.
pub(super) fn refined_request(
    original: &DraftRequest,
    base: &str,
    kind: Refinement,
) -> DraftRequest {
    let mut r = original.clone();
    let value = &original.context.field.value;
    let chars: Vec<char> = value.chars().collect();
    let n = chars.len();
    let (a, b) = match original.context.field.selection {
        Some((a, b)) => {
            let (a, b) = ((a.min(b) as usize).min(n), (a.max(b) as usize).min(n));
            (a, b)
        }
        None => (n, n),
    };
    let before: String = chars[..a].iter().collect();
    let after: String = chars[b..].iter().collect();
    let start = u32::try_from(a).unwrap_or(u32::MAX);
    let end = start.saturating_add(u32::try_from(base.chars().count()).unwrap_or(u32::MAX));
    r.context.field.value = format!("{before}{base}{after}");
    r.context.field.selection = Some((start, end));
    r.instruction = Some(kind.instruction().into());
    r
}

/// Whether the field's text (else the newest message around it) starts right to left.
fn is_rtl_context(f: &nugget_a11y::Focused) -> bool {
    first_strong_rtl(&f.field.value)
        .or_else(|| f.thread.last().and_then(|m| first_strong_rtl(&m.text)))
        .unwrap_or(false)
}

/// The direction of the first strong character: `Some(true)` for Hebrew, Arabic, Syriac,
/// Thaana, NKo and their presentation forms, `Some(false)` for other letters.
fn first_strong_rtl(text: &str) -> Option<bool> {
    text.chars().find(|c| c.is_alphabetic()).map(|c| {
        matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF)
    })
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
                if is_practice(app) {
                    insert_practice(app, text);
                } else {
                    tauri::async_runtime::spawn(insert(app.clone(), text));
                }
            }
            Effect::Redraft(refine) => {
                tauri::async_runtime::spawn(redraft(app.clone(), refine));
            }
            Effect::RestoreFocus => {
                if is_practice(app) {
                    focus_practice(app, practice::Action::Focus);
                } else {
                    let h = app.clone();
                    let target = lock(&app.state::<DoubleTap>().target).clone();
                    tauri::async_runtime::spawn_blocking(move || {
                        restore_focus(&h, target.as_ref().map(|t| &t.app));
                    });
                }
            }
            Effect::Remember(text) => remember(app, &text),
        }
    }
}

fn is_practice(app: &AppHandle) -> bool {
    lock(&app.state::<DoubleTap>().target)
        .as_ref()
        .is_some_and(|t| t.practice)
}

/// Bring the main window back and hand the practice field its action.
fn focus_practice(app: &AppHandle, action: practice::Action) {
    if let Some(main) = app.get_webview_window(MAIN_LABEL) {
        let _ = main.set_focus();
    }
    if let Err(e) = app.emit_to(MAIN_LABEL, PRACTICE_EVENT, &action) {
        tracing::debug!(error = %e, "cannot reach the practice field");
    }
}

/// Practice: the page puts the text in its field; nothing touches another app.
fn insert_practice(app: &AppHandle, text: String) {
    focus_practice(app, practice::Action::Insert { text });
    probe::record(
        "insert",
        || serde_json::json!({ "ok": true, "practice": true }),
    );
    apply(
        app,
        Event::Inserted {
            verified: true,
            method: nugget_a11y::InsertMethod::Accessibility,
        },
    );
}

/// Keep a draft in the history (in memory; never sensitive, see `history.rs`).
fn remember(app: &AppHandle, text: &str) {
    let state = app.state::<DoubleTap>();
    let (app_name, skip) = lock(&state.target)
        .as_ref()
        .map(|t| (t.app.name.clone(), t.practice || t.sensitive))
        .unwrap_or_default();
    if skip {
        return;
    }
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let list = {
        let mut h = lock(&state.history);
        if !h.push(text, &app_name, at) {
            return;
        }
        h.list()
    };
    emit_history(app, &list);
}

pub(super) fn emit_history(app: &AppHandle, list: &[super::history::Entry]) {
    if let Err(e) = app.emit(HISTORY_EVENT, list) {
        tracing::debug!(error = %e, "cannot announce the draft history");
    }
}

// --- the overlay window ----------------------------------------------------------------------------

fn show_overlay(app: &AppHandle, focus: bool) {
    let Some(win) = app.get_webview_window(OVERLAY_LABEL) else {
        return;
    };
    // Choose the side of the caret afresh for each new draft.
    lock(&app.state::<DoubleTap>().layout).above = None;
    place_overlay(app, &win);
    let _ = win.set_focusable(focus);
    let _ = win.set_always_on_top(true);
    let _ = win.show();
    if focus {
        let _ = win.set_focus();
    }
    probe::window("show", &win);
}

/// Size and place the overlay for the measured content height, beside the caret without
/// covering it. Returns the height applied (logical).
fn place_overlay(app: &AppHandle, win: &tauri::WebviewWindow) -> f64 {
    let state = app.state::<DoubleTap>();
    let (caret, rtl, practice) = lock(&state.target)
        .as_ref()
        .map(|t| (t.caret, t.rtl, t.practice))
        .unwrap_or((None, false, false));
    let layout = *lock(&state.layout);
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
    // The practice field's rectangle is computed in physical pixels already.
    let units = if practice {
        placement::Units::Physical
    } else {
        placement::CARET_UNITS
    };
    let want = placement::Want {
        size: (OVERLAY_SIZE.0, layout.height),
        min_height: OVERLAY_MIN_HEIGHT,
        rtl,
        prefer_above: layout.above,
    };
    let Some(fit) = placement::fit(caret, units, &screens, want, fallback) else {
        return layout.height;
    };
    let _ = win.set_size(PhysicalSize::new(fit.width, fit.height));
    let _ = win.set_position(PhysicalPosition::new(fit.x, fit.y));
    lock(&state.layout).above = Some(fit.above);
    f64::from(fit.height) / fit.scale
}

/// The overlay measured its content: fit the window to `height` (logical), within the limits
/// and the room beside the caret. Returns the height applied.
pub fn resize_overlay(app: &AppHandle, height: f64) -> f64 {
    let height = if height.is_finite() {
        height.clamp(OVERLAY_MIN_HEIGHT, OVERLAY_MAX_HEIGHT)
    } else {
        OVERLAY_SIZE.1
    };
    lock(&app.state::<DoubleTap>().layout).height = height;
    let Some(win) = app.get_webview_window(OVERLAY_LABEL) else {
        return height;
    };
    if !win.is_visible().unwrap_or(false) {
        return height;
    }
    let applied = place_overlay(app, &win);
    probe::window("resize", &win);
    applied
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

    fn request(value: &str, selection: Option<(u32, u32)>) -> DraftRequest {
        DraftRequest {
            context: DraftContext {
                app: app(Some("com.apple.mail"), "Mail"),
                field: nugget_core::ipc::FieldInfo {
                    role: "text_area".into(),
                    value: value.into(),
                    selection,
                    ..Default::default()
                },
                ..DraftContext::default()
            },
            instruction: None,
            provider: Some(ProviderId::new("local")),
            max_tokens: None,
        }
    }

    #[test]
    fn refinements_select_the_draft_where_it_would_go() {
        // Empty reply box: the draft is the whole field, selected.
        let r = refined_request(
            &request("", None),
            "Thanks, Thursday works.",
            Refinement::Shorter,
        );
        assert_eq!(r.context.field.value, "Thanks, Thursday works.");
        assert_eq!(r.context.field.selection, Some((0, 23)));
        assert_eq!(
            r.instruction.as_deref(),
            Some(Refinement::Shorter.instruction())
        );
        assert_eq!(
            r.provider,
            Some(ProviderId::new("local")),
            "same provider choice"
        );
        assert_eq!(r.context.app.name, "Mail");
        // Text before the caret is kept; the draft goes at the caret.
        let r = refined_request(&request("Hi Ada,\n", None), "Sure.", Refinement::Casual);
        assert_eq!(r.context.field.value, "Hi Ada,\nSure.");
        assert_eq!(r.context.field.selection, Some((8, 13)));
        // A selection is what the draft replaced: the draft takes its place (char offsets).
        let r = refined_request(&request("é old x", Some((5, 2))), "née", Refinement::Formal);
        assert_eq!(r.context.field.value, "é née x");
        assert_eq!(r.context.field.selection, Some((2, 5)));
        // A selection past the end is clamped.
        let r = refined_request(&request("ab", Some((1, 99))), "Z", Refinement::Longer);
        assert_eq!(r.context.field.value, "aZ");
        assert_eq!(r.context.field.selection, Some((1, 2)));
    }

    #[test]
    fn rtl_from_the_first_strong_character() {
        assert_eq!(first_strong_rtl("  123 שלום"), Some(true));
        assert_eq!(first_strong_rtl("مرحبا Ada"), Some(true));
        assert_eq!(first_strong_rtl("Hello مرحبا"), Some(false));
        assert_eq!(first_strong_rtl("こんにちは"), Some(false));
        assert_eq!(first_strong_rtl("123 !?"), None);
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
