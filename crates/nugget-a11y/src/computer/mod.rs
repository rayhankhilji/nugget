//! Computer use (M4): drive any app through its accessibility tree instead of guessing at pixels.
//!
//! **Contract.** [`ComputerUse`] and the types below are what `nugget-computer` (the agent tools)
//! and the bench build on. [`Backend`](crate::Backend) implements it over every
//! [`Platform`](crate::Platform); [`Unsupported`](crate::Unsupported) refuses everything.
//!
//! - **Enumerate** running apps ([`AppEntry`]) and their top-level windows ([`WindowInfo`]).
//! - **Snapshot** one window as a bounded [`UiTree`] of [`UiNode`]s: role, name, value (never
//!   for a secure element), bounds, [`States`], the [`UiActionKind`]s it supports, and a
//!   **stable [`ElementId`]**: the same element gets the same id in the next snapshot as long as
//!   its path of (role, identifier or name, ordinal among like siblings) from the window is
//!   unchanged. Ids are resolved back to live elements by re-walking that path, so a stale id
//!   fails with `NotFound` instead of hitting some other element.
//! - **Act** on an element by id ([`UiAction`]), send **key input** ([`KeyInput`]: typed text or
//!   named keys, never raw scan codes) to the focused element, **open** (launch or activate) an
//!   app. Every action re-reads the element (or app) afterwards and reports in
//!   [`ActionOutcome::verified`] whether the intended effect is visible.
//! - Policy, in the platform-neutral layer: denied apps (the [`DenyList`](crate::DenyList)) are
//!   neither listed nor read nor acted on; secure values are never read and never set; every
//!   string is sanitised; walks are bounded by [`TreeLimits`].

mod backend;
mod ids;
mod keys;

use nugget_core::ipc::AppInfo;
use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};

pub use ids::{IdKey, element_id};
pub use keys::{Key, KeyChord, Modifiers, NamedKey};

use crate::role::Role;
use crate::tree::States;
use crate::{Permission, Rect};

/// A stable element id, unique within the snapshots of one backend (`e` + a short hash).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ElementId(pub String);

impl ElementId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ElementId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A window id, stable while the window exists (`w` + a short hash of the platform's handle).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WindowId(pub String);

impl WindowId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WindowId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A running app with at least one window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppEntry {
    /// Bundle id / AppUserModelID / desktop id when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default)]
    pub frontmost: bool,
}

/// A top-level window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: WindowId,
    pub app: AppEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Rect>,
    /// The focused window of the frontmost app.
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub minimized: bool,
}

/// What can be done to an element, as advertised by the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiActionKind {
    /// Press, click, toggle, activate (AXPress, Invoke/Toggle, AT-SPI action 0).
    Press,
    /// Replace the value (editable text, sliders).
    SetValue,
    /// Select this item (list item, tab, option, radio, menu item).
    Select,
    Focus,
    Scroll,
    /// Expand or collapse (tree items, disclosure, combo boxes, menus).
    Expand,
}

impl UiActionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UiActionKind::Press => "press",
            UiActionKind::SetValue => "set_value",
            UiActionKind::Select => "select",
            UiActionKind::Focus => "focus",
            UiActionKind::Scroll => "scroll",
            UiActionKind::Expand => "expand",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

/// An action on one element.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum UiAction {
    Press,
    SetValue {
        value: String,
    },
    Select,
    Focus,
    /// Scroll the element (or its nearest scrollable ancestor) by `amount` platform steps
    /// (lines or wheel clicks); `0` means "scroll this element into view".
    Scroll {
        direction: ScrollDirection,
        amount: u32,
    },
    Expand {
        expand: bool,
    },
}

impl UiAction {
    pub fn kind(&self) -> UiActionKind {
        match self {
            UiAction::Press => UiActionKind::Press,
            UiAction::SetValue { .. } => UiActionKind::SetValue,
            UiAction::Select => UiActionKind::Select,
            UiAction::Focus => UiActionKind::Focus,
            UiAction::Scroll { .. } => UiActionKind::Scroll,
            UiAction::Expand { .. } => UiActionKind::Expand,
        }
    }
}

/// Keyboard input to the focused element: literal text, or named keys / chords.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum KeyInput {
    /// Typed as text (Unicode), never interpreted as shortcuts. `\r` is dropped; `\n` is typed
    /// only into multi-line fields.
    Text(String),
    /// Named keys or chords, pressed in order (`enter`, `ctrl+a`, `shift+tab`).
    Keys(Vec<KeyChord>),
}

/// One element of a [`UiTree`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiNode {
    pub id: ElementId,
    pub role: Role,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub raw_role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Never set for a secure element.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Rect>,
    pub states: States,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<UiActionKind>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<UiNode>,
}

