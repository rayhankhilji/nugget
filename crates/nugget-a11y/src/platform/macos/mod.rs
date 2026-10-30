//! macOS backend: the Accessibility (AX) API from ApplicationServices, the general pasteboard
//! and a synthetic Cmd+V.
//!
//! - Permission: `AXIsProcessTrustedWithOptions` (with the prompt for `request_permission`).
//! - Frontmost: the system-wide `AXFocusedApplication`, its `AXFocusedWindow`, and the app's
//!   `AXFocusedUIElement`, which must belong to that window.
//! - Secure fields (`AXSecureTextField`) are detected from the subrole before any value is
//!   requested, and their value is never asked for.
//! - Chromium browsers and Electron apps build their web accessibility tree only for a client
//!   that sets `AXManualAccessibility` on the app (not `AXEnhancedUserInterface`, which breaks
//!   window animations). It is set once per pid before the focus is read; a placeholder focus
//!   (window, group, web area) right after that is read once more after a short wait.
//! - Rich web editors that expose no `AXValue`/`AXSelectedTextRange` are read through text
//!   markers (`AXTextMarkerRangeForUIElement`, `AXStringForTextMarkerRange`,
//!   `AXSelectedTextMarkerRange`), bounded and best effort.
//! - Insertion sets `AXSelectedTextRange` to the selection and then `AXSelectedText`, which
//!   replaces the selection like typing. Offsets are converted between characters and the
//!   UTF-16 units AX uses. Text with line breaks for a multi-line web editor is refused, so the
//!   engine pastes it: Chromium and WebKit insert it as one `insertText`, which rich editor
//!   frameworks handle inconsistently.
//! - Computer use (M4) is in `computer`: apps and windows, element actions, key input, launch.

mod ax;
mod computer;
mod pasteboard;

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use nugget_core::ipc::AppInfo;
use nugget_core::{Error, Result};
use objc2_app_kit::NSRunningApplication;
use objc2_application_services::AXUIElement;
use objc2_core_foundation::{CFArray, CFBoolean, CFRetained, CFString, CFType, CFURL};
use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventTapLocation};

use crate::clipboard::Clipboard;
use crate::computer::{AppEntry, KeyInput, UiAction, UiActionKind};
use crate::engine::{FieldRead, Platform, PlatformWindow, Target};
use crate::platform::hints;
use crate::platform::text_map::{self, TextMap, chars_to_utf16, utf16_to_chars};
use crate::role::{Role, role_from_ax};
use crate::sanitize;
use crate::tree::{Element, Node, States};
use crate::verify::FieldState;
use crate::{Permission, Rect};

/// macOS AX platform.
#[derive(Debug, Default)]
pub struct AxPlatform;

impl AxPlatform {
    pub fn new() -> Self {
        AxPlatform
    }
}

/// A live AX element.
#[derive(Clone)]
pub struct AxEl(ax::Ax);

fn cf_string(t: &CFType) -> Option<String> {
    t.downcast_ref::<CFString>().map(|s| s.to_string())
}

fn cf_bool(t: &CFType) -> Option<bool> {
    t.downcast_ref::<CFBoolean>().map(CFBoolean::value)
}

fn cf_url(t: &CFType) -> Option<String> {
    if let Some(s) = cf_string(t) {
        return Some(s);
    }
    let url = t.downcast_ref::<CFURL>()?;
    Some(url.string().to_string())
}

fn cf_string_list(t: &CFType) -> Option<String> {
    let out: Vec<String> = ax::array_items(t, 16)
        .iter()
        .filter_map(|v| cf_string(v))
        .collect();
    (!out.is_empty()).then(|| out.join(" "))
}

/// How long after `AXManualAccessibility` was set a placeholder focus is read again.
const MANUAL_AX_FRESH: Duration = Duration::from_secs(2);
/// The wait before reading the focus again.
const FOCUS_RETRY_WAIT: Duration = Duration::from_millis(250);
/// Ancestors climbed to find a web area.
const WEB_AREA_DEPTH: usize = 40;
/// Cap on text read through text markers (bytes).
const MARKER_MAX_BYTES: usize = 1 << 20;

