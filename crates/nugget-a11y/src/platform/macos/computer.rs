//! Computer use on macOS (M4): apps and windows, element actions, key input and app launch.
//!
//! - Apps: the window server's list (`CGWindowListCopyWindowInfo`, layer 0) gives the processes
//!   with normal windows, front to back. It is live without a run loop, unlike
//!   `NSWorkspace.runningApplications`. `NSRunningApplication` adds the name and bundle id, and
//!   only regular (Dock) apps are kept. The frontmost app is AX `AXFocusedApplication`.
//! - Windows: `AXWindows` of each app. The native id is the CGWindowID matched by pid and frame
//!   (`cg:<id>`), else `ax:<pid>:<index>`.
//! - Actions: `AXPress` (or `AXPick`/`AXConfirm`/`AXOpen`), settable `AXValue`, `AXSelected`
//!   (or the parent's `AXSelectedRows`), `AXFocused`, `AXExpanded`/`AXDisclosing`,
//!   `AXShowMenu`/`AXCancel` on pop-ups and menus, `AXScrollToVisible`, and a line-unit scroll
//!   wheel event at the element's centre.
//! - Keys: CGEvents posted at the HID tap. Text goes as Unicode strings with no modifier flags,
//!   so it is never read as a shortcut; named keys and chords use ANSI virtual key codes.
//! - Launch: activate a running app; otherwise `/usr/bin/open -b <bundle id>` or `-a <name>`,
//!   without a shell.

use std::process::{Command, Stdio};
use std::time::Duration;

use nugget_core::{Error, Result};
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplicationActivationOptions, NSApplicationActivationPolicy, NSRunningApplication,
};
use objc2_application_services::{AXError, AXUIElement};
use objc2_core_foundation::{CFArray, CFBoolean, CFNumber, CFString, CFType, CGPoint};
use objc2_core_graphics::{
    CGEvent, CGEventFlags, CGEventTapLocation, CGScrollEventUnit, CGWindowListCopyWindowInfo,
    CGWindowListOption,
};

use super::{AxEl, ax, cf_bool, cf_string};
use crate::Rect;
use crate::computer::{
    AppEntry, Key, KeyChord, KeyInput, Modifiers, NamedKey, ScrollDirection, UiAction, UiActionKind,
};
use crate::engine::PlatformWindow;
use crate::tree::Element;

/// Most lines one scroll action moves.
const MAX_SCROLL_LINES: u32 = 100;
/// `CGEventKeyboardSetUnicodeString` reads at most this many UTF-16 units.
const UNICODE_CHUNK: usize = 20;

fn ax_err(what: &str, e: AXError) -> Error {
    Error::Other(format!("AX {what} failed: {e:?}"))
}

pub(super) fn require_trust() -> Result<()> {
    if ax::is_trusted(false) {
        Ok(())
    } else {
        Err(Error::Unauthorized(
            "Accessibility permission is not granted".into(),
        ))
    }
}

// --- Apps and windows ---

struct CgWindow {
    number: u32,
    pid: i32,
    bounds: Rect,
}

fn cf_i64(t: &CFType) -> Option<i64> {
    t.downcast_ref::<CFNumber>().and_then(CFNumber::as_i64)
}

fn cf_f64(t: &CFType) -> Option<f64> {
    t.downcast_ref::<CFNumber>().and_then(CFNumber::as_f64)
}

/// Normal (layer 0) windows of every process, front to back. Titles are not read (they need
/// the screen recording permission); only ids, owners and frames.
fn cg_windows() -> Vec<CgWindow> {
    let opts = CGWindowListOption::OptionAll | CGWindowListOption::ExcludeDesktopElements;
    let Some(list) = CGWindowListCopyWindowInfo(opts, 0) else {
        return Vec::new();
    };
    ax::array_items(&list, 4096)
        .iter()
        .filter_map(|d| {
            let num = |k: &str| ax::dict_get(d, k).as_deref().and_then(cf_i64);
            if num("kCGWindowLayer")? != 0 {
                return None;
            }
            let b = ax::dict_get(d, "kCGWindowBounds")?;
            let f = |k: &str| ax::dict_get(&b, k).as_deref().and_then(cf_f64);
            Some(CgWindow {
                number: u32::try_from(num("kCGWindowNumber")?).ok()?,
                pid: i32::try_from(num("kCGWindowOwnerPID")?).ok()?,
                bounds: Rect {
                    x: f("X")?,
                    y: f("Y")?,
                    width: f("Width")?,
                    height: f("Height")?,
                },
            })
        })
        .collect()
}

