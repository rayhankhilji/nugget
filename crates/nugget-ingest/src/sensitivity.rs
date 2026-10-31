//! Deterministic sensitivity rules (QA GG4-6).
//!
//! The PII scan only finds patterns (card numbers, SSNs, IBANs, secrets), and model triage only
//! runs when a local model is routable and has got to the item. Health, money, credentials and
//! legal content must be kept away from cloud models even without either, so every item is
//! also checked against these rules when it is indexed:
//! - words and phrases about health, therapy, pay, banking, credentials and legal or identity
//!   matters, in the title, the text and the mail subject;
//! - senders whose mailbox is a role that handles such content (`hr@`, `payroll@`,
//!   `security@`, ...) or whose domain names it (`…-therapy.example`, `…clinic…`, `…bank…`).
//!
//! A hit only marks the item sensitive: it stays searchable and usable locally and is never
//! sent to cloud models. The rules lean towards recall; a false positive costs only cloud use.

use std::collections::HashSet;

use serde_json::Value;

/// Single words, matched against whole lowercase tokens.
const WORDS: &[(&str, &str)] = &[
    // health
    ("therapy", "health"),
    ("therapist", "health"),
    ("psychotherapy", "health"),
    ("psychiatrist", "health"),
    ("psychologist", "health"),
    ("counselling", "health"),
    ("counseling", "health"),
    ("clinic", "health"),
    ("hospital", "health"),
    ("medical", "health"),
    ("patient", "health"),
    ("diagnosis", "health"),
    ("diagnosed", "health"),
    ("prescription", "health"),
    ("medication", "health"),
    ("pharmacy", "health"),
    ("hba1c", "health"),
    ("biopsy", "health"),
    ("oncology", "health"),
    ("pregnancy", "health"),
    ("vaccination", "health"),
    ("arzt", "health"),
    ("ärztin", "health"),
    ("diagnose", "health"),
    ("médecin", "health"),
    ("médico", "health"),
    ("thérapie", "health"),
    ("terapia", "health"),
    // money
    ("salary", "financial"),
    ("compensation", "financial"),
    ("payslip", "financial"),
    ("payroll", "financial"),
    ("paycheck", "financial"),
    ("bonus", "financial"),
    ("mortgage", "financial"),
    ("pension", "financial"),
    ("iban", "financial"),
    ("gehalt", "financial"),
    ("salaire", "financial"),
    ("salario", "financial"),
    // credentials
    ("password", "credentials"),
    ("passcode", "credentials"),
    ("otp", "credentials"),
    ("2fa", "credentials"),
    ("passwort", "credentials"),
    ("contraseña", "credentials"),
    // legal / identity
    ("confidential", "legal"),
    ("passport", "identity"),
    ("lawsuit", "legal"),
    ("attorney", "legal"),
    ("solicitor", "legal"),
    ("subpoena", "legal"),
    ("custody", "legal"),
    ("divorce", "legal"),
    ("immigration", "identity"),
    ("vertraulich", "legal"),
];

/// Phrases (lowercase words separated by single spaces), matched on token boundaries.
const PHRASES: &[(&str, &str)] = &[
    ("test results", "health"),
    ("lab results", "health"),
    ("blood test", "health"),
    ("mental health", "health"),
    ("patient portal", "health"),
    ("bank account", "financial"),
    ("account number", "financial"),
    ("routing number", "financial"),
    ("sort code", "financial"),
    ("card ending", "financial"),
    ("credit card", "financial"),
    ("debit card", "financial"),
    ("fraud alert", "financial"),
    ("tax return", "financial"),
    ("credit score", "financial"),
    ("pay slip", "financial"),
    ("verification code", "credentials"),
    ("security code", "credentials"),
    ("one time code", "credentials"),
    ("login code", "credentials"),
    ("recovery code", "credentials"),
    ("pin code", "credentials"),
    ("api key", "credentials"),
    ("mot de passe", "credentials"),
    ("social security", "identity"),
    ("national insurance", "identity"),
    ("driver's license", "identity"),
    ("driving licence", "identity"),
    ("date of birth", "identity"),
];

/// Mailbox names (the part before `@`) of senders that handle sensitive matters.
const ROLE_MAILBOXES: &[(&str, &str)] = &[
    ("hr", "financial"),
    ("people", "financial"),
    ("payroll", "financial"),
    ("benefits", "financial"),
    ("legal", "legal"),
    ("security", "credentials"),
    ("fraud", "financial"),
    ("compliance", "legal"),
];

