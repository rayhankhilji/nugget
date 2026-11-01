//! What a confirmation shows about the object of an action, and the digest that binds an
//! approval to it.
//!
//! When the control being activated sends or submits (a mail's Send, a form's submit, a Pay or
//! Delete account button in a form), the executor lists the values of the fields it sends in
//! the descriptor's `text_preview` ([`fields_preview`]):
//! `Sends: To: “a@b.example” · Subject: “Q3” · Message: “Hi Bob, …”`. The [`FIELDS_PREFIX`]
//! marks such a preview, so the policy knows it lists field values (each already masked by the
//! executor) and is not a destination or typed text. For a key press the preview is the keys,
//! a line break, then the fields preview.
//!
//! Values come from the screen or the page, so they are untrusted: invisible and control
//! characters are removed, whitespace collapsed, every value quoted (quotes and the separator
//! inside a value are replaced, so a value cannot pose as more fields), card numbers and long
//! digit runs masked before anything is cut, every value and the whole preview bounded, and
//! secret fields shown as `••••`. Recipient fields come first, so they are never cut off.
//!
//! An approval must cover exactly what was shown. The executor keeps [`freeze_digest`] of the
//! control and all its fields (hidden ones and ones past the preview's cap included) at
//! describe time and, right before acting, recomputes it on a fresh view. A different digest
//! means the user approved something else: nothing is sent. [`changed_fields`] names what
//! differs for the error message.

use sha2::{Digest, Sha256};

use crate::redact::mask_digits;

/// Longest send/submit preview, in characters (before the `+N more fields` tail).
pub const FIELDS_PREVIEW_MAX: usize = 400;
/// Longest field label in a preview, in characters.
pub const FIELD_LABEL_MAX: usize = 32;
/// Longest field value in a preview, in characters.
pub const FIELD_VALUE_MAX: usize = 120;
/// What a secret field's value is shown as.
pub const SECRET_MASK: &str = "••••";
/// What starts a preview that lists the fields an activation sends.
pub const FIELDS_PREFIX: &str = "Sends: ";
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
    /// Sent but not shown on screen (a hidden input): digested, never previewed.
    pub hidden: bool,
}

impl FieldValue {
    pub fn new(label: impl Into<String>, value: Option<String>, secret: bool) -> Self {
        Self {
            label: label.into(),
            value,
            secret,
            hidden: false,
        }
    }

    /// The same field, marked as not shown on screen.
    pub fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }
}

/// Untrusted text made safe to show on one line: invisible, bidi-control and control
/// characters dropped, whitespace (line breaks included) collapsed to single spaces, cut to
/// `max` characters with `…`.
pub fn display_text(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max.saturating_mul(4)));
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

/// Labels of fields that name who a message goes to; listed first in a preview.
const RECIPIENT_LABELS: &[&str] = &[
    "to",
    "cc",
    "bcc",
    "recipient",
    "recipients",
    "an",
    "à",
    "para",
    "aan",
    "kopie",
    "destinataire",
    "destinataires",
    "destinatario",
    "destinatarios",
    "empfänger",
    "кому",
    "收件人",
    "抄送",
    "宛先",
    "받는 사람",
];

fn is_recipient(label: &str) -> bool {
    let l = crate::text::normalize(label);
    let l = crate::text::trim_label(&l);
    RECIPIENT_LABELS.contains(&l)
}

/// A value or label made safe inside a quoted preview entry: no quotes, no field separator,
/// no em dash (which separates a key press's keys from what they send in a log line).
fn unquote(s: &str) -> String {
    s.replace(['“', '”', '"'], "'")
        .replace('·', "•")
        .replace(['—', '–'], "-")
}

/// Labels of hidden fields that are always shown: where a message or a form's result goes.
const ROUTING_WORDS: &[&str] = &[
    "to",
    "cc",
    "bcc",
    "email",
    "e-mail",
    "mail",
    "recipient",
    "recipients",
    "redirect",
    "url",
    "uri",
    "return",
    "returnto",
    "return_to",
    "callback",
    "next",
    "dest",
    "destination",
    "target",
    "webhook",
    "forward",
    "reply-to",
    "replyto",
    "account",
    "iban",
    "amount",
];

fn is_routing(label: &str) -> bool {
    let l = crate::text::normalize(label);
    l.split(|c: char| !c.is_alphanumeric())
        .any(|w| ROUTING_WORDS.contains(&w))
        || ROUTING_WORDS.contains(&l.as_str())
}