fn frontmost_pid() -> Option<i32> {
    let sys = ax::system_wide();
    let app = ax::attr(&sys, "AXFocusedApplication")?
        .downcast::<AXUIElement>()
        .ok()?;
    ax::pid(&app)
}

struct RunningApp {
    entry: AppEntry,
    running: Retained<NSRunningApplication>,
}

/// Regular apps with at least one normal window, frontmost first, then front to back.
fn running_apps(cg: &[CgWindow]) -> Vec<RunningApp> {
    let front = frontmost_pid();
    let mut seen: Vec<i32> = Vec::new();
    let mut out = Vec::new();
    for w in cg {
        if seen.contains(&w.pid) {
            continue;
        }
        seen.push(w.pid);
        let Some(r) = NSRunningApplication::runningApplicationWithProcessIdentifier(w.pid) else {
            continue;
        };
        if r.isTerminated() || r.activationPolicy() != NSApplicationActivationPolicy::Regular {
            continue;
        }
        out.push(RunningApp {
            entry: AppEntry {
                id: r.bundleIdentifier().map(|s| s.to_string()),
                name: r.localizedName().map(|s| s.to_string()).unwrap_or_default(),
                pid: u32::try_from(w.pid).ok(),
                frontmost: Some(w.pid) == front,
            },
            running: r,
        });
    }
    out.sort_by_key(|a| !a.entry.frontmost);
    out
}

pub(super) fn apps() -> Result<Vec<AppEntry>> {
    Ok(running_apps(&cg_windows())
        .into_iter()
        .map(|a| a.entry)
        .collect())
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1.0
}

/// The first unused window-server window of `pid` with the same frame.
fn match_cg(cg: &[CgWindow], used: &mut [bool], pid: i32, b: Option<Rect>) -> Option<u32> {
    let b = b?;
    let i = cg.iter().enumerate().position(|(i, w)| {
        !used[i]
            && w.pid == pid
            && near(w.bounds.x, b.x)
            && near(w.bounds.y, b.y)
            && near(w.bounds.width, b.width)
            && near(w.bounds.height, b.height)
    })?;
    used[i] = true;
    Some(cg[i].number)
}

pub(super) fn windows() -> Result<Vec<PlatformWindow<AxEl>>> {
    require_trust()?;
    // Sets the messaging timeout for every element of this process.
    let _ = ax::system_wide();
    let cg = cg_windows();
    let mut used = vec![false; cg.len()];
    let mut out = Vec::new();
    for app in running_apps(&cg) {
        let Some(pid) = app.entry.pid.and_then(|p| i32::try_from(p).ok()) else {
            continue;
        };
        let ax_app = AxEl(ax::application(pid));
        let focused_win = if app.entry.frontmost {
            ax_app.get("AXFocusedWindow").and_then(AxEl::element)
        } else {
            None
        };
        let wins = ax::attr_values(&ax_app.0, "AXWindows", 64)
            .into_iter()
            .filter_map(AxEl::element);
        for (i, w) in wins.enumerate() {
            let v = ax::attrs(
                &w.0,
                &["AXRole", "AXTitle", "AXPosition", "AXSize", "AXMinimized"],
            );
            if v[0].as_deref().and_then(cf_string).as_deref() != Some("AXWindow") {
                continue;
            }
            let bounds = match (
                v[2].as_deref().and_then(ax::as_point),
                v[3].as_deref().and_then(ax::as_size),
            ) {
                (Some(p), Some(s)) if s.width > 0.0 && s.height > 0.0 => Some(Rect {
                    x: p.x,
                    y: p.y,
                    width: s.width,
                    height: s.height,
                }),
                _ => None,
            };
            let native_id = match match_cg(&cg, &mut used, pid, bounds) {
                Some(n) => format!("cg:{n}"),
                None => format!("ax:{pid}:{i}"),
            };
            out.push(PlatformWindow {
                app: app.entry.clone(),
                native_id,
                title: v[1]
                    .as_deref()
                    .and_then(cf_string)
                    .filter(|t| !t.is_empty()),
                bounds,
                focused: focused_win.as_ref().is_some_and(|f| f.same(&w)),
                minimized: v[4].as_deref().and_then(cf_bool).unwrap_or(false),
                element: w,
            });
        }
    }
    out.sort_by_key(|w| !w.focused);
    Ok(out)
}