/// Pids whose web accessibility was handled: when, and whether the app embeds Chromium.
static MANUAL_AX: Mutex<BTreeMap<i32, (Instant, bool)>> = Mutex::new(BTreeMap::new());

/// Turn on a Chromium/Electron app's web accessibility (once per pid). Returns whether that
/// happened recently enough that its tree may still be building.
fn ensure_web_accessibility(app: &AxEl, pid: i32, running: Option<&NSRunningApplication>) -> bool {
    let mut seen = MANUAL_AX.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(&(since, chromium)) = seen.get(&pid) {
        return chromium && since.elapsed() < MANUAL_AX_FRESH;
    }
    let id = running
        .and_then(|r| r.bundleIdentifier())
        .map(|s| s.to_string());
    let chromium = id
        .as_deref()
        .is_some_and(hints::macos_needs_manual_accessibility)
        || running
            .and_then(|r| r.bundleURL())
            .and_then(|u| u.path())
            .is_some_and(|p| {
                let root = std::path::PathBuf::from(p.to_string());
                hints::macos_bundle_embeds_chromium(|rel| root.join(rel).exists())
            });
    if chromium {
        let r = ax::set_attr(&app.0, "AXManualAccessibility", CFBoolean::new(true));
        tracing::debug!(pid, ok = r.is_ok(), "set AXManualAccessibility");
    }
    // Pids are reused; forget old entries rather than grow without bound.
    if seen.len() >= 512 {
        seen.clear();
    }
    seen.insert(pid, (Instant::now(), chromium));
    chromium
}

const BASE_ATTRS: &[&str] = &[
    "AXTitle",
    "AXDescription",
    "AXPlaceholderValue",
    "AXFocused",
    "AXPosition",
    "AXSize",
    "AXEnabled",
    "AXURL",
    "AXDOMClassList",
    "AXIdentifier",
    "AXSelected",
    "AXExpanded",
    "AXDisclosing",
    "AXTitleUIElement",
];

/// How deep [`AxEl::label_text`] looks for a label's text.
const LABEL_DEPTH: usize = 2;

impl AxEl {
    fn get(&self, name: &str) -> Option<CFRetained<CFType>> {
        ax::attr(&self.0, name)
    }

    fn element(t: CFRetained<CFType>) -> Option<AxEl> {
        t.downcast::<AXUIElement>().ok().map(AxEl)
    }

    fn role_subrole(&self) -> (String, Option<String>) {
        let v = ax::attrs(&self.0, &["AXRole", "AXSubrole"]);
        let role = v[0].as_deref().and_then(cf_string).unwrap_or_default();
        let sub = v[1].as_deref().and_then(cf_string);
        (role, sub)
    }

