//! What a confirmation shows about the object of an action, and the digest that binds an
//! approval to it.
//!
//! When the control being activated sends or submits (a mail's Send, a form's submit), the
//! executor lists the values of the fields it sends in the descriptor's `text_preview`
//! ([`fields_preview`]): `To: a@b.example · Subject: Q3 · Message: Hi Bob, …`. Values come from
//! the screen or the page, so they are untrusted: invisible and control characters are removed,
//! whitespace collapsed, every value and the whole preview bounded, card numbers and long digit
//! runs masked, and secret fields shown as `••••`.
//!
//! An approval must cover exactly what was shown. The executor keeps [`fields_digest`] of the
//! fields at describe time and, right before acting, recomputes it on a fresh view of the form.
//! A different digest (a value changed, a field appeared or went away) means the user approved
//! something else: nothing is sent. [`changed_fields`] names what differs for the error message.

use sha2::{Digest, Sha256};

use crate::redact::mask_digits;

/// Longest send/submit preview, in characters.
pub const FIELDS_PREVIEW_MAX: usize = 400;
/// Longest field label in a preview, in characters.
pub const FIELD_LABEL_MAX: usize = 32;
/// Longest field value in a preview, in characters.
pub const FIELD_VALUE_MAX: usize = 120;
/// What a secret field's value is shown as.
pub const SECRET_MASK: &str = "••••";
/// Separator between fields in a preview.
const SEP: &str = " · ";

/// One field of a form or dialog that an activation sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldValue {
    /// The field's label (accessible name, else placeholder), as the screen shows it.
    pub label: String,
    /// Its current value (`None` when it cannot be read, as for secure fields).
    pub value: Option<String>,
    /// The field holds a secret (password, one-time code, ...): its value is never shown.
    pub secret: bool,
}

impl FieldValue {
    pub fn new(label: impl Into<String>, value: Option<String>, secret: bool) -> Self {
        Self {
            label: label.into(),
            value,
            secret,
        }
    }
}

/// Untrusted text made safe to show on one line: invisible, bidi-control and control
/// characters dropped, whitespace (line breaks included) collapsed to single spaces, cut to
/// `max` characters with `…`.
pub fn display_text(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max * 4));
    let mut chars = 0usize;
    let mut pending_space = false;
    for c in s.chars() {
        if crate::text::is_invisible(c) {
            continue;
        }
        if c.is_whitespace() || c.is_control() {
            pending_space = !out.is_empty();
            continue;
        }
        if chars >= max {
            out.push('…');
            return out;
        }
        if pending_space {
            out.push(' ');
            chars += 1;
            pending_space = false;
            if chars >= max {
                out.push('…');
                return out;
            }
        }
        out.push(c);
        chars += 1;
    }
    out
}

/// The preview of what an activation sends, or `None` when there are no fields. Empty fields
/// are listed as `(empty)` so a blank recipient is visible too. Bounded by
/// [`FIELDS_PREVIEW_MAX`]; fields that do not fit are counted (`+2 more fields`).
pub fn fields_preview(fields: &[FieldValue]) -> Option<String> {
    if fields.is_empty() {
        return None;
    }
    let mut out = String::new();
    let mut shown = 0usize;
    for f in fields {
        let label = display_text(&f.label, FIELD_LABEL_MAX);
        let label = if label.is_empty() {
            "field".to_string()
        } else {
            label
        };
        let value = if f.secret {
            SECRET_MASK.to_string()
        } else {
            match f.value.as_deref().map(|v| display_text(v, FIELD_VALUE_MAX)) {
                Some(v) if !v.is_empty() => mask_digits(&v),
                _ => "(empty)".to_string(),
            }
        };
        let part = format!("{label}: {value}");
        let len = out.chars().count() + SEP.chars().count() + part.chars().count();
        if shown > 0 && len > FIELDS_PREVIEW_MAX {
            break;
        }
        if shown > 0 {
            out.push_str(SEP);
        }
        out.push_str(&part);
        shown += 1;
    }
    let rest = fields.len() - shown;
    if rest > 0 {
        out.push_str(&format!(
            "{SEP}+{rest} more field{}",
            if rest == 1 { "" } else { "s" }
        ));
    }
    Some(display_text(&out, FIELDS_PREVIEW_MAX + 40))
}