// --- Element actions ---

/// AX actions that press, in order of preference.
const PRESS_ACTIONS: &[&str] = &["AXPress", "AXPick", "AXConfirm", "AXOpen"];
/// Roles whose `AXValue` may be settable (secure fields are excluded separately).
const VALUE_ROLES: &[&str] = &[
    "AXTextField",
    "AXTextArea",
    "AXComboBox",
    // A web `<select>` (Chromium) takes its option's text as the value.
    "AXPopUpButton",
    "AXSlider",
    "AXIncrementor",
    "AXDateField",
    "AXColorWell",
];
/// Roles that never take keyboard focus (skips an IPC round trip per node).
const UNFOCUSABLE_ROLES: &[&str] = &[
    "AXStaticText",
    "AXImage",
    "AXGroup",
    "AXUnknown",
    "AXSplitter",
    "AXApplication",
    "AXLayoutArea",
    "AXLayoutItem",
    "AXValueIndicator",
    "AXMenu",
    "AXMenuItem",
    "AXMenuBar",
    "AXMenuBarItem",
];
/// Roles that scroll their content.
const SCROLL_ROLES: &[&str] = &[
    "AXScrollArea",
    "AXTable",
    "AXOutline",
    "AXList",
    "AXBrowser",
    "AXWebArea",
    "AXTextArea",
];
/// Roles where `AXShowMenu` opens the element's own menu rather than a context menu.
const MENU_ROLES: &[&str] = &[
    "AXPopUpButton",
    "AXComboBox",
    "AXMenuButton",
    "AXMenuBarItem",
    "AXMenuItem",
];

/// Which attributes of an element are settable, as far as action advertising needs.
#[derive(Debug, Default, Clone, Copy)]
struct Settable {
    value: bool,
    selected: bool,
    /// The parent table or outline takes `AXSelectedRows`.
    parent_rows: bool,
    focused: bool,
    /// `AXExpanded` or `AXDisclosing`.
    expanded: bool,
}

/// The action kinds an element supports, from its AX role, action names and settable
/// attributes.
fn kinds_from_ax(role: &str, actions: &[String], s: Settable) -> Vec<UiActionKind> {
    let has = |a: &str| actions.iter().any(|x| x == a);
    let mut k = Vec::new();
    if PRESS_ACTIONS.iter().any(|a| has(a)) {
        k.push(UiActionKind::Press);
    }
    if s.value {
        k.push(UiActionKind::SetValue);
    }
    if s.selected || s.parent_rows {
        k.push(UiActionKind::Select);
    }
    if s.focused {
        k.push(UiActionKind::Focus);
    }
    if has("AXScrollToVisible") || SCROLL_ROLES.contains(&role) {
        k.push(UiActionKind::Scroll);
    }
    if s.expanded
        || role == "AXDisclosureTriangle"
        || (has("AXShowMenu") && MENU_ROLES.contains(&role))
    {
        k.push(UiActionKind::Expand);
    }
    k
}

/// Checked state from a check box, radio button or toggle's numeric `AXValue` (0 off, 1 on,
/// 2 mixed, which counts as not checked).
pub(super) fn checked_from_ax(role: &str, subrole: Option<&str>, value: &CFType) -> Option<bool> {
    let checkable = matches!(role, "AXCheckBox" | "AXRadioButton" | "AXMenuItemCheckbox")
        || matches!(subrole, Some("AXSwitch" | "AXToggle"));
    if !checkable {
        return None;
    }
    cf_i64(value).map(|v| v == 1)
}