    fn read_node(&self) -> Node {
        let (raw, sub) = self.role_subrole();
        let secure = sub.as_deref() == Some("AXSecureTextField");
        let mut names: Vec<&str> = BASE_ATTRS.to_vec();
        if !secure {
            names.push("AXValue");
        }
        let v = ax::attrs(&self.0, &names);
        let get = |name: &str| {
            names
                .iter()
                .position(|n| *n == name)
                .and_then(|i| v[i].as_deref())
        };
        let role = role_from_ax(&raw, sub.as_deref());
        let enabled = get("AXEnabled").and_then(cf_bool).unwrap_or(true);
        let multiline = match raw.as_str() {
            "AXTextArea" => Some(true),
            "AXTextField" | "AXComboBox" => Some(false),
            _ => None,
        };
        let mut node = Node {
            role,
            raw_role: match &sub {
                Some(s) => format!("{raw}/{s}"),
                None => raw.clone(),
            },
            name: get("AXTitle").and_then(cf_string),
            description: get("AXDescription").and_then(cf_string),
            placeholder: get("AXPlaceholderValue").and_then(cf_string),
            states: States {
                // View-based NSTableView cells report AXFocused = true whether or not the table
                // has focus (a cell being edited puts the focus on its field editor instead), so
                // a cell's claim is ignored.
                focused: raw != "AXCell" && get("AXFocused").and_then(cf_bool).unwrap_or(false),
                editable: role.is_text_input() && enabled,
                secure,
                multiline,
                selected: get("AXSelected").and_then(cf_bool).unwrap_or(false),
                offscreen: false,
                disabled: !enabled,
                checked: None,
                expanded: get("AXExpanded")
                    .or_else(|| get("AXDisclosing"))
                    .and_then(cf_bool),
            },
            ..Default::default()
        };
        if let (Some(p), Some(s)) = (
            get("AXPosition").and_then(ax::as_point),
            get("AXSize").and_then(ax::as_size),
        ) && s.width > 0.0
            && s.height > 0.0
        {
            node.bounds = Some(Rect {
                x: p.x,
                y: p.y,
                width: s.width,
                height: s.height,
            });
        }
        if !secure && let Some(v) = get("AXValue") {
            node.value = cf_string(v);
            node.states.checked = computer::checked_from_ax(&raw, sub.as_deref(), v);
            if node.states.expanded.is_none() && raw == "AXDisclosureTriangle" {
                node.states.expanded = computer::checked_from_ax("AXCheckBox", None, v);
            }
        }
        // A web control labelled by a `<label for>` (Chromium, WebKit) has no AXTitle: its name
        // is the text of its AXTitleUIElement.
        if node.name.as_deref().is_none_or(|n| n.trim().is_empty())
            && let Some(label) = names
                .iter()
                .position(|n| *n == "AXTitleUIElement")
                .and_then(|i| v[i].clone())
                .and_then(AxEl::element)
        {
            node.name = label.label_text(LABEL_DEPTH);
        }
        if let Some(u) = get("AXURL").and_then(cf_url) {
            node.attrs.insert("url".into(), u);
        }
        if let Some(c) = get("AXDOMClassList").and_then(cf_string_list) {
            node.attrs.insert("dom_classes".into(), c);
        }
        if let Some(id) = get("AXIdentifier").and_then(cf_string) {
            node.attrs.insert("identifier".into(), id);
        }
        // Rich editors (Mail's compose web view, contenteditable regions) are editable web
        // areas or groups; AX says so by making the value settable.
        if !node.states.editable
            && node.states.focused
            && matches!(
                role,
                Role::WebArea | Role::Group | Role::Document | Role::Unknown
            )
        {
            node.states.editable =
                ax::is_settable(&self.0, "AXSelectedText") || ax::is_settable(&self.0, "AXValue");
        }
        node
    }

    /// The text of a label element: its title, value or description, else its children's text.
    fn label_text(&self, depth: usize) -> Option<String> {
        let v = ax::attrs(&self.0, &["AXTitle", "AXValue", "AXDescription"]);
        if let Some(s) = v
            .iter()
            .flatten()
            .filter_map(|t| cf_string(t))
            .map(|s| s.trim().to_owned())
            .find(|s| !s.is_empty())
        {
            return Some(s);
        }
        if depth == 0 {
            return None;
        }
        let parts: Vec<String> = self
            .children(16)
            .iter()
            .filter_map(|c| c.label_text(depth - 1))
            .collect();
        (!parts.is_empty()).then(|| parts.join(" "))
    }

    fn raw_role(&self) -> String {
        self.get("AXRole")
            .and_then(|t| cf_string(&t))
            .unwrap_or_default()
    }

    /// The focus a Chromium app reports before its web tree is ready.
    fn is_placeholder_focus(&self) -> bool {
        hints::macos_focus_is_placeholder(
            &self.raw_role(),
            ax::is_settable(&self.0, "AXSelectedText"),
        )
    }

    /// Whether this element is, or is inside, a web area (bounded walk up).
    fn in_web_area(&self) -> bool {
        let mut cur = Some(self.clone());
        for _ in 0..WEB_AREA_DEPTH {
            let Some(el) = cur else { return false };
            match el.raw_role().as_str() {
                "AXWebArea" => return true,
                "AXWindow" | "AXApplication" => return false,
                _ => {}
            }
            cur = el.parent();
        }
        false
    }

