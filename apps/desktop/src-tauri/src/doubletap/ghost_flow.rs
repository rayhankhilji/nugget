//! The runtime half of ghost text: the suggestion key tap, the typing-pause trigger, the field
//! reads, the click-through window at the caret and the insertion. The decisions are made by
//! [`super::ghost::Machine`]; this module only performs its effects and feeds results back.
//!
//! # Guards
//!
//! A suggestion is never requested (and the field's text never leaves the accessibility read)
//! when: Double Tap is off; the kill switch is engaged; the field is secure or its label or
//! placeholder names a secret (password, PIN, one-time code, card number…); the app is
//! excluded (the user's list and the accessibility deny list), a terminal (unless allowed) or
//! Heed itself; the field is a search box. Automatic suggestions additionally need the
//! automatic setting on, a precise caret (not a frame guess), an empty selection, text before the
//! caret and nothing but whitespace after it on its line. The daemon gates every request again
//! (`draft.begin`), and the draft engine's grounding, injection filter, sensitivity rules and
//! post-checks apply to every suggestion, so nothing is shown that a Double Tap draft could not.
//!
//! # The window
//!
//! One transparent, undecorated, always-on-top window (`overlay.html#ghost`) that ignores the
//! mouse and is never focused: it is shown without activating Heed (`orderFrontRegardless` on
//! macOS, a no-activate window elsewhere), so the keyboard stays in the user's app. When the
//! field's frame is known the window spans the field from the caret's line down, and the text
//! starts with an indent up to the caret, so it wraps like text typed there.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use nugget_a11y::{CaretGeometry, CaretRead, Rect};
use nugget_core::ipc::{
    ChatCancel, ChatDelta, ChatDone, DraftFeedback, DraftMode, DraftRequest, FieldInfo, method,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Listener as _, Manager, WebviewUrl, WebviewWindowBuilder};

use super::ghost::{
    Anchor, DEBOUNCE, Effect, Event, FOLLOW_EVERY, GhostView, Machine, Origin, REREAD_DELAY,
    Reading,
};
use super::placement::{CARET_UNITS, Units};
use super::settings::DoubleTapSettings;
use super::{DoubleTap, errors, flow, lock, readahead};
use crate::daemon::{self, Daemon};

/// The ghost window's label.
pub const GHOST_LABEL: &str = "ghost-text";
/// The event carrying a [`GhostPaint`] to the ghost window.
pub const GHOST_EVENT: &str = "doubletap:ghost";
/// The widest the window gets when the field's frame is unknown (logical px).
const MAX_WIDTH: f64 = 640.0;
/// Lines of room below the caret when the field's frame is unknown.
const FREE_LINES: f64 = 4.0;

/// Managed alongside [`DoubleTap`].
#[derive(Default)]
pub struct Ghost {
    machine: Mutex<Machine>,
    tap: Mutex<Option<nugget_hotkey::SuggestTap>>,
    /// Why the tap could not start (shown by the settings page), if it could not.
    pub(crate) tap_error: Mutex<Option<String>>,
    following: AtomicBool,
}

impl Ghost {
    /// Whether ghost text can be shown (the key tap runs).
    pub fn available(&self) -> bool {
        lock(&self.tap).is_some()
    }
}

/// What the ghost window renders: the view plus where and how (logical px).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct GhostPaint {
    #[serde(flatten)]
    pub view: GhostView,
    pub font_px: f64,
    pub line_px: f64,
    /// Room before the first line's text: the caret's distance from the window's start edge.
    pub indent_px: f64,
    /// The field's text colour (`#rrggbb`), when the toolkit reports it.
    pub color: Option<String>,
}

// --- setup ---------------------------------------------------------------------------------------

/// Call from `doubletap::setup` (main thread): the hidden window, the stream listeners and the
/// key tap.
pub fn setup(app: &tauri::App) {
    if let Err(e) = create_window(app) {
        tracing::warn!(error = %e, "cannot create the ghost text window");
    }
    let handle = app.handle();
    follow_streams(handle);
    restart(handle);
}