/// SHA-256 over the fields' labels, secrecy and full values (not the truncated preview), in
/// order, hex encoded. Equal digests mean the same fields with the same contents.
pub fn fields_digest(fields: &[FieldValue]) -> String {
    let mut h = Sha256::new();
    h.update((fields.len() as u64).to_le_bytes());
    for f in fields {
        for part in [
            f.label.as_bytes(),
            if f.secret { b"1" } else { b"0" },
            f.value.as_deref().unwrap_or("\u{0}none").as_bytes(),
        ] {
            h.update((part.len() as u64).to_le_bytes());
            h.update(part);
        }
    }
    hex::encode(h.finalize())
}

/// Labels of the fields that differ between `before` and `after` (values changed, fields
/// added or removed), sanitized for an error message, at most five.
pub fn changed_fields(before: &[FieldValue], after: &[FieldValue]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |l: &str, what: &str| {
        let l = display_text(l, FIELD_LABEL_MAX);
        let s = format!("{} {what}", if l.is_empty() { "a field" } else { &l });
        if !out.contains(&s) {
            out.push(s);
        }
    };
    for (i, b) in before.iter().enumerate() {
        match after.get(i) {
            Some(a) if a.label == b.label => {
                if a.value != b.value || a.secret != b.secret {
                    push(&b.label, "changed");
                }
            }
            Some(a) => {
                push(&b.label, "replaced");
                push(&a.label, "appeared");
            }
            None => push(&b.label, "disappeared"),
        }
    }
    for a in after.iter().skip(before.len()) {
        push(&a.label, "appeared");
    }
    out.truncate(5);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(label: &str, value: &str) -> FieldValue {
        FieldValue::new(label, Some(value.to_string()), false)
    }

    #[test]
    fn preview_lists_fields_masks_secrets_and_is_bounded() {
        let fields = vec![
            f("To", "a@b.example"),
            f("Subject", "Q3"),
            f("Message", "Hi Bob,\nsee you\u{202E} Thursday"),
            FieldValue::new("Password", Some("hunter2".into()), true),
            f("Card", "4111 1111 1111 1111"),
            f("Notes", ""),
        ];
        let p = fields_preview(&fields).unwrap();
        assert!(p.starts_with("To: a@b.example · Subject: Q3 · Message: Hi Bob, see you Thursday"));
        assert!(p.contains("Password: ••••"), "{p}");
        assert!(!p.contains("hunter2"));
        assert!(!p.contains("4111 1111 1111 1111"), "{p}");
        assert!(p.contains("Notes: (empty)"));
        assert!(!p.contains('\n') && !p.contains('\u{202E}'));

        let long: Vec<FieldValue> = (0..20)
            .map(|i| f(&format!("F{i}"), &"x".repeat(200)))
            .collect();
        let p = fields_preview(&long).unwrap();
        assert!(
            p.chars().count() <= FIELDS_PREVIEW_MAX + 40,
            "{}",
            p.chars().count()
        );
        assert!(p.contains("more fields"), "{p}");
        assert_eq!(fields_preview(&[]), None);
    }

    #[test]
    fn digest_binds_full_values_and_names_changes() {
        let a = vec![f("To", "a@b.example"), f("Message", &"y".repeat(500))];
        let mut b = a.clone();
        assert_eq!(fields_digest(&a), fields_digest(&b));
        // A change past the preview's cut still changes the digest.
        b[1].value = Some(format!("{}z", "y".repeat(499)));
        assert_ne!(fields_digest(&a), fields_digest(&b));
        assert_eq!(changed_fields(&a, &b), vec!["Message changed".to_string()]);
        let c = vec![f("To", "a@b.example")];
        assert_ne!(fields_digest(&a), fields_digest(&c));
        assert_eq!(
            changed_fields(&a, &c),
            vec!["Message disappeared".to_string()]
        );
        assert_eq!(changed_fields(&c, &a), vec!["Message appeared".to_string()]);
    }
}