pub(super) fn actions(el: &AxEl) -> Vec<UiActionKind> {
    let (raw, sub) = el.role_subrole();
    let secure = sub.as_deref() == Some("AXSecureTextField");
    let names = ax::action_names(&el.0);
    let present = ax::attrs(&el.0, &["AXSelected", "AXExpanded", "AXDisclosing"]);
    let mut s = Settable::default();
    if !secure && VALUE_ROLES.contains(&raw.as_str()) {
        s.value = ax::is_settable(&el.0, "AXValue");
    }
    if present[0].is_some() {
        s.selected = ax::is_settable(&el.0, "AXSelected");
    }
    if !s.selected && raw == "AXRow" {
        s.parent_rows = el
            .parent()
            .is_some_and(|p| ax::is_settable(&p.0, "AXSelectedRows"));
    }
    if !UNFOCUSABLE_ROLES.contains(&raw.as_str()) {
        s.focused = ax::is_settable(&el.0, "AXFocused");
    }
    s.expanded = (present[1].is_some() && ax::is_settable(&el.0, "AXExpanded"))
        || (present[2].is_some() && ax::is_settable(&el.0, "AXDisclosing"));
    kinds_from_ax(&raw, &names, s)
}

fn set_bool(el: &AxEl, attr: &str, v: bool) -> Result<()> {
    ax::set_attr(&el.0, attr, CFBoolean::new(v)).map_err(|e| ax_err(&format!("set {attr}"), e))
}

fn perform_named(el: &AxEl, action: &str) -> Result<()> {
    ax::perform_action(&el.0, action).map_err(|e| ax_err(action, e))
}

fn element_bounds(el: &AxEl) -> Option<Rect> {
    let v = ax::attrs(&el.0, &["AXPosition", "AXSize"]);
    let p = v[0].as_deref().and_then(ax::as_point)?;
    let s = v[1].as_deref().and_then(ax::as_size)?;
    (s.width > 0.0 && s.height > 0.0).then_some(Rect {
        x: p.x,
        y: p.y,
        width: s.width,
        height: s.height,
    })
}

/// Line deltas `(vertical, horizontal)` for a scroll wheel event; positive is up / left.
fn wheel_deltas(direction: ScrollDirection, amount: u32) -> (i32, i32) {
    let n = i32::try_from(amount.min(MAX_SCROLL_LINES)).unwrap_or(0);
    match direction {
        ScrollDirection::Up => (n, 0),
        ScrollDirection::Down => (-n, 0),
        ScrollDirection::Left => (0, n),
        ScrollDirection::Right => (0, -n),
    }
}

fn scroll_wheel(el: &AxEl, direction: ScrollDirection, amount: u32) -> Result<()> {
    let b = element_bounds(el)
        .ok_or_else(|| Error::Unsupported("the element has no on-screen bounds".into()))?;
    let (v, h) = wheel_deltas(direction, amount);
    let ev = CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Line, 2, v, h, 0)
        .ok_or_else(|| Error::Other("could not create a scroll event".into()))?;
    // The window under this point receives the event; the pointer itself does not move.
    CGEvent::set_location(
        Some(&ev),
        CGPoint {
            x: b.x + b.width / 2.0,
            y: b.y + b.height / 2.0,
        },
    );
    CGEvent::set_flags(Some(&ev), CGEventFlags::empty());
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&ev));
    Ok(())
}

/// How long [`bring_to_front`] waits for the app to become frontmost.
const ACTIVATE_WAIT: Duration = Duration::from_millis(1500);

