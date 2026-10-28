//! Typed browser actions: validation against the latest snapshot, risk classification and the
//! [`ActionDescriptor`] the policy gate sees, and post-action verification.
//!
//! The extension validates again in the page (`extensions/browser/src/actions.ts`); validating
//! here as well means a bad action is refused before the policy gate or the user is asked about it.

use nugget_policy::{ActionDescriptor, RiskClass};
use serde::{Deserialize, Serialize};

use crate::snapshot::{ElementInfo, PageSnapshot, Sensitive, SnapshotDiff, host_of};

/// Longest text a single `type` action may enter (matches the extension).
pub const MAX_TYPE_CHARS: usize = 10_000;
/// Longest `wait_for` timeout (matches the extension).
pub const MAX_WAIT_MS: u64 = 15_000;
/// How much of typed text the policy gate and the user see in a preview.
const PREVIEW_CHARS: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitCondition {
    TextPresent(String),
    ElementPresent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    UrlContains(String),
    ElementGone(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrowserAction {
    Click {
        element: String,
    },
    Type {
        element: String,
        text: String,
        /// Replace the current value (default) or append to it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clear: Option<bool>,
    },
    Select {
        element: String,
        /// Option value or visible label.
        value: String,
    },
    Scroll {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        element: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dx: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dy: Option<f64>,
    },
    Navigate {
        url: String,
    },
    /// Submit the form that `element` belongs to.
    Submit {
        element: String,
    },
    WaitFor {
        until: WaitCondition,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("take a snapshot of the page first")]
    NoSnapshot,
    #[error("element {0} is not in the latest snapshot")]
    UnknownElement(String),
    #[error("element {0} is not visible")]
    NotVisible(String),
    #[error("element {0} is disabled")]
    Disabled(String),
    #[error("element {0} is not editable")]
    NotEditable(String),
    #[error("element {id} is a {kind:?} field; Nugget never fills password or payment fields")]
    SensitiveField { id: String, kind: Sensitive },
    #[error("element {id} has no option {value:?}")]
    BadOption { id: String, value: String },
    #[error("element {0} is not inside a form")]
    NotInForm(String),
    #[error("bad URL: {0}")]
    BadUrl(String),
    #[error("{0}")]
    Invalid(String),
}

impl BrowserAction {
    /// Policy verb for this action.
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Click { .. } => "click",
            Self::Type { .. } => "set_value",
            Self::Select { .. } => "select",
            Self::Scroll { .. } => "scroll",
            Self::Navigate { .. } => "navigate",
            Self::Submit { .. } => "submit",
            Self::WaitFor { .. } => "wait_for",
        }
    }

    /// The element the action targets, if any.
    pub fn element(&self) -> Option<&str> {
        match self {
            Self::Click { element }
            | Self::Type { element, .. }
            | Self::Select { element, .. }
            | Self::Submit { element } => Some(element),
            Self::Scroll { element, .. } => element.as_deref(),
            Self::WaitFor {
                until: WaitCondition::ElementGone(element),
                ..
            } => Some(element),
            Self::Navigate { .. } | Self::WaitFor { .. } => None,
        }
    }

    /// Whether the action needs a snapshot to validate against. Only `navigate` does not (it
    /// can start from a blank tab).
    pub fn needs_snapshot(&self) -> bool {
        !matches!(self, Self::Navigate { .. })
    }

    /// Check the action against `snapshot`, the latest view of the page.
    pub fn validate(&self, snapshot: Option<&PageSnapshot>) -> Result<(), ValidationError> {
        if let Self::Navigate { url } = self {
            resolve_url(url, snapshot.map(|s| s.url.as_str()))?;
            return Ok(());
        }
        let snap = snapshot.ok_or(ValidationError::NoSnapshot)?;
        let el = |id: &str| {
            snap.element(id)
                .ok_or_else(|| ValidationError::UnknownElement(id.to_owned()))
        };
        let usable = |e: &ElementInfo| {
            if !e.visible {
                Err(ValidationError::NotVisible(e.id.clone()))
            } else if e.disabled {
                Err(ValidationError::Disabled(e.id.clone()))
            } else {
                Ok(())
            }
        };
        match self {
            Self::Click { element } => usable(el(element)?),
            Self::Type { element, text, .. } => {
                let e = el(element)?;
                usable(e)?;
                if let Some(kind) = e.sensitive {
                    return Err(ValidationError::SensitiveField {
                        id: e.id.clone(),
                        kind,
                    });
                }
                if !e.editable {
                    return Err(ValidationError::NotEditable(e.id.clone()));
                }
                if text.chars().count() > MAX_TYPE_CHARS {
                    return Err(ValidationError::Invalid(format!(
                        "text longer than {MAX_TYPE_CHARS} characters"
                    )));
                }
                Ok(())
            }
            Self::Select { element, value } => {
                let e = el(element)?;
                usable(e)?;
                // A payment select (card expiry, ...) is a payment field like any other.
                if let Some(kind) = e.sensitive {
                    return Err(ValidationError::SensitiveField {
                        id: e.id.clone(),
                        kind,
                    });
                }
                let options = e
                    .options
                    .as_ref()
                    .ok_or_else(|| ValidationError::NotEditable(e.id.clone()))?;
                if options
                    .iter()
                    .any(|o| &o.value == value || &o.label == value)
                {
                    Ok(())
                } else {
                    Err(ValidationError::BadOption {
                        id: e.id.clone(),
                        value: value.clone(),
                    })
                }
            }
            Self::Scroll { element, dx, dy } => {
                if let Some(id) = element {
                    el(id)?;
                }
                if dx.is_some_and(|v| !v.is_finite()) || dy.is_some_and(|v| !v.is_finite()) {
                    return Err(ValidationError::Invalid(
                        "scroll offsets must be finite".into(),
                    ));
                }
                Ok(())
            }
            Self::Submit { element } => {
                let e = el(element)?;
                if e.form.is_none() {
                    return Err(ValidationError::NotInForm(e.id.clone()));
                }
                if e.disabled {
                    return Err(ValidationError::Disabled(e.id.clone()));
                }
                Ok(())
            }
            Self::WaitFor { until, timeout_ms } => {
                if timeout_ms.is_some_and(|t| t > MAX_WAIT_MS) {
                    return Err(ValidationError::Invalid(format!(
                        "timeout_ms must be at most {MAX_WAIT_MS}"
                    )));
                }
                match until {
                    WaitCondition::ElementGone(id) => el(id).map(|_| ()),
                    WaitCondition::ElementPresent {
                        role: None,
                        name: None,
                    } => Err(ValidationError::Invalid(
                        "element_present needs a role or a name".into(),
                    )),
                    _ => Ok(()),
                }
            }
            Self::Navigate { .. } => unreachable!("handled above"),
        }
    }

    /// The executor's own risk estimate. The policy engine may raise it, never lower it.
    pub fn classify(&self, snapshot: Option<&PageSnapshot>) -> RiskClass {
        let element = |id: &str| snapshot.and_then(|s| s.element(id));
        match self {
            Self::Scroll { .. } | Self::WaitFor { .. } | Self::Navigate { .. } => {
                RiskClass::Observe
            }
            Self::Type { .. } | Self::Select { .. } => RiskClass::Edit,
            Self::Submit { element: id } => {
                let form = element(id).and_then(|e| e.form.as_deref());
                let base = form_risk(snapshot, form).max(RiskClass::Send);
                // A submit button's own label can say more ("Pay now", "Delete account").
                match element(id) {
                    Some(e) if is_button(e) => base.max(keyword_risk(&e.name).unwrap_or(base)),
                    _ => base,
                }
            }
            Self::Click { element: id } => match element(id) {
                Some(e) => click_risk(snapshot, e),
                None => RiskClass::Edit,
            },
        }
    }

    /// Describe the action for the policy gate. `target_app` is the host the action affects:
    /// the destination for `navigate`, otherwise the current page.
    pub fn describe(&self, snapshot: Option<&PageSnapshot>, task_id: &str) -> ActionDescriptor {
        let element = self
            .element()
            .and_then(|id| snapshot.and_then(|s| s.element(id)));
        let target_app = match self {
            Self::Navigate { url } => resolve_url(url, snapshot.map(|s| s.url.as_str()))
                .ok()
                .and_then(|u| host_of(u.as_str())),
            _ => snapshot.and_then(PageSnapshot::host),
        }
        .unwrap_or_default();
        let target_label = match self {
            Self::Navigate { url } => Some(url.clone()),
            Self::WaitFor { until, .. } => Some(describe_wait(until)),
            _ => element.map(|e| {
                if e.name.is_empty() {
                    format!("{} {}", e.role, e.id)
                } else {
                    format!("{} \"{}\"", e.role, e.name)
                }
            }),
        };
        let text_preview = match self {
            Self::Type { text, .. } => Some(preview(text)),
            Self::Select { value, .. } => Some(preview(value)),
            _ => None,
        };
        ActionDescriptor {
            target_app,
            verb: self.verb().to_owned(),
            target_label,
            text_preview,
            declared_risk: self.classify(snapshot),
            task_id: task_id.to_owned(),
        }
    }
}

