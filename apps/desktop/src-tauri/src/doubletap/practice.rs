//! The practice field on the Double Tap page: a sandbox target for the real trigger, so people
//! learn their shortcut without touching another app.
//!
//! While the field has focus the page sends its state ([`PracticeField`]). When the trigger fires
//! and Nugget's main window is in front with that state set, the flow drafts for real (the same
//! `draft.begin`, the user's chosen provider) from a sandbox context: a sample message to answer
//! and the practice field's text. Nothing is read from or inserted into another app: Insert sends
//! the text to the page ([`Action::Insert`]). Practice drafts send no `draft.feedback` (they are
//! not the user's voice) and are never kept in the history.

use nugget_core::ipc::{AppInfo, DraftContext, DraftRequest, FieldInfo, ThreadMessage};
use serde::{Deserialize, Serialize};

/// The app id practice drafts are made for.
pub const APP_ID: &str = "app.nugget.practice";
/// Caps on what the page sends (characters).
const MAX_VALUE: usize = 10_000;
const MAX_SAMPLE: usize = 1_000;

/// The practice field's rectangle, CSS pixels relative to the page's viewport.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The practice field while it has focus (command `doubletap_practice`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PracticeField {
    pub value: String,
    /// UTF-16 offsets as the page reports them (`selectionStart`, `selectionEnd`); converted to
    /// character offsets, as `FieldInfo::selection` wants, by [`PracticeField::validated`].
    /// `None` is a caret at the end.
    #[serde(default)]
    pub selection: Option<(u32, u32)>,
    pub rect: Rect,
    #[serde(default)]
    pub rtl: bool,
    /// The sample message to answer, in the UI language.
    pub sample: String,
}

/// What the shell asks of the practice field (event `doubletap:practice`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    Insert { text: String },
    Focus,
}

/// The character offset of UTF-16 offset `u` in `s` (clamped to the end; inside a surrogate
/// pair rounds up).
fn utf16_to_chars(s: &str, u: u32) -> u32 {
    let mut units = 0u32;
    for (i, c) in s.chars().enumerate() {
        if units >= u {
            return u32::try_from(i).unwrap_or(u32::MAX);
        }
        units = units.saturating_add(u32::try_from(c.len_utf16()).unwrap_or(2));
    }
    u32::try_from(s.chars().count()).unwrap_or(u32::MAX)
}

fn cap(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

impl PracticeField {
    /// Within the caps, with finite coordinates and a selection inside the text.
    pub fn validated(mut self) -> Result<Self, String> {
        let r = self.rect;
        if ![r.x, r.y, r.width, r.height].iter().all(|v| v.is_finite()) {
            return Err("the practice field's position is not a number".into());
        }
        self.value = cap(&self.value, MAX_VALUE);
        self.sample = cap(self.sample.trim(), MAX_SAMPLE);
        let value = &self.value;
        self.selection = self
            .selection
            .map(|(a, b)| (utf16_to_chars(value, a), utf16_to_chars(value, b)))
            .map(|(a, b)| (a.min(b), a.max(b)));
        Ok(self)
    }

    pub fn app() -> AppInfo {
        AppInfo {
            id: Some(APP_ID.into()),
            name: "Nugget practice".into(),
            window_title: None,
            url: None,
        }
    }

    /// The sandbox request: the sample message as the thread, the practice text as the field.
    pub fn request(&self, provider: Option<nugget_core::ids::ProviderId>) -> DraftRequest {
        DraftRequest {
            context: DraftContext {
                app: Self::app(),
                field: FieldInfo {
                    role: "text_area".into(),
                    label: Some("Practice reply".into()),
                    placeholder: None,
                    value: self.value.clone(),
                    selection: self.selection,
                    secure: false,
                },
                thread: if self.sample.is_empty() {
                    Vec::new()
                } else {
                    vec![ThreadMessage {
                        author: Some("Sam".into()),
                        text: self.sample.clone(),
                        from_me: Some(false),
                        timestamp: None,
                    }]
                },
                recipients: vec!["Sam".into()],
                subject: None,
                screenshot_png_base64: None,
            },
            instruction: None,
            provider,
            max_tokens: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(value: &str, selection: Option<(u32, u32)>) -> PracticeField {
        PracticeField {
            value: value.into(),
            selection,
            rect: Rect {
                x: 10.0,
                y: 20.0,
                width: 300.0,
                height: 80.0,
            },
            rtl: false,
            sample: "  Can we move our call to Thursday?  ".into(),
        }
    }

    #[test]
    fn validation_caps_and_orders_the_selection() {
        let f = field("héllo", Some((9, 2))).validated().unwrap();
        assert_eq!(f.selection, Some((2, 5)));
        // UTF-16 offsets from the page: the emoji is two units, one character.
        let f = field("a😀b", Some((3, 4))).validated().unwrap();
        assert_eq!(f.selection, Some((2, 3)));
        assert_eq!(f.sample, "Can we move our call to Thursday?");
        let long = field(&"x".repeat(MAX_VALUE + 50), None)
            .validated()
            .unwrap();
        assert_eq!(long.value.chars().count(), MAX_VALUE);
        let mut bad = field("", None);
        bad.rect.x = f64::NAN;
        assert!(bad.validated().is_err());
    }

    #[test]
    fn the_request_is_a_sandbox() {
        let f = field("Sure, ", None).validated().unwrap();
        let r = f.request(None);
        assert_eq!(r.context.app.id.as_deref(), Some(APP_ID));
        assert_eq!(r.context.field.value, "Sure, ");
        assert!(!r.context.field.secure);
        assert_eq!(r.context.thread.len(), 1);
        assert_eq!(r.context.thread[0].from_me, Some(false));
        assert!(r.context.screenshot_png_base64.is_none());
        assert!(r.instruction.is_none());
    }

    #[test]
    fn wire_format() {
        let f: PracticeField = serde_json::from_value(serde_json::json!({
            "value": "a", "selection": [0, 1],
            "rect": { "x": 1, "y": 2, "width": 3, "height": 4 },
            "rtl": true, "sample": "s"
        }))
        .unwrap();
        assert_eq!(f.selection, Some((0, 1)));
        assert!(f.rtl);
        assert_eq!(
            serde_json::to_value(Action::Insert { text: "t".into() }).unwrap(),
            serde_json::json!({ "kind": "insert", "text": "t" })
        );
        assert_eq!(
            serde_json::to_value(Action::Focus).unwrap(),
            serde_json::json!({ "kind": "focus" })
        );
    }
}