    fn marker_param(&self, name: &str, param: &CFType) -> Option<CFRetained<CFType>> {
        ax::param_attr(&self.0, name, param)
    }

    fn marker_string(&self, range: &CFType) -> Option<String> {
        let v = self.marker_param("AXStringForTextMarkerRange", range)?;
        let s = cf_string(&v)?;
        (s.len() <= MARKER_MAX_BYTES).then_some(s)
    }

    /// The text between two markers, in either order.
    fn string_between(&self, a: &CFType, b: &CFType) -> Option<String> {
        let pair = CFArray::<CFType>::from_objects(&[a, b]);
        let range = self.marker_param("AXTextMarkerRangeForUnorderedTextMarkers", &pair)?;
        self.marker_string(&range)
    }

    /// Text and selection (character offsets into that text) through text markers, for rich
    /// web editors without `AXValue`/`AXSelectedTextRange`. Best effort: `None` when the
    /// element has no marker range; the selection is `None` when it cannot be placed in the
    /// element's text.
    fn marker_state(&self) -> Option<(String, Option<(u32, u32)>)> {
        let el_range = self.marker_param("AXTextMarkerRangeForUIElement", &self.0)?;
        let text = self.marker_string(&el_range)?;
        let selection = (|| {
            let sel = self.get("AXSelectedTextMarkerRange")?;
            let sel_start = self.marker_param("AXStartTextMarkerForTextMarkerRange", &sel)?;
            // The text from the element's start marker to the selection start (WebKit and
            // Chromium both answer `AXStartTextMarkerForTextMarkerRange`).
            let el_start = self.marker_param("AXStartTextMarkerForTextMarkerRange", &el_range)?;
            let prefix = self.string_between(&el_start, &sel_start)?;
            // The selection must lie in this element: its prefix is a prefix of the text.
            if !text.starts_with(&prefix) {
                return None;
            }
            let start = prefix.chars().count() as u32;
            let selected = self.marker_string(&sel).unwrap_or_default();
            let total = text.chars().count() as u32;
            let end = (start + selected.chars().count() as u32).min(total);
            Some((start, end))
        })();
        Some((text, selection))
    }

    /// Bounds of the selected text marker range (rich web editors).
    fn marker_selection_bounds(&self) -> Option<Rect> {
        let sel = self.get("AXSelectedTextMarkerRange")?;
        let v = self.marker_param("AXBoundsForTextMarkerRange", &sel)?;
        let r = ax::as_rect(&v)?;
        (r.size.height > 0.0).then_some(Rect {
            x: r.origin.x,
            y: r.origin.y,
            width: r.size.width.max(1.0),
            height: r.size.height,
        })
    }

    fn selection_utf16(&self) -> Option<(usize, usize)> {
        let r = ax::as_range(&*self.get("AXSelectedTextRange")?)?;
        let loc = usize::try_from(r.location).ok()?;
        let len = usize::try_from(r.length).ok()?;
        Some((loc, loc + len))
    }

    fn bounds_for_range(&self, loc: usize, len: usize) -> Option<Rect> {
        let param = ax::range_value(loc, len)?;
        let v = ax::param_attr(&self.0, "AXBoundsForRange", &param)?;
        let r = ax::as_rect(&v)?;
        (r.size.height > 0.0).then_some(Rect {
            x: r.origin.x,
            y: r.origin.y,
            width: r.size.width,
            height: r.size.height,
        })
    }
}

impl Element for AxEl {
    fn node(&self) -> Node {
        self.read_node()
    }

    fn children(&self, max: usize) -> Vec<Self> {
        ax::attr_values(&self.0, "AXChildren", max)
            .into_iter()
            .filter_map(AxEl::element)
            .collect()
    }

    fn parent(&self) -> Option<Self> {
        self.get("AXParent").and_then(AxEl::element)
    }

    fn same(&self, other: &Self) -> bool {
        *self.0 == *other.0
    }

    fn actions(&self) -> Vec<UiActionKind> {
        computer::actions(self)
    }