fn describe_wait(until: &WaitCondition) -> String {
    match until {
        WaitCondition::TextPresent(t) => format!("text \"{}\"", preview(t)),
        WaitCondition::ElementPresent { role, name } => format!(
            "{} \"{}\"",
            role.as_deref().unwrap_or("element"),
            name.as_deref().unwrap_or("")
        ),
        WaitCondition::UrlContains(u) => format!("URL containing \"{}\"", preview(u)),
        WaitCondition::ElementGone(id) => format!("{id} to disappear"),
    }
}

fn preview(text: &str) -> String {
    let mut out: String = text.chars().take(PREVIEW_CHARS).collect();
    if text.chars().count() > PREVIEW_CHARS {
        out.push('…');
    }
    out
}

/// Resolve `raw` against `base`, accepting only http(s) URLs without embedded credentials.
pub fn resolve_url(raw: &str, base: Option<&str>) -> Result<url::Url, ValidationError> {
    let parsed = match base.and_then(|b| url::Url::parse(b).ok()) {
        Some(b) => b.join(raw),
        None => url::Url::parse(raw),
    }
    .map_err(|e| ValidationError::BadUrl(format!("{raw}: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ValidationError::BadUrl(format!(
            "scheme {} is not allowed",
            parsed.scheme()
        )));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ValidationError::BadUrl(
            "credentials in URLs are not allowed".into(),
        ));
    }
    Ok(parsed)
}