impl Default for ElementId {
    fn default() -> Self {
        ElementId(String::new())
    }
}

impl UiNode {
    /// Depth-first pre-order iterator over this node and its descendants.
    pub fn iter(&self) -> impl Iterator<Item = &UiNode> {
        let mut stack = vec![self];
        std::iter::from_fn(move || {
            let n = stack.pop()?;
            stack.extend(n.children.iter().rev());
            Some(n)
        })
    }

    pub fn find(&self, id: &ElementId) -> Option<&UiNode> {
        self.iter().find(|n| &n.id == id)
    }
}

/// A bounded snapshot of one window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiTree {
    pub app: AppInfo,
    pub window: WindowInfo,
    pub root: UiNode,
    /// The focused element, when it is inside this window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<ElementId>,
    /// The walk stopped early (node budget, depth or deadline).
    #[serde(default)]
    pub truncated: bool,
}

impl UiTree {
    pub fn find(&self, id: &ElementId) -> Option<&UiNode> {
        self.root.find(id)
    }

    pub fn count(&self) -> usize {
        self.root.iter().count()
    }
}

/// Bounds on a window snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TreeLimits {
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_children: usize,
    pub deadline: std::time::Duration,
    /// Skip off-screen subtrees (their root is kept, marked `offscreen`).
    pub skip_offscreen: bool,
}

impl Default for TreeLimits {
    fn default() -> Self {
        TreeLimits {
            max_nodes: 800,
            max_depth: 40,
            max_children: 200,
            deadline: std::time::Duration::from_millis(1500),
            skip_offscreen: true,
        }
    }
}

/// Which window to snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WindowSel {
    /// The focused window of the frontmost app.
    #[default]
    Focused,
    Id(WindowId),
    /// The first window of the first app whose name or id matches (case-insensitive).
    App(String),
}

/// What an action did, judged by re-reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionOutcome {
    /// The intended effect was observed after the action (value set, item selected, focus
    /// moved, app frontmost, or for a press: the element or its window visibly changed).
    pub verified: bool,
    /// Human-readable account of what was observed (sanitised; may contain screen text).
    pub detail: String,
    /// The element before and after (shallow, no children), when it could be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<UiNode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<UiNode>,
}

/// Computer use over accessibility. Object safe; `Send + Sync`.
pub trait ComputerUse: Send + Sync {
    fn permission(&self) -> Permission;
    /// Running apps with windows, frontmost first. Denied apps are left out.
    fn apps(&self) -> Result<Vec<AppEntry>>;
    /// Top-level windows, the focused one first. Denied apps are left out.
    fn windows(&self) -> Result<Vec<WindowInfo>>;
    /// A bounded snapshot of one window. `NotFound` when there is no such window; `Invalid` when
    /// its app is denied.
    fn snapshot(&self, window: &WindowSel, limits: &TreeLimits) -> Result<UiTree>;
    /// Act on an element from a recent snapshot. `NotFound` when the id is unknown or stale.
    fn act(&self, id: &ElementId, action: &UiAction) -> Result<ActionOutcome>;
    /// Key input to `target` (focused first) or, with `None`, to whatever has focus.
    fn key_input(&self, target: Option<&ElementId>, input: &KeyInput) -> Result<ActionOutcome>;
    /// Launch the app by name or id, or bring it to the front when it is running.
    fn open_app(&self, name: &str) -> Result<ActionOutcome>;
    /// A PNG of the window, for the screenshot fallback. `Ok(None)` when unsupported.
    fn screenshot(&self, _window: &WindowSel) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
}

fn unsupported<T>() -> Result<T> {
    Err(Error::Unsupported(
        "computer use is not available on this platform".into(),
    ))
}

impl ComputerUse for crate::Unsupported {
    fn permission(&self) -> Permission {
        Permission::NotRequired
    }
    fn apps(&self) -> Result<Vec<AppEntry>> {
        Ok(Vec::new())
    }
    fn windows(&self) -> Result<Vec<WindowInfo>> {
        Ok(Vec::new())
    }
    fn snapshot(&self, _window: &WindowSel, _limits: &TreeLimits) -> Result<UiTree> {
        unsupported()
    }
    fn act(&self, _id: &ElementId, _action: &UiAction) -> Result<ActionOutcome> {
        unsupported()
    }
    fn key_input(&self, _t: Option<&ElementId>, _i: &KeyInput) -> Result<ActionOutcome> {
        unsupported()
    }
    fn open_app(&self, _name: &str) -> Result<ActionOutcome> {
        unsupported()
    }
}
