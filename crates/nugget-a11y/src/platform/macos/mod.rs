//! macOS backend: the Accessibility (AX) API from ApplicationServices, the general pasteboard
//! and a synthetic Cmd+V.
//!
//! - Permission: `AXIsProcessTrustedWithOptions` (with the prompt for `request_permission`).
//! - Frontmost: the system-wide `AXFocusedApplication`, its `AXFocusedWindow`, and the app's
//!   `AXFocusedUIElement`, which must belong to that window.
//! - Secure fields (`AXSecureTextField`) are detected from the subrole before any value is
//!   requested, and their value is never asked for.
//! - Insertion sets `AXSelectedTextRange` to the selection and then `AXSelectedText`, which
//!   replaces the selection like typing. Offsets are converted between characters and the
//!   UTF-16 units AX uses.

mod ax;
mod pasteboard;

use nugget_core::ipc::AppInfo;
use nugget_core::{Error, Result};
use objc2_app_kit::NSRunningApplication;
use objc2_application_services::AXUIElement;
use objc2_core_foundation::{CFBoolean, CFRetained, CFString, CFType, CFURL};
use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventTapLocation};

use crate::clipboard::Clipboard;
use crate::engine::{FieldRead, Platform, Target};
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

fn utf16_to_chars(s: &str, units: usize) -> u32 {
    let mut seen = 0usize;
    let mut chars = 0u32;
    for c in s.chars() {
        if seen >= units {
            break;
        }
        seen += c.len_utf16();
        chars += 1;
    }
    chars
}

fn chars_to_utf16(s: &str, chars: u32) -> usize {
    s.chars().take(chars as usize).map(char::len_utf16).sum()
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
];

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
                focused: get("AXFocused").and_then(cf_bool).unwrap_or(false),
                editable: role.is_text_input() && enabled,
                secure,
                multiline,
                selected: get("AXSelected").and_then(cf_bool).unwrap_or(false),
                offscreen: false,
                ..States::default()
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
        if !secure {
            node.value = get("AXValue").and_then(cf_string);
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
        if !ax::is_trusted(false) {
            return Err(Error::Unauthorized(
                "Accessibility permission is not granted".into(),
            ));
        }
        let sys = ax::system_wide();
        let Some(app) = ax::attr(&sys, "AXFocusedApplication").and_then(AxEl::element) else {
            return Ok(None);
        };
        let Some(el) = app.get("AXFocusedUIElement").and_then(AxEl::element) else {
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
        let pid = ax::pid(&app.0);
        let running = pid.and_then(NSRunningApplication::runningApplicationWithProcessIdentifier);
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
        } else {
            None
        };
        let state = if node.states.secure {
            FieldState::default()
        } else {
            let raw = node.value.clone().unwrap_or_default();
            let value = sanitize::clean_value(&raw, max_value_bytes);
            let truncated = value.len() < raw.len();
            let selection = el
                .selection_utf16()
                .map(|(s, e)| (utf16_to_chars(&raw, s), utf16_to_chars(&raw, e)));
            FieldState {
                value,
                selection,
                truncated,
            }
        };
        Ok(FieldRead { node, label, state })
    }

    fn caret_bounds(&self, el: &AxEl, state: &FieldState) -> Option<Rect> {
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
        if let Some((s, e)) = before.selection {
            let (s, e) = (s.min(e), s.max(e));
            let s16 = chars_to_utf16(&before.value, s);
            let e16 = chars_to_utf16(&before.value, e);
            if let Some(range) = ax::range_value(s16, e16 - s16) {
                // Best effort: keeps the insertion where the user left the caret.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_offsets() {
        let s = "a😀b";
        assert_eq!(utf16_to_chars(s, 0), 0);
        assert_eq!(utf16_to_chars(s, 1), 1);
        assert_eq!(utf16_to_chars(s, 3), 2);
        assert_eq!(utf16_to_chars(s, 4), 3);
        assert_eq!(chars_to_utf16(s, 2), 3);
        assert_eq!(chars_to_utf16(s, 3), 4);
    }
}