fn is_button(e: &ElementInfo) -> bool {
    e.role == "button"
}

/// Risk implied by the fields of a form: payment fields mean money, password fields mean
/// credentials.
fn form_risk(snapshot: Option<&PageSnapshot>, form: Option<&str>) -> RiskClass {
    let (Some(snap), Some(form)) = (snapshot, form) else {
        return RiskClass::Edit;
    };
    snap.form_elements(form)
        .filter_map(|e| e.sensitive)
        .map(|s| match s {
            Sensitive::Payment => RiskClass::Spend,
            Sensitive::Password => RiskClass::Security,
        })
        .max()
        .unwrap_or(RiskClass::Edit)
}

fn click_risk(snapshot: Option<&PageSnapshot>, e: &ElementInfo) -> RiskClass {
    match e.role.as_str() {
        "link" => keyword_risk(&e.name).unwrap_or(RiskClass::Observe),
        "button" | "menuitem" => {
            let submits = e.form.is_some()
                && (e.tag == "button" && e.input_type.is_none()
                    || matches!(e.input_type.as_deref(), Some("submit" | "image")));
            let base = if submits {
                form_risk(snapshot, e.form.as_deref()).max(RiskClass::Send)
            } else {
                RiskClass::Edit
            };
            keyword_risk(&e.name).map_or(base, |k| k.max(base))
        }
        _ => RiskClass::Edit,
    }
}

const SPEND: &[&str] = &[
    "buy",
    "pay",
    "purchase",
    "place order",
    "checkout",
    "check out",
    "subscribe",
    "book now",
    "reserve",
    "donate",
    "upgrade",
    "add funds",
    "transfer",
    "confirm order",
    "complete order",
];
const SECURITY: &[&str] = &[
    "password",
    "two-factor",
    "2fa",
    "authorize",
    "grant access",
    "allow access",
    "install",
    "security",
    "permissions",
    "api key",
    "revoke",
];
const DESTROY: &[&str] = &[
    "delete",
    "remove",
    "discard",
    "erase",
    "cancel subscription",
    "close account",
    "unsubscribe",
    "archive",
    "empty trash",
    "deactivate",
    "overwrite",
];
const SEND: &[&str] = &[
    "send", "post", "publish", "submit", "share", "reply", "comment", "tweet", "invite", "sign up",
    "register", "apply", "message",
];