/// Make the element's app frontmost and its window main. Key events are posted to the
/// frontmost app, so focusing an element in a background app would send the keys that
/// follow somewhere else. AX `AXFrontmost` works from a background process, where
/// `NSRunningApplication.activate` may be refused (macOS 14+). Best effort.
fn bring_to_front(el: &AxEl) {
    let Some(pid) = ax::pid(&el.0) else {
        return;
    };
    if let Some(w) = el.get("AXWindow").and_then(AxEl::element) {
        let _ = perform_named(&w, "AXRaise");
        let _ = set_bool(&w, "AXMain", true);
    }
    if frontmost_pid() == Some(pid) {
        return;
    }
    let app = AxEl(ax::application(pid));
    let _ = set_bool(&app, "AXFrontmost", true);
    let t0 = std::time::Instant::now();
    while frontmost_pid() != Some(pid) && t0.elapsed() < ACTIVATE_WAIT {
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub(super) fn perform(el: &AxEl, action: &UiAction) -> Result<()> {
    match action {
        UiAction::Press => {
            let names = ax::action_names(&el.0);
            let a = PRESS_ACTIONS
                .iter()
                .find(|a| names.iter().any(|n| n == *a))
                .ok_or_else(|| Error::Unsupported("the element cannot be pressed".into()))?;
            perform_named(el, a)
        }
        UiAction::SetValue { value } => {
            let (_, sub) = el.role_subrole();
            if sub.as_deref() == Some("AXSecureTextField") {
                return Err(Error::Invalid("refusing to set a secure field".into()));
            }
            if !ax::is_settable(&el.0, "AXValue") {
                return Err(Error::Unsupported("AXValue is not settable".into()));
            }
            // Sliders and steppers hold a number; text holds a string.
            let numeric = el
                .get("AXValue")
                .is_some_and(|v| v.downcast_ref::<CFNumber>().is_some());
            let r = if numeric {
                let n: f64 = value
                    .trim()
                    .parse()
                    .map_err(|_| Error::Invalid(format!("{value:?} is not a number")))?;
                ax::set_attr(&el.0, "AXValue", &CFNumber::new_f64(n))
            } else {
                ax::set_attr(&el.0, "AXValue", &CFString::from_str(value))
            };
            r.map_err(|e| ax_err("set AXValue", e))
        }
        UiAction::Select => {
            if ax::is_settable(&el.0, "AXSelected") {
                return set_bool(el, "AXSelected", true);
            }
            if let Some(p) = el.parent()
                && ax::is_settable(&p.0, "AXSelectedRows")
            {
                let rows = CFArray::<AXUIElement>::from_objects(&[&*el.0]);
                return ax::set_attr(&p.0, "AXSelectedRows", rows.as_opaque())
                    .map_err(|e| ax_err("set AXSelectedRows", e));
            }
            Err(Error::Unsupported("the element cannot be selected".into()))
        }
        UiAction::Focus => {
            bring_to_front(el);
            set_bool(el, "AXFocused", true)
        }
        UiAction::Scroll { amount: 0, .. } => perform_named(el, "AXScrollToVisible"),
        UiAction::Scroll { direction, amount } => scroll_wheel(el, *direction, *amount),
        UiAction::Expand { expand } => {
            for attr in ["AXExpanded", "AXDisclosing"] {
                if ax::is_settable(&el.0, attr) {
                    return set_bool(el, attr, *expand);
                }
            }
            let (raw, _) = el.role_subrole();
            if raw == "AXDisclosureTriangle" {
                let open = el.get("AXValue").as_deref().and_then(cf_i64).unwrap_or(0) != 0;
                return if open == *expand {
                    Ok(())
                } else {
                    perform_named(el, "AXPress")
                };
            }
            let names = ax::action_names(&el.0);
            let has = |a: &str| names.iter().any(|n| n == a);
            match expand {
                true if has("AXShowMenu") => perform_named(el, "AXShowMenu"),
                false if has("AXCancel") => perform_named(el, "AXCancel"),
                _ => Err(Error::Unsupported(
                    "the element cannot be expanded or collapsed".into(),
                )),
            }
        }
    }
}

// --- Key input ---

/// Virtual key codes (`kVK_*`, HIToolbox Events.h).
const KVK_RETURN: u16 = 36;
const KVK_TAB: u16 = 48;
const KVK_COMMAND: u16 = 55;
const KVK_SHIFT: u16 = 56;
const KVK_OPTION: u16 = 58;
const KVK_CONTROL: u16 = 59;

fn named_key_code(k: NamedKey) -> Option<u16> {
    Some(match k {
        NamedKey::Enter => KVK_RETURN,
        NamedKey::Tab => KVK_TAB,
        NamedKey::Escape => 53,
        NamedKey::Backspace => 51,
        NamedKey::Delete => 117,
        NamedKey::Space => 49,
        NamedKey::Left => 123,
        NamedKey::Right => 124,
        NamedKey::Down => 125,
        NamedKey::Up => 126,
        NamedKey::Home => 115,
        NamedKey::End => 119,
        NamedKey::PageUp => 116,
        NamedKey::PageDown => 121,
        NamedKey::F(n) => match n {
            1 => 122,
            2 => 120,
            3 => 99,
            4 => 118,
            5 => 96,
            6 => 97,
            7 => 98,
            8 => 100,
            9 => 101,
            10 => 109,
            11 => 103,
            12 => 111,
            _ => return None,
        },
    })
}

/// The ANSI (US layout) key for a printable character, and whether Shift is needed.
fn char_key_code(c: char) -> Option<(u16, bool)> {
    const UNSHIFTED: &[(char, u16)] = &[
        ('a', 0),
        ('s', 1),
        ('d', 2),
        ('f', 3),
        ('h', 4),
        ('g', 5),
        ('z', 6),
        ('x', 7),
        ('c', 8),
        ('v', 9),
        ('b', 11),
        ('q', 12),
        ('w', 13),
        ('e', 14),
        ('r', 15),
        ('y', 16),
        ('t', 17),
        ('1', 18),
        ('2', 19),
        ('3', 20),
        ('4', 21),
        ('6', 22),
        ('5', 23),
        ('=', 24),
        ('9', 25),
        ('7', 26),
        ('-', 27),
        ('8', 28),
        ('0', 29),
        (']', 30),
        ('o', 31),
        ('u', 32),
        ('[', 33),
        ('i', 34),
        ('p', 35),
        ('l', 37),
        ('j', 38),
        ('\'', 39),
        ('k', 40),
        (';', 41),
        ('\\', 42),
        (',', 43),
        ('/', 44),
        ('n', 45),
        ('m', 46),
        ('.', 47),
        ('`', 50),
        (' ', 49),
    ];
    const SHIFTED: &[(char, char)] = &[
        ('!', '1'),
        ('@', '2'),
        ('#', '3'),
        ('$', '4'),
        ('%', '5'),
        ('^', '6'),
        ('&', '7'),
        ('*', '8'),
        ('(', '9'),
        (')', '0'),
        ('_', '-'),
        ('+', '='),
        ('{', '['),
        ('}', ']'),
        ('|', '\\'),
        (':', ';'),
        ('"', '\''),
        ('<', ','),
        ('>', '.'),
        ('?', '/'),
        ('~', '`'),
    ];
    let lookup = |c: char| UNSHIFTED.iter().find(|(k, _)| *k == c).map(|(_, v)| *v);
    if c.is_ascii_uppercase() {
        return lookup(c.to_ascii_lowercase()).map(|k| (k, true));
    }
    if let Some(k) = lookup(c) {
        return Some((k, false));
    }
    let base = SHIFTED.iter().find(|(s, _)| *s == c)?.1;
    lookup(base).map(|k| (k, true))
}

/// Keys the hardware sends with the Fn (and, for arrows, numeric pad) flag.
fn key_flags(code: u16) -> CGEventFlags {
    match code {
        123..=126 => CGEventFlags::MaskSecondaryFn | CGEventFlags::MaskNumericPad,
        115..=122 | 96..=111 => CGEventFlags::MaskSecondaryFn,
        _ => CGEventFlags::empty(),
    }
}

/// Modifier keys to hold, in press order.
fn modifier_keys(m: Modifiers) -> Vec<(u16, CGEventFlags)> {
    let mut v = Vec::new();
    if m.ctrl {
        v.push((KVK_CONTROL, CGEventFlags::MaskControl));
    }
    if m.alt {
        v.push((KVK_OPTION, CGEventFlags::MaskAlternate));
    }
    if m.shift {
        v.push((KVK_SHIFT, CGEventFlags::MaskShift));
    }
    if m.meta {
        v.push((KVK_COMMAND, CGEventFlags::MaskCommand));
    }
    v
}

fn post_key(code: u16, down: bool, flags: CGEventFlags) -> Result<()> {
    let ev = CGEvent::new_keyboard_event(None, code, down)
        .ok_or_else(|| Error::Other("could not create a keyboard event".into()))?;
    CGEvent::set_flags(Some(&ev), flags);
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&ev));
    Ok(())
}