fn create_window(app: &tauri::App) -> tauri::Result<()> {
    let builder = WebviewWindowBuilder::new(
        app,
        GHOST_LABEL,
        WebviewUrl::App("overlay.html#ghost".into()),
    )
    .title("Heed suggestion")
    .inner_size(MAX_WIDTH, 40.0)
    .decorations(false)
    .resizable(false)
    .shadow(false)
    .always_on_top(true)
    .visible_on_all_workspaces(true)
    .skip_taskbar(true)
    .visible(false)
    .focused(false)
    .focusable(false)
    .transparent(true);
    let win = builder.build()?;
    let _ = win.set_ignore_cursor_events(true);
    native::prepare(&win);
    Ok(())
}

/// Start or stop the key tap for the current settings (after a change, like
/// `flow::restart_listener`). Also drops a visible suggestion.
pub fn restart(app: &AppHandle) {
    let Some(state) = app.try_state::<DoubleTap>() else {
        return;
    };
    apply(app, Event::Reset);
    let s = lock(&state.settings).clone();
    let mut slot = lock(&state.ghost.tap);
    *slot = None;
    let mut error = None;
    if s.enabled {
        let h = app.clone();
        match nugget_hotkey::start_suggest_tap(Box::new(move |k| on_key(&h, k))) {
            Ok(tap) => *slot = Some(tap),
            Err(e) => {
                tracing::info!(error = %e, "ghost text keys unavailable; Double Tap uses the panel");
                error = Some(e.to_string());
            }
        }
    }
    *lock(&state.ghost.tap_error) = error;
}

/// Runs on the key tap's dispatcher thread.
fn on_key(app: &AppHandle, key: nugget_hotkey::SuggestKey) {
    use nugget_hotkey::SuggestKey as K;
    // The kill switch stops everything Nugget does: drop a suggestion (nothing is inserted)
    // and start no new one.
    if nugget_policy::KillSwitch::global().halted() {
        return apply(app, Event::Reset);
    }
    let event = match key {
        K::Accept => Event::Accept,
        K::AcceptWord => Event::AcceptWord,
        K::Dismiss => Event::Dismiss,
        K::Typed => Event::Typed,
        K::Click => Event::Click,
    };
    apply(app, event);
}

fn follow_streams(app: &AppHandle) {
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
            // Any error, including `doubletap.error.held_back` (the safety checks removed the
            // whole answer), means "no suggestion": the machine hides quietly, with no
            // error UI over someone else's field. Sentences the checks release stream in
            // as deltas and are shown as they arrive.
            apply(
                &h,
                Event::Done {
                    stream_id: d.stream_id,
                    error: d.error.is_some(),
                },
            );
        }
    });
}

// --- the machine ---------------------------------------------------------------------------------

/// Feed an event to the machine and run its effects.
pub fn apply(app: &AppHandle, event: Event) {
    let Some(state) = app.try_state::<DoubleTap>() else {
        return;
    };
    let effects = lock(&state.ghost.machine).apply(event);
    for effect in effects {
        run(app, effect);
    }
}

fn run(app: &AppHandle, effect: Effect) {
    match effect {
        Effect::Show(view) => show(app, view),
        Effect::Hide => hide(app),
        Effect::SetTapActive(active) => {
            if let Some(tap) = lock(&app.state::<DoubleTap>().ghost.tap).as_ref() {
                tap.set_active(active);
            }
        }
        Effect::CancelStream(stream_id) => {
            flow::call_later(app, method::DRAFT_CANCEL, ChatCancel { stream_id });
        }
        Effect::Debounce(token) => {
            let h = app.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(DEBOUNCE).await;
                on_pause(h, token).await;
            });
        }
        Effect::Reread { generation } => {
            let h = app.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(REREAD_DELAY).await;
                let reading = read(&h).await.map(|r| reading_of(&r));
                apply(
                    &h,
                    Event::Reread {
                        generation,
                        reading,
                    },
                );
            });
        }
        Effect::Insert {
            generation,
            text,
            expect_before,
            whole: _,
        } => {
            tauri::async_runtime::spawn(insert(app.clone(), generation, text, expect_before));
        }
        Effect::Accepted { stream_id } => flow::call_later(
            app,
            method::DRAFT_FEEDBACK,
            DraftFeedback {
                stream_id,
                outcome: "accepted".into(),
                final_text: None,
                insert_verified: None,
            },
        ),
    }
}

