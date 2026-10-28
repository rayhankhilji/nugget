//! Cross-platform accessibility: read the focused text field and the conversation around it, and
//! insert text at the caret with read-back verification.
//!
//! **Contract (M3; extended for computer use in M4):** [`Accessibility`], [`system`], and the
//! types below. Backends: macOS AX, Windows UI Automation, Linux AT-SPI. Work package M3-A
//! implements them; this file holds the stable surface and a no-op fallback.
//!
//! Rules every backend follows:
//! - Never read the value of a secure (password) field; report `secure: true` instead.
//! - Read only the focused window of the frontmost app.
//! - Insertion prefers the accessibility API (insert at selection), verifies by reading the field
//!   back, and falls back to a clipboard paste that saves and restores every pasteboard type.

use nugget_core::Result;
use nugget_core::ipc::{AppInfo, FieldInfo, ThreadMessage};

/// Screen rectangle in logical points, origin top-left of the primary display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// The focused editable field and its surroundings.
#[derive(Debug, Clone, PartialEq)]
pub struct Focused {
    pub app: AppInfo,
    pub field: FieldInfo,
    /// Caret (or selection) bounds, for placing the overlay.
    pub caret: Option<Rect>,
    /// Messages around the field, oldest first (empty when none are readable).
    pub thread: Vec<ThreadMessage>,
    pub recipients: Vec<String>,
    pub subject: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Granted,
    /// The user must grant it (macOS Accessibility; Linux AT-SPI disabled).
    Denied,
    /// Not applicable on this platform.
    NotRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertMethod {
    Accessibility,
    /// Clipboard + paste keystroke, with every pasteboard type restored afterwards.
    Clipboard,
    /// Synthesised typing (last resort; never used where Return would send a message early).
    Typing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertOutcome {
    pub method: InsertMethod,
    /// The field was read back and contains the inserted text where expected.
    pub verified: bool,
}

/// Platform accessibility backend.
pub trait Accessibility: Send + Sync {
    fn permission(&self) -> Permission;
    /// Ask the OS to show its permission prompt / settings pane, if any.
    fn request_permission(&self) -> Result<()>;
    /// The focused editable field, or `None` when focus is not in a text field.
    fn focused(&self) -> Result<Option<Focused>>;
    /// Insert `text` at the caret of the focused field (replacing the selection).
    fn insert(&self, text: &str) -> Result<InsertOutcome>;
}

/// No-op backend for unsupported platforms and tests.
#[derive(Debug, Default)]
pub struct Unsupported;

impl Accessibility for Unsupported {
    fn permission(&self) -> Permission {
        Permission::NotRequired
    }
    fn request_permission(&self) -> Result<()> {
        Ok(())
    }
    fn focused(&self) -> Result<Option<Focused>> {
        Ok(None)
    }
    fn insert(&self, _text: &str) -> Result<InsertOutcome> {
        Err(nugget_core::Error::Unsupported(
            "accessibility is not available on this platform".into(),
        ))
    }
}

/// The backend for the current OS.
pub fn system() -> Box<dyn Accessibility> {
    Box::new(Unsupported)
}