fn post_unicode(units: &[u16]) -> Result<()> {
    if units.is_empty() {
        return Ok(());
    }
    for down in [true, false] {
        let ev = CGEvent::new_keyboard_event(None, 0, down)
            .ok_or_else(|| Error::Other("could not create a keyboard event".into()))?;
        ax::set_unicode(&ev, units);
        // No modifier flags: held keys must not turn text into shortcuts.
        CGEvent::set_flags(Some(&ev), CGEventFlags::empty());
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&ev));
    }
    std::thread::sleep(Duration::from_millis(2));
    Ok(())
}

fn tap(code: u16) -> Result<()> {
    post_key(code, true, CGEventFlags::empty())?;
    post_key(code, false, CGEventFlags::empty())
}

fn type_text(text: &str) -> Result<()> {
    let mut buf: Vec<u16> = Vec::with_capacity(UNICODE_CHUNK);
    for c in text.chars() {
        match c {
            '\n' | '\t' => {
                post_unicode(&buf)?;
                buf.clear();
                tap(if c == '\n' { KVK_RETURN } else { KVK_TAB })?;
            }
            c if c.is_control() => {}
            c => {
                if buf.len() + c.len_utf16() > UNICODE_CHUNK {
                    post_unicode(&buf)?;
                    buf.clear();
                }
                let mut tmp = [0u16; 2];
                buf.extend_from_slice(c.encode_utf16(&mut tmp));
            }
        }
    }
    post_unicode(&buf)
}