// --- reads and guards ----------------------------------------------------------------------------

/// The cheap field read (no thread walk), off the async workers.
async fn read(app: &AppHandle) -> Option<CaretRead> {
    let a11y = app.state::<DoubleTap>().a11y.clone();
    tauri::async_runtime::spawn_blocking(move || a11y.caret())
        .await
        .ok()?
        .map_err(|e| tracing::debug!(error = %e, "ghost: caret read failed"))
        .ok()
        .flatten()
}

fn reading_of(r: &CaretRead) -> Reading {
    Reading {
        before: before_caret(&r.field).unwrap_or_default(),
        field: flow::FieldSig::from_parts(&r.field, r.geometry.map(|g| g.caret)),
        app_key: readahead::app_key(&r.app),
        geometry: r.geometry,
    }
}

/// The text before the caret, when the caret is known and nothing is selected.
pub fn before_caret(field: &FieldInfo) -> Option<String> {
    match field.selection {
        Some((a, b)) if a == b => Some(field.value.chars().take(a as usize).collect()),
        Some(_) => None,
        None if field.value.is_empty() => Some(String::new()),
        None => None,
    }
}

/// The rest of the caret's line after it.
fn after_caret_on_line(field: &FieldInfo) -> String {
    let Some((_, b)) = field.selection else {
        return String::new();
    };
    field
        .value
        .chars()
        .skip(b as usize)
        .take_while(|c| *c != '\n' && *c != '\r')
        .collect()
}

/// Payment and bank fields, which the secure-label list (passwords, PINs, one-time codes,
/// CVV) does not cover: a suggestion there could only be a guess at a number.
const PAYMENT_LABELS: &[&str] = &[
    "card number",
    "credit card",
    "debit card",
    "cc number",
    "cardholder",
    "expiry",
    "expiration",
    "security code",
    "iban",
    "account number",
    "routing number",
    "sort code",
    "swift",
];

fn names_payment(label: &str) -> bool {
    let l = label.to_lowercase();
    PAYMENT_LABELS.iter().any(|k| l.contains(k))
}

/// Why no suggestion may be shown in this field, if one may not. `own`: the field is in a
/// Heed window.
pub fn refusal(
    r: &CaretRead,
    s: &DoubleTapSettings,
    origin: Origin,
    own: bool,
) -> Option<&'static str> {
    let f = &r.field;
    let names_secret = |v: &Option<String>| {
        v.as_deref()
            .is_some_and(|l| nugget_policy::redact::is_secure_label(l) || names_payment(l))
    };
    if !s.enabled {
        return Some("disabled");
    }
    if nugget_policy::KillSwitch::global().halted() {
        return Some("halted");
    }
    if f.secure || names_secret(&f.label) || names_secret(&f.placeholder) {
        return Some(errors::REFUSED_SECURE);
    }
    if r.denied {
        return Some(errors::REFUSED_EXCLUDED);
    }
    if let Some(k) = flow::local_refusal(&r.app, s) {
        return Some(k);
    }
    if own || f.role == "search_field" {
        return Some(errors::REFUSED_NO_FIELD);
    }
    let Some(g) = r.geometry else {
        return Some("no_caret");
    };
    let before = before_caret(f);
    match origin {
        Origin::Auto => {
            if !s.auto_suggest {
                return Some("auto_off");
            }
            if !g.precise() {
                return Some("no_caret");
            }
            match before {
                None => return Some("selection"),
                Some(b) if b.trim().is_empty() => return Some("empty"),
                Some(_) => {}
            }
            if !after_caret_on_line(f).trim().is_empty() {
                return Some("mid_line");
            }
        }
        Origin::DoubleTap => {
            // A frame guess only places text right in an empty field.
            if before.is_none() || (!g.precise() && !f.value.is_empty()) {
                return Some("no_caret");
            }
        }
    }
    None
}

// --- requests ------------------------------------------------------------------------------------