/// Risk implied by an element's label, strongest match first.
pub fn keyword_risk(label: &str) -> Option<RiskClass> {
    let l = label.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| contains_word(&l, w));
    if has(SPEND) {
        Some(RiskClass::Spend)
    } else if has(SECURITY) {
        Some(RiskClass::Security)
    } else if has(DESTROY) {
        Some(RiskClass::Destroy)
    } else if has(SEND) {
        Some(RiskClass::Send)
    } else {
        None
    }
}

/// `needle` occurs in `hay` on word boundaries ("pay" matches "Pay now", not "display").
fn contains_word(hay: &str, needle: &str) -> bool {
    hay.match_indices(needle).any(|(i, _)| {
        let before = hay[..i].chars().next_back();
        let after = hay[i + needle.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// Outcome of checking an action's effect in the post-action snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "detail", rename_all = "snake_case")]
pub enum Verification {
    /// The expected effect is visible.
    Confirmed(String),
    /// Nothing visibly changed; the action may have had no effect.
    NoVisibleEffect,
    /// The page shows something other than the expected effect.
    Mismatch(String),
}

impl Verification {
    pub fn is_confirmed(&self) -> bool {
        matches!(self, Self::Confirmed(_))
    }
}

/// Check that `action` did what it should, from the snapshots around it.
pub fn verify(
    action: &BrowserAction,
    before: Option<&PageSnapshot>,
    after: &PageSnapshot,
    diff: &SnapshotDiff,
) -> Verification {
    match action {
        BrowserAction::Type {
            element,
            text,
            clear,
        } => match after.element(element) {
            Some(e) => {
                let value = e.value.as_deref().unwrap_or("");
                let ok = if clear == &Some(false) {
                    value.ends_with(text.as_str())
                } else {
                    value == text
                };
                if ok {
                    Verification::Confirmed(format!("{element} now holds the typed text"))
                } else {
                    Verification::Mismatch(format!("{element} holds {:?}", preview(value)))
                }
            }
            None => Verification::Mismatch(format!("{element} disappeared")),
        },
        BrowserAction::Select { element, value } => match after.element(element) {
            Some(e) => {
                let selected = e
                    .options
                    .iter()
                    .flatten()
                    .find(|o| o.selected)
                    .map(|o| (o.value.as_str(), o.label.as_str()));
                match selected {
                    Some((v, l)) if v == value || l == value => {
                        Verification::Confirmed(format!("{element} is set to {value:?}"))
                    }
                    Some((v, _)) => Verification::Mismatch(format!("{element} is set to {v:?}")),
                    None => Verification::Mismatch(format!("{element} has no selection")),
                }
            }
            None => Verification::Mismatch(format!("{element} disappeared")),
        },
        BrowserAction::Navigate { url } => {
            let want = resolve_url(url, before.map(|s| s.url.as_str())).ok();
            let strip = |u: &str| {
                u.split('#')
                    .next()
                    .unwrap_or(u)
                    .trim_end_matches('/')
                    .to_owned()
            };
            match want {
                Some(w) if strip(&after.url) == strip(w.as_str()) => {
                    Verification::Confirmed(format!("now on {}", after.url))
                }
                // Redirects are normal; any URL change counts, and the gate saw the destination.
                _ if diff.url_changed.is_some() => {
                    Verification::Confirmed(format!("navigated (redirected) to {}", after.url))
                }
                _ => Verification::Mismatch(format!("still on {}", after.url)),
            }
        }
        BrowserAction::Scroll { .. } => {
            let moved = before.is_some_and(|b| {
                b.viewport.scroll_x != after.viewport.scroll_x
                    || b.viewport.scroll_y != after.viewport.scroll_y
            });
            if moved || !diff.is_empty() {
                Verification::Confirmed("view moved".into())
            } else {
                Verification::NoVisibleEffect
            }
        }
        BrowserAction::WaitFor { .. } => Verification::Confirmed("condition met".into()),
        BrowserAction::Click { .. } | BrowserAction::Submit { .. } => {
            if diff.is_empty() {
                Verification::NoVisibleEffect
            } else if let Some(c) = &diff.url_changed {
                Verification::Confirmed(format!("page changed to {}", c.to))
            } else {
                Verification::Confirmed(format!(
                    "{} added, {} removed, {} changed",
                    diff.added.len(),
                    diff.removed.len(),
                    diff.changed.len()
                ))
            }
        }
    }
}