    fn perform(&self, action: &UiAction) -> Result<()> {
        computer::perform(self, action)
    }
}

impl Platform for AxPlatform {
    type Element = AxEl;

    fn permission(&self) -> Permission {
        if ax::is_trusted(false) {
            Permission::Granted
        } else {
            Permission::Denied
        }
    }

    fn request_permission(&self) -> Result<()> {
        // Shows the "would like to control this computer" prompt with a link to Settings.
        ax::is_trusted(true);
        Ok(())
    }

    fn focus(&self) -> Result<Option<Target<AxEl>>> {
        computer::require_trust()?;
        let sys = ax::system_wide();
        let Some(app) = ax::attr(&sys, "AXFocusedApplication").and_then(AxEl::element) else {
            return Ok(None);
        };
        let pid = ax::pid(&app.0);
        let running = pid.and_then(NSRunningApplication::runningApplicationWithProcessIdentifier);
        // Chromium and Electron expose web content only once asked; before the focus is read.
        let fresh = pid.is_some_and(|p| ensure_web_accessibility(&app, p, running.as_deref()));
        let mut focused = app.get("AXFocusedUIElement").and_then(AxEl::element);
        if fresh && focused.as_ref().is_none_or(AxEl::is_placeholder_focus) {
            std::thread::sleep(FOCUS_RETRY_WAIT);
            if let Some(again) = app.get("AXFocusedUIElement").and_then(AxEl::element) {
                focused = Some(again);
            }
        }
        let Some(el) = focused else {
            return Ok(None);
        };
        let window = app.get("AXFocusedWindow").and_then(AxEl::element);
        // Only the focused window: an element in another window (or a floating panel) is not
        // the user's target.
        if let (Some(w), Some(ew)) = (&window, el.get("AXWindow").and_then(AxEl::element))
            && !w.same(&ew)
        {
            return Ok(None);
        }
        let name = running
            .as_ref()
            .and_then(|r| r.localizedName())
            .map(|s| s.to_string())
            .or_else(|| app.get("AXTitle").and_then(|t| cf_string(&t)))
            .unwrap_or_default();
        let id = running
            .as_ref()
            .and_then(|r| r.bundleIdentifier())
            .map(|s| s.to_string());
        let window_title = window
            .as_ref()
            .and_then(|w| w.get("AXTitle"))
            .and_then(|t| cf_string(&t))
            .filter(|t| !t.is_empty());
        Ok(Some(Target {
            app: AppInfo {
                id,
                name,
                window_title,
                url: None,
            },
            element: el,
        }))
    }

    fn read_field(&self, el: &AxEl, max_value_bytes: usize) -> Result<FieldRead> {
        let node = el.read_node();
        let label = if node.name.is_none() {
            el.get("AXTitleUIElement")
                .and_then(AxEl::element)
                .and_then(|l| {
                    let n = l.read_node();
                    n.value.or(n.name)
                })
                // AppKit's `accessibilityLabel` (what VoiceOver reads for a field) is the
                // element's AXDescription.
                .or_else(|| node.description.clone())
        } else {
            None
        };
        let state = if node.states.secure {
            FieldState::default()
        } else {
            let raw = node.value.clone().unwrap_or_default();
            let mut norm = text_map::normalize_newlines(&raw);
            let mut selection = el.selection_utf16().map(|(s, e)| {
                (
                    norm.to_text(utf16_to_chars(&raw, s)),
                    norm.to_text(utf16_to_chars(&raw, e)),
                )
            });
            // Rich web editors (contenteditable, Mail compose) may expose neither: read the
            // text markers instead.
            if (raw.is_empty() || selection.is_none())
                && node.states.editable
                && el.in_web_area()
                && let Some((text, sel)) = el.marker_state()
            {
                let marked = text_map::normalize_newlines(&text);
                let map_sel = |m: &TextMap| sel.map(|(s, e)| (m.to_text(s), m.to_text(e)));
                if raw.is_empty() {
                    selection = map_sel(&marked);
                    norm = marked;
                } else if selection.is_none() && marked.text == norm.text {
                    selection = map_sel(&marked);
                }
            }
            let value = sanitize::clean_value(&norm.text, max_value_bytes);
            let truncated = value.len() < norm.text.len();
            FieldState {
                value,
                selection,
                truncated,
            }
        };
        Ok(FieldRead { node, label, state })
    }

