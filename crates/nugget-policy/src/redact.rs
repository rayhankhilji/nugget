//! Redaction for anything that reaches the log or a confirmation prompt: secure fields are never
//! logged, card numbers and long digit runs are masked, labels and previews are capped.

use serde::{Deserialize, Serialize};

use crate::classify::card_spans;
use crate::text::{Keyword, cap, normalize};
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
                .map(|p| redact_preview(p, secure)),
            declared_risk: action.declared_risk,
            task_id: cap(&action.task_id, LABEL_CAP),
        }
    }
}

/// Whether a field label names a secret (password, PIN, one-time code, card security code, ...),
/// in the languages the classifier knows.
pub fn is_secure_label(label: &str) -> bool {
    use std::sync::OnceLock;
    static KEYWORDS: OnceLock<Vec<Keyword>> = OnceLock::new();
    let kws = KEYWORDS.get_or_init(|| {
        [
            "password*",
            "passcode",
            "passphrase",
            "pin",
            "pin code",
            "otp",
            "one-time code",
            "verification code",
            "2fa",
            "security code",
            "cvv",
            "cvc",
            "csc",
            "secret",
            "api key",
            "token",
            "ssn",
            "social security",
            "contraseña",
            "senha",
            "mot de passe",
            "passwort",
            "kennwort",
            "wachtwoord",
            "hasło",
            "şifre",
            "parola",
            "пароль",
            "密码",
            "密碼",
            "パスワード",
            "暗証番号",
            "비밀번호",
            "암호",
            "كلمة المرور",
            "पासवर्ड",
        ]
        .iter()
        .map(|k| Keyword::new(k))
        .collect()
    });
    let label = normalize(label);
    kws.iter().any(|k| k.matches(&label))
}

/// Text that would be typed or sent, made safe to log: `[redacted]` for secure fields, card
/// numbers and long digit runs masked to their last four digits, capped at [`PREVIEW_CAP`].
pub fn redact_preview(preview: &str, secure: bool) -> String {
    if secure {
        return REDACTED.to_string();
    }
    cap(&mask_digits(preview), PREVIEW_CAP)
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