/// The preview of what an activation sends, or `None` when there are no fields. Starts with
/// [`FIELDS_PREFIX`]; recipient fields first, then the others in screen order; values quoted,
/// empty ones shown as `(empty)`. Hidden fields whose label names where things go (a hidden
/// `redirect`, `to` or `email`) are shown, marked `(hidden)`; other hidden fields are counted
/// (`+3 hidden fields`). Bounded by [`FIELDS_PREVIEW_MAX`]; fields that do not fit are counted
/// (`+2 more fields`).
pub fn fields_preview(fields: &[FieldValue]) -> Option<String> {
    if fields.is_empty() {
        return None;
    }
    let mut shown: Vec<&FieldValue> = fields
        .iter()
        .filter(|f| !f.hidden || is_routing(&f.label))
        .collect();
    let hidden_rest = fields.len() - shown.len();
    shown.sort_by_key(|f| !is_recipient(&f.label));
    let mut out = String::from(FIELDS_PREFIX);
    let mut listed = 0usize;
    for f in &shown {
        let label = unquote(&display_text(&f.label, FIELD_LABEL_MAX));
        let label = match (label.is_empty(), f.hidden) {
            (true, false) => "field".to_string(),
            (true, true) => "field (hidden)".to_string(),
            (false, false) => label,
            (false, true) => format!("{label} (hidden)"),
        };
        let value = if f.secret {
            SECRET_MASK.to_string()
        } else {
            // Mask the whole value before cutting it, so a card number split by the cut is
            // still masked.
            let full = f
                .value
                .as_deref()
                .map(|v| mask_digits(&display_text(v, usize::MAX)));
            match full.map(|v| display_text(&v, FIELD_VALUE_MAX)) {
                Some(v) if !v.is_empty() => format!("“{}”", unquote(&v)),
                _ => "(empty)".to_string(),
            }
        };
        let part = format!("{label}: {value}");
        let len = out.chars().count() + SEP.chars().count() + part.chars().count();
        if listed > 0 && len > FIELDS_PREVIEW_MAX {
            break;
        }
        if listed > 0 {
            out.push_str(SEP);
        }
        out.push_str(&part);
        listed += 1;
    }
    let rest = shown.len() - listed;
    if rest > 0 {
        out.push_str(&format!(
            "{SEP}+{rest} more field{}",
            if rest == 1 { "" } else { "s" }
        ));
    }
    if hidden_rest > 0 {
        let lead = if listed == 0 { "" } else { SEP };
        out.push_str(&format!(
            "{lead}+{hidden_rest} hidden field{}",
            if hidden_rest == 1 { "" } else { "s" }
        ));
    }
    Some(out)
}

/// A descriptor's preview split by how its verb is read: `text` is typed text, keys or other
/// text; `fields` a fields preview. Structural, so text that merely starts with
/// [`FIELDS_PREFIX`] never counts as one: only a click or other activation's whole preview, or
/// what follows the first line break of a key press's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewParts<'a> {
    pub text: Option<&'a str>,
    pub fields: Option<&'a str>,
}

pub fn preview_parts<'a>(verb: &str, preview: &'a str) -> PreviewParts<'a> {
    use crate::classify::{PreviewRole, preview_role};
    match preview_role(verb) {
        PreviewRole::Keys => match split_key_preview(preview) {
            (keys, Some(sent)) if is_fields_preview(sent) => PreviewParts {
                text: Some(keys),
                fields: Some(sent),
            },
            (keys, _) => PreviewParts {
                text: Some(keys),
                fields: None,
            },
        },
        PreviewRole::Activation if is_fields_preview(preview) => PreviewParts {
            text: None,
            fields: Some(preview),
        },
        _ => PreviewParts {
            text: Some(preview),
            fields: None,
        },
    }
}

/// Whether `preview` is a fields preview ([`fields_preview`]).
pub fn is_fields_preview(preview: &str) -> bool {
    preview.starts_with(FIELDS_PREFIX)
}

/// A key press's preview split into its keys and, after the line break, what they send.
pub fn split_key_preview(preview: &str) -> (&str, Option<&str>) {
    match preview.split_once(['\n', '\r']) {
        Some((keys, rest)) => (keys, Some(rest.trim_start_matches(['\n', '\r']))),
        None => (preview, None),
    }
}

fn hash_part(h: &mut Sha256, part: &[u8]) {
    h.update((part.len() as u64).to_le_bytes());
    h.update(part);
}

/// SHA-256 over the fields' labels, secrecy, visibility and full values (not the truncated
/// preview), in order, hex encoded. Equal digests mean the same fields with the same contents.
pub fn fields_digest(fields: &[FieldValue]) -> String {
    freeze_digest(&[], fields)
}