fn press_chord(chord: &KeyChord) -> Result<()> {
    let (code, shift) = match chord.key {
        Key::Named(k) => (named_key_code(k), false),
        Key::Char(c) => match char_key_code(c) {
            Some((k, s)) => (Some(k), s),
            None => (None, false),
        },
    };
    let code = code.ok_or_else(|| Error::Invalid(format!("no key code for {:?}", chord.key)))?;
    let mut mods = chord.modifiers;
    mods.shift |= shift;
    let held = modifier_keys(mods);
    let mut flags = CGEventFlags::empty();
    for (k, f) in &held {
        flags |= *f;
        post_key(*k, true, flags)?;
    }
    post_key(code, true, flags | key_flags(code))?;
    post_key(code, false, flags | key_flags(code))?;
    for (k, f) in held.iter().rev() {
        flags.remove(*f);
        post_key(*k, false, flags)?;
    }
    Ok(())
}

pub(super) fn key_input(input: &KeyInput) -> Result<()> {
    require_trust()?;
    match input {
        KeyInput::Text(t) => type_text(t),
        KeyInput::Keys(chords) => {
            for c in chords {
                press_chord(c)?;
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        }
    }
}

// --- Launch and activate ---

/// App names we pass to `open`: no paths, no options, no control characters.
fn valid_app_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && !name.starts_with('-')
        && !name.contains('/')
        && !name.chars().any(char::is_control)
}

/// `com.apple.Safari`: reverse-DNS with at least two dots, no spaces.
fn looks_like_bundle_id(name: &str) -> bool {
    name.matches('.').count() >= 2
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && !name.starts_with('.')
        && !name.ends_with('.')
}

fn app_matches(e: &AppEntry, name: &str) -> bool {
    let bare = name.strip_suffix(".app").unwrap_or(name);
    e.name.eq_ignore_ascii_case(bare)
        || e.name.to_lowercase() == bare.to_lowercase()
        || e.id
            .as_deref()
            .is_some_and(|id| id.eq_ignore_ascii_case(name))
}