    fn caret_bounds(&self, el: &AxEl, state: &FieldState) -> Option<Rect> {
        if el.selection_utf16().is_none()
            && let Some(r) = el.marker_selection_bounds()
        {
            return Some(r);
        }
        let (s, e) = el.selection_utf16().or_else(|| {
            let (s, e) = state.selection?;
            Some((
                chars_to_utf16(&state.value, s),
                chars_to_utf16(&state.value, e),
            ))
        })?;
        if e > s {
            return el.bounds_for_range(s, e - s);
        }
        let len: usize = state.value.encode_utf16().count();
        if s < len
            && let Some(r) = el.bounds_for_range(s, 1)
        {
            return Some(Rect { width: 1.0, ..r });
        }
        if s > 0 {
            let r = el.bounds_for_range(s - 1, 1)?;
            return Some(Rect {
                x: r.x + r.width,
                width: 1.0,
                ..r
            });
        }
        // Empty field: the zero-length range still has a position in most editors.
        el.bounds_for_range(0, 0)
    }

    fn insert_text(&self, el: &AxEl, text: &str, before: &FieldState) -> Result<()> {
        if !ax::is_settable(&el.0, "AXSelectedText") {
            return Err(Error::Unsupported("AXSelectedText is not settable".into()));
        }
        if text.contains('\n')
            && hints::macos_web_multiline_editor(&el.raw_role(), el.in_web_area())
        {
            return Err(Error::Unsupported(
                "line breaks into a web editor are pasted".into(),
            ));
        }
        // Best effort: keeps the insertion where the user left the caret. Offsets are placed in
        // the live `AXValue` (the read value had its line endings normalised); fields read
        // through text markers keep their own selection.
        let raw = el
            .get("AXValue")
            .and_then(|t| cf_string(&t))
            .unwrap_or_default();
        let map = text_map::normalize_newlines(&raw);
        if let Some((s, e)) = before.selection
            && !raw.is_empty()
            && map.len() as usize == before.value.chars().count()
        {
            let (s, e) = (s.min(e), s.max(e));
            let s16 = chars_to_utf16(&raw, map.to_source(s));
            let e16 = chars_to_utf16(&raw, map.to_source(e));
            if let Some(range) = ax::range_value(s16, e16.saturating_sub(s16)) {
                let _ = ax::set_attr(&el.0, "AXSelectedTextRange", &range);
            }
        }
        let value = CFString::from_str(text);
        ax::set_attr(&el.0, "AXSelectedText", &value)
            .map_err(|e| Error::Other(format!("AX set AXSelectedText failed: {e:?}")))
    }

    fn clipboard(&self) -> Result<Box<dyn Clipboard>> {
        Ok(Box::new(pasteboard::Pasteboard::general()))
    }

    fn paste(&self, _el: &AxEl) -> Result<()> {
        // kVK_ANSI_V with the Command flag. Never Return.
        const KVK_ANSI_V: u16 = 9;
        for down in [true, false] {
            let ev = CGEvent::new_keyboard_event(None, KVK_ANSI_V, down)
                .ok_or_else(|| Error::Other("could not create a keyboard event".into()))?;
            CGEvent::set_flags(Some(&ev), CGEventFlags::MaskCommand);
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&ev));
        }
        Ok(())
    }

    fn apps(&self) -> Result<Vec<AppEntry>> {
        computer::apps()
    }

    fn windows(&self) -> Result<Vec<PlatformWindow<AxEl>>> {
        computer::windows()
    }

    fn key_input(&self, input: &KeyInput) -> Result<()> {
        computer::key_input(input)
    }

    fn open_app(&self, name: &str) -> Result<()> {
        computer::open_app(name)
    }
}