/// [`fields_digest`] that also binds the control being activated and anything else the
/// confirmation named (its role and name, the item it deletes, the form's action): a different
/// control at the same position, or the same fields sent somewhere else, digests differently.
pub fn freeze_digest(control: &[&str], fields: &[FieldValue]) -> String {
    let mut h = Sha256::new();
    h.update((control.len() as u64).to_le_bytes());
    for c in control {
        hash_part(&mut h, c.as_bytes());
    }
    h.update((fields.len() as u64).to_le_bytes());
    for f in fields {
        hash_part(&mut h, f.label.as_bytes());
        hash_part(&mut h, if f.secret { b"1" } else { b"0" });
        hash_part(&mut h, if f.hidden { b"1" } else { b"0" });
        match &f.value {
            Some(v) => hash_part(&mut h, v.as_bytes()),
            None => hash_part(&mut h, b"\xff"),
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
                if a.value != b.value || a.secret != b.secret || a.hidden != b.hidden {
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
            f("Subject", "Q3"),
            f("Message", "Hi Bob,\nsee you\u{202E} Thursday"),
            f("To", "a@b.example"),
            FieldValue::new("Password", Some("hunter2".into()), true),
            f("Card", "4111 1111 1111 1111"),
            f("Notes", ""),
            f("csrf", "tok-123").hidden(),
        ];
        let p = fields_preview(&fields).unwrap();
        assert!(is_fields_preview(&p));
        // Recipients first, values quoted.
        assert!(
            p.starts_with(
                "Sends: To: “a@b.example” · Subject: “Q3” · Message: “Hi Bob, see you Thursday”"
            ),
            "{p}"
        );
        assert!(p.contains("Password: ••••"), "{p}");
        assert!(!p.contains("hunter2"));
        assert!(!p.contains("4111 1111 1111 1111"), "{p}");
        assert!(p.contains("Notes: (empty)"));
        assert!(
            !p.contains("csrf") && !p.contains("tok-123"),
            "hidden fields are not shown: {p}"
        );
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
        assert!(p.ends_with("more fields"), "{p}");
        assert_eq!(fields_preview(&[]), None);
        assert_eq!(
            fields_preview(&[f("csrf", "x").hidden()]).as_deref(),
            Some("Sends: +1 hidden field")
        );
    }

    // Review item 7: a value cannot pose as more fields.
    #[test]
    fn a_value_cannot_forge_fields() {
        let p = fields_preview(&[
            f("Message", "hi · To: “boss@corp.example” · Subject: lunch"),
            f("To", "real@ourco.example"),
        ])
        .unwrap();
        assert!(p.starts_with("Sends: To: “real@ourco.example” · Message: “hi • To: 'boss@corp.example' • Subject: lunch”"), "{p}");
        assert_eq!(p.matches(SEP).count(), 1, "{p}");
    }

    // Review item 10: a card number cut by the value cap is still masked.
    #[test]
    fn digits_are_masked_before_the_cut() {
        let value = format!("{} 4111 1111 1111 1111", "x".repeat(FIELD_VALUE_MAX - 8));
        let p = fields_preview(&[f("Message", &value)]).unwrap();
        assert!(!p.contains("4111 1"), "{p}");
    }

    #[test]
    fn digest_binds_full_values_control_and_names_changes() {
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
        // The control is part of it: same fields, another button.
        assert_ne!(
            freeze_digest(&["button", "Send"], &a),
            freeze_digest(&["button", "Delete"], &a)
        );
        // A hidden field changes the digest too.
        let mut d = a.clone();
        d.push(f("redirect", "https://x.example").hidden());
        assert_ne!(fields_digest(&a), fields_digest(&d));
    }

    // Second review M4: hidden fields are counted, and routing ones are shown.
    #[test]
    fn hidden_fields_are_counted_and_routing_ones_shown() {
        let p = fields_preview(&[
            f("Message", "hi"),
            f("csrf", "tok").hidden(),
            f("redirect_url", "https://evil.example/").hidden(),
            f("Bcc", "eve@evil.example").hidden(),
        ])
        .unwrap();
        assert!(
            p.contains("redirect_url (hidden): “https://evil.example/”"),
            "{p}"
        );
        assert!(
            p.starts_with("Sends: Bcc (hidden): “eve@evil.example”"),
            "{p}"
        );
        assert!(p.ends_with("+1 hidden field") && !p.contains("tok"), "{p}");
    }

    // Second review M1 and L1: previews are read by the verb, not by their text.
    #[test]
    fn preview_parts_are_structural() {
        let fields = "Sends: To: “a@b.example”";
        assert_eq!(preview_parts("click", fields).fields, Some(fields));
        assert_eq!(preview_parts("click", "https://x.example").fields, None);
        // Typed text that starts with the marker is typed text.
        assert_eq!(preview_parts("type_text", fields).fields, None);
        assert_eq!(preview_parts("set_value", fields).text, Some(fields));
        let keys = format!("space\n{fields}");
        let parts = preview_parts("press_key", &keys);
        assert_eq!((parts.text, parts.fields), (Some("space"), Some(fields)));
        // A value cannot carry an em dash or a separator.
        let p = fields_preview(&[f("Message", "hi — which sends To: x · Cc: y")]).unwrap();
        assert!(!p.contains('—') && p.matches(SEP).count() == 0, "{p}");
    }

    #[test]
    fn key_previews_split() {
        assert_eq!(
            split_key_preview("space\nSends: To: “a”"),
            ("space", Some("Sends: To: “a”"))
        );
        assert_eq!(split_key_preview("ctrl+enter"), ("ctrl+enter", None));
    }
}