fn run_open(flag: &str, arg: &str) -> Result<()> {
    let status = Command::new("/usr/bin/open")
        .arg(flag)
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| Error::Other(format!("could not run open: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::NotFound(format!("no app named {arg:?}")))
    }
}

pub(super) fn open_app(name: &str) -> Result<()> {
    let name = name.trim();
    if !valid_app_name(name) {
        return Err(Error::Invalid(format!("not an app name: {name:?}")));
    }
    if let Some(app) = running_apps(&cg_windows())
        .into_iter()
        .find(|a| app_matches(&a.entry, name))
    {
        if app.running.isHidden() {
            app.running.unhide();
        }
        if app
            .running
            .activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows)
        {
            return Ok(());
        }
        // macOS 14+ may refuse activation from a background process; LaunchServices does not.
        if let Some(id) = &app.entry.id {
            return run_open("-b", id);
        }
    }
    if looks_like_bundle_id(name) {
        run_open("-b", name)
    } else {
        run_open("-a", name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_codes() {
        assert_eq!(named_key_code(NamedKey::Enter), Some(36));
        assert_eq!(named_key_code(NamedKey::F(12)), Some(111));
        assert_eq!(named_key_code(NamedKey::F(13)), None);
        assert_eq!(char_key_code('a'), Some((0, false)));
        assert_eq!(char_key_code('A'), Some((0, true)));
        assert_eq!(char_key_code('v'), Some((9, false)));
        assert_eq!(char_key_code('?'), Some((44, true)));
        assert_eq!(char_key_code('é'), None);
        assert!(key_flags(126).contains(CGEventFlags::MaskSecondaryFn));
        assert!(key_flags(0).is_empty());
    }

    #[test]
    fn modifiers_in_order() {
        let m = Modifiers {
            ctrl: true,
            shift: true,
            meta: true,
            ..Default::default()
        };
        let codes: Vec<u16> = modifier_keys(m).iter().map(|(k, _)| *k).collect();
        assert_eq!(codes, vec![KVK_CONTROL, KVK_SHIFT, KVK_COMMAND]);
    }

    #[test]
    fn action_mapping() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let k = kinds_from_ax(
            "AXButton",
            &names(&["AXPress", "AXShowMenu"]),
            Settable::default(),
        );
        assert_eq!(k, vec![UiActionKind::Press]);
        let k = kinds_from_ax(
            "AXPopUpButton",
            &names(&["AXPress", "AXShowMenu"]),
            Settable {
                focused: true,
                ..Default::default()
            },
        );
        assert_eq!(
            k,
            vec![
                UiActionKind::Press,
                UiActionKind::Focus,
                UiActionKind::Expand
            ]
        );
        let k = kinds_from_ax(
            "AXTextField",
            &names(&["AXConfirm"]),
            Settable {
                value: true,
                focused: true,
                ..Default::default()
            },
        );
        assert_eq!(
            k,
            vec![
                UiActionKind::Press,
                UiActionKind::SetValue,
                UiActionKind::Focus
            ]
        );
        let k = kinds_from_ax(
            "AXRow",
            &names(&["AXScrollToVisible"]),
            Settable {
                parent_rows: true,
                expanded: true,
                ..Default::default()
            },
        );
        assert_eq!(
            k,
            vec![
                UiActionKind::Select,
                UiActionKind::Scroll,
                UiActionKind::Expand
            ]
        );
    }

    #[test]
    fn scroll_deltas() {
        assert_eq!(wheel_deltas(ScrollDirection::Down, 3), (-3, 0));
        assert_eq!(wheel_deltas(ScrollDirection::Right, 2), (0, -2));
        assert_eq!(wheel_deltas(ScrollDirection::Up, 1000), (100, 0));
    }

    #[test]
    fn app_names() {
        assert!(valid_app_name("Safari"));
        assert!(!valid_app_name("-a"));
        assert!(!valid_app_name("/Applications/Safari.app"));
        assert!(looks_like_bundle_id("com.apple.Safari"));
        assert!(!looks_like_bundle_id("Visual Studio Code"));
        assert!(!looks_like_bundle_id("Notes.app"));
        let e = AppEntry {
            id: Some("com.apple.Notes".into()),
            name: "Notes".into(),
            pid: Some(1),
            frontmost: false,
        };
        assert!(app_matches(&e, "notes"));
        assert!(app_matches(&e, "Notes.app"));
        assert!(app_matches(&e, "COM.APPLE.NOTES"));
        assert!(!app_matches(&e, "Mail"));
    }
}