/// The typing pause `token` ran out: request an automatic suggestion if it is still the latest
/// pause and every guard passes.
async fn on_pause(app: AppHandle, token: u64) {
    let state = app.state::<DoubleTap>();
    let fresh = {
        let m = lock(&state.ghost.machine);
        m.idle() && m.pause_token() == token
    };
    if !fresh || !lock(&state.settings).auto_suggest || lock(&state.machine).busy() {
        return;
    }
    let Some(r) = read(&app).await else { return };
    let s = lock(&state.settings).clone();
    if refusal(&r, &s, Origin::Auto, readahead::is_own(&app, &r.app)).is_some() {
        return;
    }
    let before = before_caret(&r.field).unwrap_or_default();
    {
        let m = lock(&state.ghost.machine);
        if !m.idle() || m.pause_token() != token || !m.auto_allowed(&before) {
            return;
        }
    }
    request(&app, Origin::Auto, r, &s).await;
}

/// Double Tap: show the draft as ghost text when the caret can be placed. `false` when it can't
/// (no key tap, the practice field, no caret, a refused field): the caller opens the panel,
/// which explains refusals and works without caret geometry.
pub async fn on_demand(app: &AppHandle) -> bool {
    let state = app.state::<DoubleTap>();
    if !state.ghost.available()
        || flow::practice_target(app).is_some()
        || lock(&state.machine).busy()
    {
        return false;
    }
    let Some(r) = read(app).await else {
        return false;
    };
    let s = lock(&state.settings).clone();
    if refusal(&r, &s, Origin::DoubleTap, readahead::is_own(app, &r.app)).is_some() {
        return false;
    }
    request(app, Origin::DoubleTap, r, &s).await;
    true
}

async fn request(app: &AppHandle, origin: Origin, r: CaretRead, s: &DoubleTapSettings) {
    let Some(geometry) = r.geometry else { return };
    let before = before_caret(&r.field).unwrap_or_default();
    let anchor = Anchor {
        rtl: first_strong_rtl(&before),
        before,
        field: flow::FieldSig::from_parts(&r.field, Some(geometry.caret)),
        app_key: readahead::app_key(&r.app),
        geometry,
    };
    let (fx, generation) = {
        let state = app.state::<DoubleTap>();
        let mut m = lock(&state.ghost.machine);
        (m.apply(Event::Request { origin, anchor }), m.generation())
    };
    for e in fx {
        run(app, e);
    }
    // The full read (the thread around the field) for the prompt.
    let a11y = app.state::<DoubleTap>().a11y.clone();
    let focused = tauri::async_runtime::spawn_blocking(move || a11y.api().focused())
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    let Some(focused) = focused.filter(|f| !f.field.secure) else {
        return apply(app, Event::Failed { generation });
    };
    // Focus moved during the read: the anchor no longer describes this field.
    if readahead::app_key(&focused.app) != readahead::app_key(&r.app) {
        return apply(app, Event::Failed { generation });
    }
    let mut req = flow::draft_request(focused, s);
    if origin == Origin::Auto {
        req.mode = DraftMode::Continue;
    }
    match begin(app, &req).await {
        Some(stream_id) => apply(
            app,
            Event::Started {
                generation,
                stream_id,
            },
        ),
        None => apply(app, Event::Failed { generation }),
    }
}

async fn begin(app: &AppHandle, req: &DraftRequest) -> Option<String> {
    let daemon = app.state::<Daemon>();
    let client = daemon.client(app).await.ok()?;
    match client
        .call::<_, nugget_core::ipc::DraftStarted>(method::DRAFT_BEGIN, req)
        .await
    {
        Ok(s) => Some(s.stream_id),
        Err(e) => {
            tracing::debug!(error = %e, "ghost: draft.begin refused or failed");
            None
        }
    }
}

fn first_strong_rtl(text: &str) -> bool {
    text.chars().find(|c| c.is_alphabetic()).is_some_and(|c| {
        matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF)
    })
}

// --- insertion -----------------------------------------------------------------------------------

async fn insert(app: AppHandle, generation: u64, text: String, expect_before: String) {
    let state = app.state::<DoubleTap>();
    let anchor = lock(&state.ghost.machine).anchor().cloned();
    let a11y = state.a11y.clone();
    let ok = tauri::async_runtime::spawn_blocking(move || {
        // Only into the field the suggestion was written for, at the place it was shown.
        let now = a11y.caret().ok().flatten()?;
        let before = before_caret(&now.field)?;
        if now.field.secure || now.denied || before != expect_before {
            return None;
        }
        if let Some(a) = &anchor {
            let sig = flow::FieldSig::from_parts(&now.field, now.geometry.map(|g| g.caret));
            if readahead::app_key(&now.app) != a.app_key || !a.field.matches(&sig) {
                return None;
            }
        }
        a11y.api()
            .insert(&text)
            .map_err(|e| tracing::debug!(error = %e, "ghost: insert failed"))
            .ok()
    })
    .await
    .ok()
    .flatten();
    apply(
        &app,
        Event::Inserted {
            generation,
            ok: ok.is_some(),
        },
    );
}

