//! Redaction for anything that reaches the log or a confirmation prompt: secure fields are never
//! logged, card numbers and long digit runs are masked, labels and previews are capped.

use serde::{Deserialize, Serialize};

use crate::classify::card_spans;
use crate::text::cap;
use crate::{ActionContext, ActionDescriptor, RiskClass};

/// Longest text preview kept, in characters.
pub const PREVIEW_CAP: usize = 80;
/// Longest label kept, in characters.
pub const LABEL_CAP: usize = 120;
/// Placeholder for the text typed into a secure field.
pub const REDACTED: &str = "[redacted]";

/// An [`ActionDescriptor`] safe to log or show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactedAction {
    pub target_app: String,
    pub verb: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_preview: Option<String>,
    pub declared_risk: RiskClass,
    pub task_id: String,
}

impl RedactedAction {
    pub fn new(action: &ActionDescriptor, ctx: &ActionContext) -> Self {
        let secure =
            ctx.secure_field || action.target_label.as_deref().is_some_and(is_secure_label);
        Self {
            target_app: cap(&action.target_app, LABEL_CAP),
            verb: cap(&action.verb, 64),
            target_label: action
                .target_label
                .as_deref()
                .map(|l| cap(&mask_digits(l), LABEL_CAP)),
            text_preview: action
                .text_preview
                .as_deref()
                .map(|p| redact_action_preview(p, secure, PREVIEW_CAP)),
            declared_risk: action.declared_risk,
            task_id: cap(&action.task_id, LABEL_CAP),
        }
    }
}

/// Whether a field label names a secret (password, PIN, one-time code, card security code, ...),
/// in the languages the classifier knows.
pub fn is_secure_label(label: &str) -> bool {
    crate::classify::is_secure_label_text(label)
}

/// Text that would be typed or sent, made safe to log: `[redacted]` for secure fields, card
/// numbers and long digit runs masked to their last four digits, capped at [`PREVIEW_CAP`].
pub fn redact_preview(preview: &str, secure: bool) -> String {
    redact_preview_capped(preview, secure, PREVIEW_CAP)
}

/// Longest preview a confirmation prompt shows, in characters: room for what a Send or a form
/// submit sends (see [`crate::preview`]).
pub const CONFIRM_PREVIEW_CAP: usize = crate::preview::FIELDS_PREVIEW_MAX + 40;

/// A descriptor's preview made safe to log or show, capped at `max`.
///
/// A fields preview ([`crate::preview::fields_preview`]) lists values the executor has already
/// masked field by field (secrets as `••••`): it is never redacted as a whole because of the
/// target's label ("Send verification code", "Reset password"); digit runs are still masked.
/// A key press's preview (keys, a line break, the fields) is shown as `keys — fields`, each part
/// capped on its own so the fields keep their `+N more fields` tail. Anything else is
/// [`redact_preview_capped`].
pub fn redact_action_preview(preview: &str, secure: bool, max: usize) -> String {
    use crate::preview::{is_fields_preview, split_key_preview};
    match split_key_preview(preview) {
        (keys, Some(sent)) if is_fields_preview(sent) => format!(
            "{} — {}",
            redact_preview_capped(keys, false, 60),
            cap(&mask_digits(sent), max)
        ),
        _ if is_fields_preview(preview) => cap(&mask_digits(preview), max),
        _ => redact_preview_capped(preview, secure, max),
    }
}

/// [`redact_preview`] with its own cap.
pub fn redact_preview_capped(preview: &str, secure: bool, max: usize) -> String {
    if secure {
        return REDACTED.to_string();
    }
    cap(&mask_digits(preview), max)
}

/// Mask card numbers (Luhn-valid) and any run of 9+ digits (account numbers, national ids),
/// keeping the last four digits.
pub fn mask_digits(text: &str) -> String {
    let mut spans: Vec<(usize, usize)> = card_spans(text).collect();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i - start >= 9 && !spans.iter().any(|(s, e)| *s <= start && i <= *e) {
                spans.push((start, i));
            }
        } else {
            i += 1;
        }
    }
    if spans.is_empty() {
        return text.to_string();
    }
    spans.sort_unstable();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (start, end) in spans {
        if start < last {
            continue;
        }
        out.push_str(&text[last..start]);
        let digits: String = text[start..end]
            .chars()
            .filter(char::is_ascii_digit)
            .collect();
        let tail = &digits[digits.len().saturating_sub(4)..];
        out.push_str("••••");
        out.push_str(tail);
        last = end;
    }
    out.push_str(&text[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure_fields_never_logged() {
        let a = ActionDescriptor {
            target_app: "x".into(),
            verb: "type".into(),
            target_label: Some("Password".into()),
            text_preview: Some("hunter2".into()),
            declared_risk: RiskClass::Edit,
            task_id: "t".into(),
        };
        let r = RedactedAction::new(&a, &ActionContext::default());
        assert_eq!(r.text_preview.as_deref(), Some(REDACTED));
        let mut b = a.clone();
        b.target_label = Some("Notes".into());
        let r = RedactedAction::new(&b, &ActionContext::default().with_secure_field(true));
        assert_eq!(r.text_preview.as_deref(), Some(REDACTED));
        let r = RedactedAction::new(&b, &ActionContext::default());
        assert_eq!(r.text_preview.as_deref(), Some("hunter2"));
    }

    #[test]
    fn digits_masked() {
        assert_eq!(
            mask_digits("card 4111 1111 1111 1111 ok"),
            "card ••••1111 ok"
        );
        assert_eq!(mask_digits("acct 123456789012"), "acct ••••9012");
        assert_eq!(mask_digits("call 555 1234"), "call 555 1234");
    }

    #[test]
    fn previews_capped() {
        let long = "a".repeat(500);
        let p = redact_preview(&long, false);
        assert_eq!(p.chars().count(), PREVIEW_CAP + 1);
        assert!(p.ends_with('…'));
    }

    #[test]
    fn secure_labels_multilingual() {
        assert!(is_secure_label("Contraseña"));
        assert!(is_secure_label("パスワード"));
        assert!(is_secure_label("Enter your PIN"));
        assert!(!is_secure_label("Pinned"));
        assert!(!is_secure_label("Subject"));
    }
}