/// Words in a sender's domain that name a sensitive kind of organisation.
const DOMAIN_WORDS: &[(&str, &str)] = &[
    ("therapy", "health"),
    ("therapist", "health"),
    ("clinic", "health"),
    ("health", "health"),
    ("healthcare", "health"),
    ("medical", "health"),
    ("hospital", "health"),
    ("pharmacy", "health"),
    ("dental", "health"),
    ("bank", "financial"),
    ("banking", "financial"),
    ("insurance", "financial"),
    ("law", "legal"),
    ("legal", "legal"),
];

fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .map(|t| t.trim_matches('\'').to_owned())
        .filter(|t| !t.is_empty())
        .collect()
}

/// The kind of sensitive content the rules found (`"health"`, `"financial"`, `"credentials"`,
/// `"legal"`, `"identity"`), if any. `extracted` is the extractor's metadata (for mail: `from`,
/// `subject`).
pub(crate) fn classify(title: Option<&str>, text: &str, extracted: &Value) -> Option<&'static str> {
    let subject = extracted.get("subject").and_then(Value::as_str);
    let mut toks = Vec::new();
    for part in [title, subject, Some(text)].into_iter().flatten() {
        toks.extend(tokens(part));
        toks.push(String::new()); // no phrase spans two fields
    }
    let set: HashSet<&str> = toks.iter().map(String::as_str).collect();
    if let Some((_, kind)) = WORDS.iter().find(|(w, _)| set.contains(w)) {
        return Some(kind);
    }
    let joined = format!(" {} ", toks.join(" "));
    if let Some((_, kind)) = PHRASES
        .iter()
        .find(|(p, _)| joined.contains(&format!(" {} ", tokens(p).join(" "))))
    {
        return Some(kind);
    }
    let from = extracted.get("from").and_then(Value::as_str).unwrap_or("");
    sender_kind(from)
}

/// Rules on a sender (`Name <mailbox@domain>` or a bare address).
fn sender_kind(from: &str) -> Option<&'static str> {
    let addr = match (from.rfind('<'), from.rfind('>')) {
        (Some(a), Some(b)) if a < b => &from[a + 1..b],
        _ => from,
    };
    let (mailbox, domain) = addr.trim().rsplit_once('@')?;
    let mailbox = mailbox.to_lowercase();
    let base = mailbox.split(['+', '.', '-', '_']).next().unwrap_or("");
    if let Some((_, kind)) = ROLE_MAILBOXES
        .iter()
        .find(|(m, _)| *m == mailbox || *m == base)
    {
        return Some(kind);
    }
    let domain = domain.to_lowercase();
    domain.split(['.', '-']).find_map(|part| {
        DOMAIN_WORDS
            .iter()
            .find(|(w, _)| *w == part)
            .map(|(_, k)| *k)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mail(from: &str, subject: &str, body: &str) -> Option<&'static str> {
        classify(
            Some(subject),
            body,
            &json!({"from": from, "subject": subject}),
        )
    }

    #[test]
    fn health_money_credentials_and_role_senders_are_sensitive() {
        assert_eq!(
            mail(
                "Rebecca Moss <rebecca@mossplace-therapy.example>",
                "Thursday session",
                "can we move Thursday's session to 5pm?"
            ),
            Some("health")
        );
        assert_eq!(
            mail(
                "Larkspur People Team <hr@larkspur.example>",
                "Hello",
                "see attached"
            ),
            Some("financial")
        );
        assert_eq!(
            mail("a <security@x.example>", "Hi", "hello"),
            Some("credentials")
        );
        assert_eq!(
            mail("a@b.example", "Your verification code", "It is 482-913"),
            Some("credentials")
        );
        assert_eq!(
            mail("a@b.example", "Re", "your salary for 2027"),
            Some("financial")
        );
        assert_eq!(
            mail(
                "a@b.example",
                "Re",
                "we noticed a charge on card ending 4417"
            ),
            Some("financial")
        );
        assert_eq!(
            classify(None, "Termin beim Arzt am Montag", &json!({})),
            Some("health")
        );
    }

    #[test]
    fn ordinary_content_is_not() {
        for (from, subject, body) in [
            (
                "Dana <dana@northwind.example>",
                "Checkout copy",
                "Could you send the revised checkout copy by Friday?",
            ),
            (
                "Mom <mom@example.com>",
                "Thanksgiving",
                "Are you coming home for Thanksgiving?",
            ),
            (
                "Shop <news@shop.example>",
                "Weekly deals",
                "Big savings on shoes this week. Unsubscribe here.",
            ),
            // Substrings never match: "therapy" is not in "therapyx", "hr" is not "hrm".
            ("x <hrm@banker.example>", "therapyx", "the bonuses page"),
        ] {
            assert_eq!(mail(from, subject, body), None, "{subject}");
        }
    }
}