// --- the window ----------------------------------------------------------------------------------

fn show(app: &AppHandle, view: GhostView) {
    let Some(win) = app.get_webview_window(GHOST_LABEL) else {
        return;
    };
    let Some(anchor) = lock(&app.state::<DoubleTap>().ghost.machine)
        .anchor()
        .cloned()
    else {
        return;
    };
    let monitors: Vec<Monitor> = win
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| Monitor {
            x: f64::from(m.position().x),
            y: f64::from(m.position().y),
            width: f64::from(m.size().width),
            height: f64::from(m.size().height),
            scale: m.scale_factor(),
        })
        .collect();
    let layout = layout(&anchor.geometry, anchor.rtl, CARET_UNITS, &monitors);
    let paint = GhostPaint {
        view,
        font_px: layout.font_px,
        line_px: layout.line_px,
        indent_px: layout.indent_px,
        color: anchor.geometry.style.color.map(|c| c.to_css()),
    };
    let _ = win.emit_to(GHOST_LABEL, GHOST_EVENT, &paint);
    let _ = win.set_size(tauri::PhysicalSize::new(
        (layout.width * layout.scale).round() as u32,
        (layout.height * layout.scale).round() as u32,
    ));
    let _ = win.set_position(tauri::PhysicalPosition::new(
        (layout.x * layout.scale).round() as i32,
        (layout.y * layout.scale).round() as i32,
    ));
    native::show(&win);
    start_following(app);
}

fn hide(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(GHOST_LABEL) {
        let _ = win.emit_to(GHOST_LABEL, GHOST_EVENT, &GhostPaint::default());
        native::hide(&win);
    }
}

/// While a suggestion is visible, read the field every [`FOLLOW_EVERY`]: the window follows the
/// caret when the field scrolls or moves, and goes away when focus leaves the field.
fn start_following(app: &AppHandle) {
    let state = app.state::<DoubleTap>();
    if state.ghost.following.swap(true, Ordering::AcqRel) {
        return;
    }
    let h = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(FOLLOW_EVERY).await;
            let state = h.state::<DoubleTap>();
            let (visible, generation) = {
                let m = lock(&state.ghost.machine);
                (m.visible(), m.generation())
            };
            if !visible {
                break;
            }
            let reading = read(&h).await.map(|r| reading_of(&r));
            apply(
                &h,
                Event::Reread {
                    generation,
                    reading,
                },
            );
        }
        h.state::<DoubleTap>()
            .ghost
            .following
            .store(false, Ordering::Release);
    });
}

/// A monitor in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Monitor {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

/// Where the window goes and how its text is sized, in logical pixels of the monitor holding the
/// caret (`scale` converts to physical).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub indent_px: f64,
    pub font_px: f64,
    pub line_px: f64,
    pub scale: f64,
}

