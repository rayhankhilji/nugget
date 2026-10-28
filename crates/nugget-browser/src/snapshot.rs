//! The DOM snapshot the extension returns (`extensions/browser/src/protocol.ts` mirrors these
//! types field for field).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Why a field's value is never read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitive {
    Password,
    Payment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
    pub selected: bool,
}

/// One interactive element. `id` is stable for the element's lifetime within one document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElementInfo {
    pub id: String,
    pub role: String,
    pub name: String,
    pub tag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    /// Never present for [`sensitive`](Self::sensitive) fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<SelectOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitive: Option<Sensitive>,
    pub disabled: bool,
    pub editable: bool,
    pub focused: bool,
    pub visible: bool,
    pub rect: Rect,
    /// Id of the owning form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<String>,
    /// Id of the same-origin frame the element lives in; `None` for the top document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<String>,
    #[serde(default)]
    pub in_shadow: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameInfo {
    pub id: String,
    pub origin: String,
    pub same_origin: bool,
    /// Whether the frame's contents are in the snapshot (same-origin frames only).
    pub traversed: bool,
    pub rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Viewport {
    pub width: f64,
    pub height: f64,
    pub scroll_x: f64,
    pub scroll_y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageSnapshot {
    pub snapshot_id: String,
    pub url: String,
    pub title: String,
    pub captured_at_ms: u64,
    pub viewport: Viewport,
    pub elements: Vec<ElementInfo>,
    #[serde(default)]
    pub elements_truncated: bool,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub text_truncated: bool,
    #[serde(default)]
    pub frames: Vec<FrameInfo>,
}

impl PageSnapshot {
    pub fn element(&self, id: &str) -> Option<&ElementInfo> {
        self.elements.iter().find(|e| e.id == id)
    }

    /// The page's host (`example.com`), which is what policy allow/deny lists match on.
    pub fn host(&self) -> Option<String> {
        host_of(&self.url)
    }

    /// Elements of the form with id `form`.
    pub fn form_elements<'a>(&'a self, form: &'a str) -> impl Iterator<Item = &'a ElementInfo> {
        self.elements
            .iter()
            .filter(move |e| e.form.as_deref() == Some(form))
    }
}

/// Lower-cased host of an http(s) URL.
pub fn host_of(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    match u.scheme() {
        "http" | "https" => u.host_str().map(str::to_ascii_lowercase),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElementChange {
    pub id: String,
    pub fields: Vec<String>,
}

/// What changed between the snapshot an action was validated against and the one after it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SnapshotDiff {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_changed: Option<Change>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_changed: Option<Change>,
    #[serde(default)]
    pub added: Vec<String>,
    #[serde(default)]
    pub removed: Vec<String>,
    #[serde(default)]
    pub changed: Vec<ElementChange>,
    #[serde(default)]
    pub text_changed: bool,
}

impl SnapshotDiff {
    pub fn is_empty(&self) -> bool {
        self.url_changed.is_none()
            && self.title_changed.is_none()
            && !self.text_changed
            && self.added.is_empty()
            && self.removed.is_empty()
            && self.changed.is_empty()
    }

    pub fn changed_fields(&self, id: &str) -> &[String] {
        self.changed
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.fields.as_slice())
            .unwrap_or(&[])
    }
}
