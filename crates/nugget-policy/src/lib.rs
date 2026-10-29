//! Safety policy for actions Nugget takes on the user's behalf (computer use, sending, booking).
//!
//! **Contract (M4):** [`ActionDescriptor`], [`RiskClass`], [`Decision`] and [`PolicyGate`]. Work
//! package M4-B implements the policy engine (allow/deny lists per app and site, a risk classifier,
//! confirmation requirements, the kill switch and the tamper-evident action log). Every executor
//! (computer use, browser, proactive actions) asks a [`PolicyGate`] before acting.
//!
//! # Pieces
//! - [`PolicyEngine`]: the [`PolicyGate`] implementation. Rules per app and site ([`Rule`]), the
//!   built-in defaults ([`default_rules`]), per-task approvals, rate limits per risk class and the
//!   kill switch. Configuration persists through a [`PolicyStore`] ([`JsonFilePolicyStore`],
//!   [`MemoryPolicyStore`]).
//! - [`RiskClassifier`]: deterministic heuristics that can only *raise* the declared risk.
//! - [`KillSwitch`]: process-wide halt, one atomic load to check. [`KillSwitch::trigger_hook`]
//!   plugs straight into `nugget_hotkey::start`.
//! - [`ConfirmationBroker`]: executors await a user decision on a [`Decision::Confirm`]; a timeout
//!   or the kill switch counts as deny.
//! - [`ActionLog`]: append-only, SHA-256 hash-chained, exportable as JSONL. See [`log`] for the
//!   adapter `nugget-agent` can use.
//!
//! # Precedence (what [`PolicyEngine::assess`] does)
//! 1. Kill switch engaged → `Deny`.
//! 2. Effective risk = max(declared, classified).
//! 3. Any matching `Deny` rule (user or default) → `Deny`. Deny always wins.
//! 4. A per-task approval for this risk class and target → `Allow`.
//! 5. The most specific matching user `Allow`/`Confirm` rule; ties go to `Confirm`.
//! 6. The most specific matching default `Allow`/`Confirm` rule.
//! 7. Nothing matched: `Confirm` for Send and above, `Allow` otherwise.
//! 8. The rate limit for the risk class is exhausted for this task → `Deny`.
//!
//! An `Allow` rule that names no risk classes only covers Observe and Edit: allowing Send,
//! Destroy, Spend or Security needs the class named explicitly.

use serde::{Deserialize, Serialize};

pub mod broker;
pub mod classify;
pub mod engine;
mod glob;
pub mod kill;
pub mod log;
pub mod redact;
pub mod rules;
pub mod store;
mod text;

pub use broker::{
    ConfirmOutcome, ConfirmationBroker, ConfirmationRequest, DenyReason, UserDecision,
};
pub use classify::{Classification, RiskClassifier};
pub use engine::{Assessment, PolicyEngine, PolicyEngineBuilder, RecheckGate, TaskApproval};
pub use glob::glob_match;
pub use kill::KillSwitch;
pub use log::{ActionLog, ChainError, LogEntry, LogEvent};
pub use redact::RedactedAction;
pub use rules::{Effect, PolicyConfig, Rule, default_rules};
pub use store::{JsonFilePolicyStore, MemoryPolicyStore, PolicyStore};
pub use text::{
    fold_confusables, normalize as normalize_text, normalize_spaced as normalize_text_spaced,
};

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

impl RiskClass {
    pub const ALL: [RiskClass; 6] = [
        RiskClass::Observe,
        RiskClass::Edit,
        RiskClass::Send,
        RiskClass::Destroy,
        RiskClass::Spend,
        RiskClass::Security,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RiskClass::Observe => "observe",
            RiskClass::Edit => "edit",
            RiskClass::Send => "send",
            RiskClass::Destroy => "destroy",
            RiskClass::Spend => "spend",
            RiskClass::Security => "security",
        }
    }
}

impl std::fmt::Display for RiskClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
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

impl Decision {
    pub fn is_allow(&self) -> bool {
        matches!(self, Decision::Allow)
    }
}

/// What the executor can see around the action, beyond the [`ActionDescriptor`]. Everything is
/// optional; the more an executor fills in, the better the classifier and the rules can judge
/// (a payment field in the same form, a compose window, the page URL behind a browser tab).
///
/// Build it with [`ActionContext::default`] and the `with_*` methods.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ActionContext {
    /// URL of the page or document the action happens in.
    pub url: Option<String>,
    /// Title of the window or tab.
    pub window_title: Option<String>,
    /// Text of the surrounding dialog, sheet or alert, if any.
    pub dialog_text: Option<String>,
    /// Labels, names or `autocomplete` values of the other fields in the same form.
    pub form_fields: Vec<String>,
    /// The target is a password or other secure text field.
    pub secure_field: bool,
    /// Accessibility role of the target (`button`, `link`, `text_field`, ...).
    pub role: Option<String>,
    /// Whether activating the target submits or commits what the form or dialog holds (a form's
    /// default button, a `submit` input). `None` when the executor cannot tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submits: Option<bool>,
    /// The containers around the target, innermost first (the list item and list a button sits
    /// in, the table row, the toolbar). Tells a row's "Remove" in a cart from a "Delete" on its
    /// own, and a message row named after its subject from a command.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ancestors: Vec<ContextNode>,
}

/// One container in [`ActionContext::ancestors`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextNode {
    /// Accessibility role, in the same vocabulary as [`ActionContext::role`].
    pub role: String,
    /// Its accessible name, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ContextNode {
    pub fn new(role: impl Into<String>, name: Option<String>) -> Self {
        Self {
            role: role.into(),
            name,
        }
    }
}

impl ActionContext {
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }
    pub fn with_window_title(mut self, title: impl Into<String>) -> Self {
        self.window_title = Some(title.into());
        self
    }
    pub fn with_dialog_text(mut self, text: impl Into<String>) -> Self {
        self.dialog_text = Some(text.into());
        self
    }
    pub fn with_form_fields<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.form_fields = fields.into_iter().map(Into::into).collect();
        self
    }
    pub fn with_secure_field(mut self, secure: bool) -> Self {
        self.secure_field = secure;
        self
    }
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.role = Some(role.into());
        self
    }
    pub fn with_submits(mut self, submits: bool) -> Self {
        self.submits = Some(submits);
        self
    }
    pub fn with_ancestors<I>(mut self, ancestors: I) -> Self
    where
        I: IntoIterator<Item = ContextNode>,
    {
        self.ancestors = ancestors.into_iter().collect();
        self
    }
}

/// Asked before every action. Implementations must be fast and must not block on the user;
/// confirmation is the executor's job when `Confirm` is returned.
pub trait PolicyGate: Send + Sync {
    fn evaluate(&self, action: &ActionDescriptor) -> Decision;
    /// Whether the global kill switch is engaged (all executors must stop at once).
    fn halted(&self) -> bool;
    /// Like [`PolicyGate::evaluate`], with what the executor can see around the action. Executors
    /// that know the form, dialog or page should call this one. The default ignores the context.
    fn evaluate_with_context(
        &self,
        action: &ActionDescriptor,
        context: &ActionContext,
    ) -> Decision {
        let _ = context;
        self.evaluate(action)
    }
    /// A user approved this `Confirm`: count it against the task's rate limit. `false` means
    /// the limit is reached and the action must be denied.
    fn record_approved(&self, action: &ActionDescriptor, risk: RiskClass) -> bool {
        let _ = (action, risk);
        true
    }
}
