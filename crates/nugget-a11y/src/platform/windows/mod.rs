//! Windows backend: UI Automation (`IUIAutomation` through the `windows` crate), the Win32
//! clipboard and `SendInput` for Ctrl+V.
//!
//! - Frontmost: `GetForegroundWindow`; the focused element (`GetFocusedElement`) must belong to
//!   that window's process.
//! - Secure fields: `IsPassword`; their value is never requested.
//! - Value and selection: `ValuePattern` and `TextPattern` (selection offsets are measured as
//!   the text length from the document start to each endpoint).
//! - Insertion: `ValuePattern.SetValue` with the selection replaced, for plain edit controls
//!   whose whole value was read; rich documents and web content use the clipboard fallback.
//! - Bounds are converted from physical pixels to logical points with the window's DPI.
//! - Computer use (M4) is in `computer`: windows, pattern-based actions, key input, launch.

mod clipboard;
mod computer;
mod uia;

use nugget_core::ipc::AppInfo;
use nugget_core::{Error, Result};

use crate::computer::{AppEntry, KeyInput, UiAction, UiActionKind};
use crate::engine::{FieldRead, Platform, PlatformWindow, Target};
use crate::role::{Role, role_from_uia, uia as ct};
use crate::sanitize;
use crate::tree::{Element, Node, States};
use crate::verify::{self, FieldState};
use crate::{Permission, Rect};

pub use uia::UiaEl;

/// UI Automation platform.
#[derive(Debug, Default)]
pub struct UiaPlatform;

impl UiaPlatform {
    pub fn new() -> Self {
        UiaPlatform
    }
}

fn scale_rect(r: Rect, scale: f64) -> Rect {
    Rect {
        x: r.x / scale,
        y: r.y / scale,
        width: r.width / scale,
        height: r.height / scale,
    }
}

fn looks_like_url(s: &str) -> bool {
    let l = s.trim_start().to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://") || l.starts_with("file://")
}

impl UiaEl {
    fn read_node(&self) -> Node {
        let info = self.info();
        let role = role_from_uia(
            info.control_type,
            info.aria_role.as_deref(),
            Some(info.class_name.as_str()),
        );
        let secure = info.is_password;
        let scale = uia::foreground_scale();
        let bounds = info.bounds.map(|b| scale_rect(b, scale));
        let multiline = match info.control_type {
            ct::EDIT => Some(
                info.aria_properties.contains("multiline=true")
                    || info.class_name.to_ascii_lowercase().contains("richedit")
                    || bounds.is_some_and(|b| b.height > 40.0),
            ),
            ct::DOCUMENT => Some(true),
            ct::COMBO_BOX => Some(false),
            _ => None,
        };
        let mut node = Node {
            role: if role == Role::TextField && multiline == Some(true) {
                Role::TextArea
            } else {
                role
            },
            raw_role: info.control_type.to_string(),
            name: (!info.name.is_empty()).then(|| info.name.clone()),
            description: None,
            states: States {
                focused: info.has_focus,
                editable: false,
                secure,
                multiline,
                selected: false,
                offscreen: info.offscreen,
                ..States::default()
            },
            bounds,
            ..Default::default()
        };
        computer::apply_caps(&mut node.states, &self.caps());
        // Chromium and Firefox put the placeholder in HelpText for inputs.
        if !info.help_text.is_empty() {
            if role.is_text_input() {
                node.placeholder = Some(info.help_text.clone());
            } else {
                node.description = Some(info.help_text.clone());
            }
        }
        if !info.automation_id.is_empty() {
            node.attrs
                .insert("identifier".into(), info.automation_id.clone());
        }
        if !info.class_name.is_empty() {
            node.attrs.insert("class".into(), info.class_name.clone());
        }
        if !secure {
            match self.value_pattern() {
                Some((value, read_only)) => {
                    // Chromium's document value is the page URL, not content.
                    if role == Role::WebArea && looks_like_url(&value) {
                        node.attrs.insert("url".into(), value);
                    } else if !value.is_empty() {
                        node.value = Some(value);
                    }
                    node.states.editable = !read_only
                        && matches!(info.control_type, ct::EDIT | ct::COMBO_BOX | ct::DOCUMENT);
                }
                None => {
                    if matches!(info.control_type, ct::EDIT | ct::DOCUMENT) {
                        if let Some(t) = self.document_text(crate::thread::MAX_MESSAGE_BYTES) {
                            node.value = (!t.is_empty()).then_some(t);
                        }
                        // A focused edit or document with a text pattern accepts typing.
                        node.states.editable = info.is_keyboard_focusable;
                    }
                }
            }
        } else {
            node.states.editable = true;
        }
        node
    }
}

