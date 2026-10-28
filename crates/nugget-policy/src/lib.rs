//! Safety policy for actions Nugget takes on the user's behalf (computer use, sending, booking).
//!
//! **Contract (M4):** [`ActionDescriptor`], [`RiskClass`], [`Decision`] and [`PolicyGate`]. Work
//! package M4-B implements the policy engine (allow/deny lists per app and site, a risk classifier,
//! confirmation requirements, the kill switch and the tamper-evident action log). Every executor
//! (computer use, browser, proactive actions) asks a [`PolicyGate`] before acting.

use serde::{Deserialize, Serialize};

/// What kind of consequence an action can have. Higher classes need stronger consent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    /// Reading, navigating, scrolling, focusing.
    Observe,
    /// Typing into a field, selecting, opening a draft (undoable, nothing leaves the device).
    Edit,
    /// Sends something to another person or service (email, message, form submit, post).
    Send,
    /// Removes or overwrites data (delete, archive, overwrite file, cancel subscription).
    Destroy,
    /// Spends money or commits to an obligation (purchase, booking, payment, signing).
    Spend,
    /// Changes security or system state (passwords, permissions, installs, settings).
    Security,
}

/// A proposed action, described for the policy engine and for the user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionDescriptor {
    /// App id or site host the action targets.
    pub target_app: String,
    /// Executor-specific verb, e.g. `click`, `set_value`, `submit`, `send_email`.
    pub verb: String,
    /// Human-readable label of the element or object acted on.
    pub target_label: Option<String>,
    /// Short preview of text that would be typed or sent.
    pub text_preview: Option<String>,
    /// The executor's own estimate; the policy engine may raise it, never lower it.
    pub declared_risk: RiskClass,
    /// The task this action belongs to (for audit and per-task approvals).
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", content = "reason", rename_all = "snake_case")]
pub enum Decision {
    Allow,
    /// Ask the user first; the string explains what will happen.
    Confirm(String),
    Deny(String),
}

/// Asked before every action. Implementations must be fast and must not block on the user;
/// confirmation is the executor's job when `Confirm` is returned.
pub trait PolicyGate: Send + Sync {
    fn evaluate(&self, action: &ActionDescriptor) -> Decision;
    /// Whether the global kill switch is engaged (all executors must stop at once).
    fn halted(&self) -> bool;
}