/// Lay the window out over the caret's line (pure).
pub fn layout(g: &CaretGeometry, rtl: bool, units: Units, monitors: &[Monitor]) -> Layout {
    // The monitor holding the caret, in the caret's units.
    let in_units = |m: &Monitor| match units {
        Units::Physical => (m.x, m.y, m.width, m.height),
        Units::Logical => (
            m.x / m.scale,
            m.y / m.scale,
            m.width / m.scale,
            m.height / m.scale,
        ),
    };
    let c = g.caret;
    let monitor = monitors
        .iter()
        .find(|m| {
            let (x, y, w, h) = in_units(m);
            c.x >= x && c.x < x + w && c.y >= y && c.y < y + h
        })
        .or_else(|| monitors.first())
        .copied()
        .unwrap_or(Monitor {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
            scale: 1.0,
        });
    let scale = if monitor.scale.is_finite() && monitor.scale > 0.0 {
        monitor.scale
    } else {
        1.0
    };
    // Everything to logical pixels.
    let k = match units {
        Units::Physical => 1.0 / scale,
        Units::Logical => 1.0,
    };
    let lg = |r: Rect| Rect {
        x: r.x * k,
        y: r.y * k,
        width: r.width * k,
        height: r.height * k,
    };
    let caret = lg(c);
    let (mx, my, mw, mh) = {
        let (x, y, w, h) = in_units(&monitor);
        (x * k, y * k, w * k, h * k)
    };
    // Points: macOS AX reports them in logical px already; elsewhere 1 pt = 4/3 px.
    let pt = match units {
        Units::Logical => 1.0,
        Units::Physical => 4.0 / 3.0,
    };
    let font_px = g.style.font_size.map_or_else(
        || nugget_a11y::geometry::estimate_font_size(caret.height),
        |s| s * pt,
    );
    let line_px = caret.height.max(font_px * 1.15);
    let right_edge = mx + mw;
    let caret_x = if rtl { caret.x } else { caret.x + caret.width };
    let (x, width, indent_px) = match g.field.map(lg) {
        Some(f) if f.width > 4.0 && caret_x >= f.x - 1.0 && caret_x <= f.x + f.width + 1.0 => {
            let indent = if rtl {
                (f.x + f.width - caret_x).max(0.0)
            } else {
                (caret_x - f.x).max(0.0)
            };
            (f.x, f.width, indent)
        }
        _ if rtl => {
            let x = (caret_x - MAX_WIDTH).max(mx);
            (x, caret_x - x, 0.0)
        }
        _ => (
            caret_x,
            MAX_WIDTH.min(right_edge - caret_x).max(line_px),
            0.0,
        ),
    };
    let bottom = g
        .field
        .map(lg)
        .map_or(caret.y + line_px * FREE_LINES, |f| {
            (f.y + f.height).max(caret.y + line_px)
        })
        .min(my + mh);
    let height = (bottom - caret.y).max(line_px).min(line_px * 12.0);
    Layout {
        x,
        y: caret.y,
        width: width.max(1.0),
        height,
        indent_px,
        font_px,
        line_px,
        scale,
    }
}

// --- native window behaviour ---------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod native {
    //! A plain NSWindow made to behave like a non-activating panel: it ignores the mouse, floats
    //! above normal windows on every Space (full-screen apps included), and is ordered front
    //! with `orderFrontRegardless`, which never makes it key or activates Heed. That gives the
    //! nonactivatingPanel behaviour without swapping the class of Tauri's own window subclass at
    //! runtime.

    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSColor, NSWindow, NSWindowCollectionBehavior};

    /// `NSStatusWindowLevel`: above normal and floating windows, below the menu bar's menus.
    const LEVEL: isize = 25;

    fn with_window(win: &tauri::WebviewWindow, f: impl FnOnce(&NSWindow) + Send + 'static) {
        let Ok(ptr) = win.ns_window() else { return };
        let ptr = ptr as usize;
        let _ = win.run_on_main_thread(move || {
            if MainThreadMarker::new().is_none() || ptr == 0 {
                return;
            }
            // SAFETY: Tauri's NSWindow for this webview window, alive while the window exists
            // (it is never destroyed: closing is prevented), used on the main thread only.
            #[allow(unsafe_code)]
            let w = unsafe { &*(ptr as *const NSWindow) };
            f(w);
        });
    }

    pub fn prepare(win: &tauri::WebviewWindow) {
        with_window(win, |w| {
            w.setIgnoresMouseEvents(true);
            w.setLevel(LEVEL);
            w.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::Stationary
                    | NSWindowCollectionBehavior::FullScreenAuxiliary
                    | NSWindowCollectionBehavior::IgnoresCycle,
            );
            w.setHasShadow(false);
            w.setOpaque(false);
            w.setBackgroundColor(Some(&NSColor::clearColor()));
            w.setHidesOnDeactivate(false);
        });
    }

    pub fn show(win: &tauri::WebviewWindow) {
        with_window(win, |w| {
            w.setIgnoresMouseEvents(true);
            w.orderFrontRegardless();
        });
    }

    pub fn hide(win: &tauri::WebviewWindow) {
        with_window(win, |w| w.orderOut(None));
    }
}