impl Element for UiaEl {
    fn node(&self) -> Node {
        self.read_node()
    }

    fn children(&self, max: usize) -> Vec<Self> {
        self.child_elements(max)
    }

    fn parent(&self) -> Option<Self> {
        self.parent_element()
    }

    fn same(&self, other: &Self) -> bool {
        self.is_same(other)
    }

    fn actions(&self) -> Vec<UiActionKind> {
        computer::actions(self)
    }

    fn perform(&self, action: &UiAction) -> Result<()> {
        computer::perform(self, action)
    }
}

impl Platform for UiaPlatform {
    type Element = UiaEl;

    fn permission(&self) -> Permission {
        Permission::NotRequired
    }

    fn request_permission(&self) -> Result<()> {
        Ok(())
    }

    fn focus(&self) -> Result<Option<Target<UiaEl>>> {
        let Some(fg) = uia::foreground()? else {
            return Ok(None);
        };
        let Some(el) = UiaEl::focused()? else {
            return Ok(None);
        };
        if el.info().process_id != fg.pid {
            // Focus is in another process than the foreground window (a tool window, or a
            // race while windows switch): not the user's target.
            return Ok(None);
        }
        Ok(Some(Target {
            app: AppInfo {
                id: fg.exe.clone(),
                name: fg
                    .exe
                    .as_deref()
                    .map(|e| {
                        e.trim_end_matches(".exe")
                            .trim_end_matches(".EXE")
                            .to_string()
                    })
                    .unwrap_or_default(),
                window_title: (!fg.title.is_empty()).then_some(fg.title),
                url: None,
            },
            element: el,
        }))
    }

    fn read_field(&self, el: &UiaEl, max_value_bytes: usize) -> Result<FieldRead> {
        let node = el.read_node();
        let label = el.labeled_by_name();
        let state = if node.states.secure {
            FieldState::default()
        } else {
            let raw = el
                .value_pattern()
                .map(|(v, _)| v)
                .filter(|v| !(node.role == Role::WebArea && looks_like_url(v)))
                .or_else(|| el.document_text(max_value_bytes))
                .unwrap_or_default();
            let value = sanitize::clean_value(&raw, max_value_bytes);
            let truncated = value.len() < raw.len();
            FieldState {
                value,
                selection: el.selection_offsets(),
                truncated,
            }
        };
        Ok(FieldRead { node, label, state })
    }

    fn caret_bounds(&self, el: &UiaEl, _state: &FieldState) -> Option<Rect> {
        el.caret_rect()
            .map(|r| scale_rect(r, uia::foreground_scale()))
    }

    fn insert_text(&self, el: &UiaEl, text: &str, before: &FieldState) -> Result<()> {
        let info = el.info();
        if !matches!(info.control_type, ct::EDIT | ct::COMBO_BOX) || before.truncated {
            return Err(Error::Unsupported(
                "ValuePattern insertion only for plain edit controls".into(),
            ));
        }
        let Some((_, read_only)) = el.value_pattern() else {
            return Err(Error::Unsupported("no ValuePattern".into()));
        };
        if read_only {
            return Err(Error::Unsupported("read-only value".into()));
        }
        let expected = verify::expected_after_insert(before, text)
            .ok_or_else(|| Error::Unsupported("selection unknown".into()))?;
        el.set_value(&expected)?;
        if let Some((s, _)) = before.selection {
            let caret = s.min(before.selection.map_or(s, |(_, e)| e)) + text.chars().count() as u32;
            el.set_caret(caret);
        }
        Ok(())
    }

    fn clipboard(&self) -> Result<Box<dyn crate::clipboard::Clipboard>> {
        Ok(Box::new(clipboard::Win32Clipboard::new()?))
    }

    fn paste(&self, _el: &UiaEl) -> Result<()> {
        uia::send_ctrl_v()
    }

    fn apps(&self) -> Result<Vec<AppEntry>> {
        computer::apps()
    }

    fn windows(&self) -> Result<Vec<PlatformWindow<UiaEl>>> {
        computer::windows()
    }

    fn key_input(&self, input: &KeyInput) -> Result<()> {
        computer::key_input(input)
    }

    fn open_app(&self, name: &str) -> Result<()> {
        computer::open_app(name)
    }
}