#[cfg(not(target_os = "macos"))]
mod native {
    //! Windows and Linux: Tauri's no-focus window (`focusable(false)`, which is a
    //! no-activate window on Windows) with cursor events ignored, so clicks go through to the
    //! app below.

    pub fn prepare(win: &tauri::WebviewWindow) {
        let _ = win.set_ignore_cursor_events(true);
    }

    pub fn show(win: &tauri::WebviewWindow) {
        let _ = win.set_always_on_top(true);
        let _ = win.show();
    }

    pub fn hide(win: &tauri::WebviewWindow) {
        let _ = win.hide();
    }
}

/// The view for the settings page: whether ghost text works here, and why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GhostStatus {
    pub available: bool,
    pub error: Option<String>,
}

pub fn status(app: &AppHandle) -> GhostStatus {
    let state = app.state::<DoubleTap>();
    GhostStatus {
        available: state.ghost.available(),
        error: lock(&state.ghost.tap_error).clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nugget_a11y::{CaretSource, TextStyle};
    use nugget_core::ipc::AppInfo;

    fn field(value: &str, caret: u32) -> FieldInfo {
        FieldInfo {
            role: "text_area".into(),
            label: Some("Message".into()),
            value: value.into(),
            selection: Some((caret, caret)),
            ..Default::default()
        }
    }

    fn geometry(source: CaretSource) -> CaretGeometry {
        CaretGeometry {
            caret: Rect {
                x: 200.0,
                y: 300.0,
                width: 1.0,
                height: 18.0,
            },
            source,
            field: Some(Rect {
                x: 100.0,
                y: 290.0,
                width: 500.0,
                height: 80.0,
            }),
            style: TextStyle::default(),
        }
    }

    fn read(f: FieldInfo, app: &str) -> CaretRead {
        CaretRead {
            app: AppInfo {
                id: Some(app.into()),
                name: app.into(),
                window_title: None,
                url: None,
            },
            field: f,
            geometry: Some(geometry(CaretSource::Range)),
            denied: false,
        }
    }

    const SLACK: &str = "com.tinyspeck.slackmacgap";

    #[test]
    fn caret_text() {
        assert_eq!(before_caret(&field("héllo world", 5)).unwrap(), "héllo");
        let mut f = field("abc", 1);
        f.selection = Some((1, 2));
        assert_eq!(before_caret(&f), None);
        f.selection = None;
        assert_eq!(before_caret(&f), None);
        f.value.clear();
        assert_eq!(before_caret(&f).unwrap(), "");
        assert_eq!(after_caret_on_line(&field("ab cd\nef", 2)), " cd");
    }

    #[test]
    fn auto_guards() {
        let s = DoubleTapSettings::default();
        let ok = read(field("Sure, I'll send", 15), SLACK);
        assert_eq!(refusal(&ok, &s, Origin::Auto, false), None);
        // Secure fields, labels naming a secret, denied and excluded apps, terminals, Nugget.
        let mut r = ok.clone();
        r.field.secure = true;
        assert_eq!(
            refusal(&r, &s, Origin::Auto, false),
            Some(errors::REFUSED_SECURE)
        );
        for label in [
            "One-time code",
            "Card number",
            "Password",
            "IBAN",
            "Expiry (MM/YY)",
        ] {
            let mut r = ok.clone();
            r.field.label = Some(label.into());
            assert_eq!(
                refusal(&r, &s, Origin::DoubleTap, false),
                Some(errors::REFUSED_SECURE),
                "{label}"
            );
        }
        let mut r = ok.clone();
        r.field.label = None;
        r.field.placeholder = Some("Enter your verification code".into());
        assert_eq!(
            refusal(&r, &s, Origin::Auto, false),
            Some(errors::REFUSED_SECURE)
        );
        let mut r = ok.clone();
        r.denied = true;
        assert_eq!(
            refusal(&r, &s, Origin::Auto, false),
            Some(errors::REFUSED_EXCLUDED)
        );
        let r = read(field("ls -la", 6), "com.apple.Terminal");
        assert_eq!(
            refusal(&r, &s, Origin::Auto, false),
            Some(errors::REFUSED_TERMINAL)
        );
        let r = read(field("x y", 3), "com.bitwarden.desktop");
        assert_eq!(
            refusal(&r, &s, Origin::Auto, false),
            Some(errors::REFUSED_EXCLUDED)
        );
        assert_eq!(
            refusal(&ok, &s, Origin::Auto, true),
            Some(errors::REFUSED_NO_FIELD)
        );
        let mut r = ok.clone();
        r.field.role = "search_field".into();
        assert_eq!(
            refusal(&r, &s, Origin::Auto, false),
            Some(errors::REFUSED_NO_FIELD)
        );
        // The settings.
        let off = DoubleTapSettings {
            auto_suggest: false,
            ..s.clone()
        };
        assert_eq!(refusal(&ok, &off, Origin::Auto, false), Some("auto_off"));
        assert_eq!(refusal(&ok, &off, Origin::DoubleTap, false), None);
        let disabled = DoubleTapSettings {
            enabled: false,
            ..s.clone()
        };
        assert_eq!(
            refusal(&ok, &disabled, Origin::DoubleTap, false),
            Some("disabled")
        );
        // Where automatic suggestions would land badly.
        let mut r = ok.clone();
        r.geometry = Some(geometry(CaretSource::Frame));
        assert_eq!(refusal(&r, &s, Origin::Auto, false), Some("no_caret"));
        assert_eq!(refusal(&r, &s, Origin::DoubleTap, false), Some("no_caret"));
        let mut empty = r.clone();
        empty.field = field("", 0);
        assert_eq!(
            refusal(&empty, &s, Origin::DoubleTap, false),
            None,
            "frame is right when empty"
        );
        assert_eq!(refusal(&empty, &s, Origin::Auto, false), Some("no_caret"));
        let r = read(field("Thanks for the", 4), SLACK);
        assert_eq!(refusal(&r, &s, Origin::Auto, false), Some("mid_line"));
        let r = read(field("line one\nsecond", 8), SLACK);
        assert_eq!(refusal(&r, &s, Origin::Auto, false), None, "end of a line");
        let mut r = ok.clone();
        r.geometry = None;
        assert_eq!(refusal(&r, &s, Origin::DoubleTap, false), Some("no_caret"));
    }

    fn mon(scale: f64) -> Monitor {
        Monitor {
            x: 0.0,
            y: 0.0,
            width: 2880.0,
            height: 1800.0,
            scale,
        }
    }

    #[test]
    fn layout_spans_the_field_with_an_indent_to_the_caret() {
        let mut g = geometry(CaretSource::Range);
        g.style.font_size = Some(13.0);
        let l = layout(&g, false, Units::Logical, &[mon(2.0)]);
        assert_eq!((l.x, l.y, l.width), (100.0, 300.0, 500.0));
        assert_eq!(l.indent_px, 101.0);
        assert_eq!(l.font_px, 13.0);
        assert_eq!(l.line_px, 18.0);
        assert_eq!(l.height, 70.0, "down to the field's bottom");
        assert_eq!(l.scale, 2.0);
        // Right to left: the indent is from the field's right edge to the caret.
        let r = layout(&g, true, Units::Logical, &[mon(2.0)]);
        assert_eq!(r.indent_px, 400.0);
    }

    #[test]
    fn layout_converts_physical_caret_units_and_points() {
        let mut g = geometry(CaretSource::Range);
        g.caret = Rect {
            x: 300.0,
            y: 450.0,
            width: 2.0,
            height: 30.0,
        };
        g.field = None;
        g.style.font_size = Some(9.0);
        let l = layout(&g, false, Units::Physical, &[mon(1.5)]);
        assert!((l.x - 302.0 / 1.5).abs() < 1e-9, "{}", l.x);
        assert_eq!(l.y, 300.0);
        assert_eq!(l.font_px, 12.0, "9 pt is 12 px");
        assert_eq!(l.line_px, 20.0);
        assert_eq!(l.indent_px, 0.0);
        assert_eq!(l.height, 80.0, "a few free lines");
        assert!(l.width <= MAX_WIDTH);
        // Without a size the font is estimated from the line height.
        g.style.font_size = None;
        let l = layout(&g, false, Units::Physical, &[mon(1.5)]);
        assert_eq!(l.font_px, 16.0);
    }
}
