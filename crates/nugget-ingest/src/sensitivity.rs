//! Deterministic sensitivity rules (QA GG4-6).
//!
//! The PII scan only finds patterns (card numbers, SSNs, IBANs, secrets), and model triage only
//! runs when a local model is routable and has got to the item. Health, money, credentials and
//! legal content must be kept away from cloud models even without either, so every item is
//! also checked against these rules when it is indexed:
//! - words and phrases about health, therapy, pay, banking, credentials and legal or identity
//!   matters, in the title, the text and the mail subject;
//! - folders and tags that name such matters (`Health/`, `Finance/`, `#finance`; GG5-2);
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
    ("diagnosis", "health"),
    ("diagnosed", "health"),
    ("prescription", "health"),
    ("medication", "health"),
    ("pharmacy", "health"),
    ("hba1c", "health"),
    ("biopsy", "health"),
    ("doctor", "health"),
    ("dentist", "health"),
    ("gp", "health"),
    ("symptoms", "health"),
    ("surgery", "health"),
    ("cancer", "health"),
    ("diabetes", "health"),
    ("depression", "health"),
    ("anxiety", "health"),
    ("hiv", "health"),
    ("pregnant", "health"),
    ("mri", "health"),
    ("diagnoses", "health"),
    ("anaemia", "health"),
    ("anemia", "health"),
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
    ("mortgage", "financial"),
    ("pension", "financial"),
    ("salaries", "financial"),
    ("loan", "financial"),
    ("debt", "financial"),
    ("overdraft", "financial"),
    ("irs", "financial"),
    ("iban", "financial"),
    ("gehalt", "financial"),
    ("salaire", "financial"),
    ("salario", "financial"),
    // credentials
    ("passcode", "credentials"),
    ("otp", "credentials"),
    ("2fa", "credentials"),
    ("passwort", "credentials"),
    ("contraseña", "credentials"),
    // legal / identity
    ("passport", "identity"),
    ("lawsuit", "legal"),
    ("lawyer", "legal"),
    ("police", "legal"),
    ("arrest", "legal"),
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
    ("income tax", "financial"),
    ("tax refund", "financial"),
    ("bank statement", "financial"),
    ("password is", "credentials"),
    ("temporary password", "credentials"),
    ("green card", "identity"),
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
    ("payroll", "financial"),
    ("benefits", "financial"),
    ("legal", "legal"),
    ("security", "credentials"),
    ("fraud", "financial"),
    ("compliance", "legal"),
];

/// Words that name a sensitive kind of organisation when they appear anywhere in a domain
/// label (`northshorebank`, `albers-counselling`).
const DOMAIN_STEMS: &[(&str, &str)] = &[
    ("therapy", "health"),
    ("therapist", "health"),
    ("counselling", "health"),
    ("counseling", "health"),
    ("clinic", "health"),
    ("hospital", "health"),
    ("pharmacy", "health"),
    ("dental", "health"),
    ("medical", "health"),
    ("healthcare", "health"),
    ("bank", "financial"),
];

/// Words in a sender's domain (whole labels) that name a sensitive kind of organisation.
const DOMAIN_WORDS: &[(&str, &str)] = &[
    ("therapy", "health"),
    ("therapist", "health"),
    ("counselling", "health"),
    ("counseling", "health"),
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

/// Folder names (any path segment of the item key) and tags that file an item as sensitive.
const FOLDERS_AND_TAGS: &[(&str, &str)] = &[
    ("health", "health"),
    ("medical", "health"),
    ("therapy", "health"),
    ("counselling", "health"),
    ("counseling", "health"),
    ("finance", "financial"),
    ("finances", "financial"),
    ("financial", "financial"),
    ("banking", "financial"),
    ("bank", "financial"),
    ("tax", "financial"),
    ("taxes", "financial"),
    ("payslip", "financial"),
    ("payslips", "financial"),
    ("salary", "financial"),
    ("mortgage", "financial"),
    ("insurance", "financial"),
    ("legal", "legal"),
    ("passwords", "credentials"),
    ("hr", "financial"),
];

fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .replace(['\u{2019}', '\u{2018}'], "'")
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .map(|t| t.trim_matches('\'').to_owned())
        .filter(|t| !t.is_empty())
        .collect()
}

/// The kind of sensitive content the rules found (`"health"`, `"financial"`, `"credentials"`,
/// `"legal"`, `"identity"`), if any. `key` is the item key (a path for file sources), `tags`
/// the extracted tags, `extracted` the extractor's metadata (for mail: `from`, `subject`).
pub(crate) fn classify(
    key: &str,
    tags: &[String],
    title: Option<&str>,
    text: &str,
    extracted: &Value,
) -> Option<&'static str> {
    // A title or subject that says "confidential" (not a footer in the body).
    let subject = extracted.get("subject").and_then(Value::as_str);
    if [title, subject]
        .into_iter()
        .flatten()
        .any(|t| tokens(t).iter().any(|w| w == "confidential"))
    {
        return Some("legal");
    }
    // Folders (not the file name) and tags. File-source keys may carry a `<root>:` prefix.
    let folders = key.rsplit_once(['/', '\\']).map_or("", |(dir, _)| dir);
    let filed = folders
        .split(['/', '\\'])
        .map(|seg| seg.rsplit(':').next().unwrap_or(seg).trim().to_lowercase())
        .chain(
            tags.iter()
                .map(|t| t.trim_start_matches('#').trim().to_lowercase()),
        );
    for name in filed {
        if let Some((_, kind)) = FOLDERS_AND_TAGS.iter().find(|(w, _)| *w == name) {
            return Some(kind);
        }
    }
    let mut toks = Vec::new();
    for part in [title, subject, Some(text)].into_iter().flatten() {
        toks.extend(tokens(part));
        toks.push(String::new()); // no phrase spans two fields
    }
    let set: HashSet<&str> = toks.iter().map(String::as_str).collect();
    // Whole words, and simple plurals of them ("medications", "lawyers").
    let plural_of = |w: &str| set.contains(format!("{w}s").as_str());
    if let Some((_, kind)) = WORDS.iter().find(|(w, _)| set.contains(w) || plural_of(w)) {
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
    if let Some(kind) = sender_kind(from) {
        return Some(kind);
    }
    // Mail sent to such organisations (the user's reply to a therapist) too.
    ["to", "cc"]
        .iter()
        .filter_map(|k| extracted.get(*k).and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
        .find_map(|a| address(a).and_then(|(_, d)| domain_kind(&d)))
}

/// `(mailbox, domain)` of `Name <mailbox@domain>` or a bare address, lowercase.
fn address(s: &str) -> Option<(String, String)> {
    let addr = match (s.rfind('<'), s.rfind('>')) {
        (Some(a), Some(b)) if a < b => &s[a + 1..b],
        _ => s,
    };
    let (m, d) = addr.trim().rsplit_once('@')?;
    Some((m.to_lowercase(), d.to_lowercase()))
}

fn domain_kind(domain: &str) -> Option<&'static str> {
    domain.split(['.', '-']).find_map(|part| {
        DOMAIN_WORDS
            .iter()
            .find(|(w, _)| *w == part)
            .or_else(|| DOMAIN_STEMS.iter().find(|(w, _)| part.contains(w)))
            .map(|(_, k)| *k)
    })
}

/// Rules on a sender: role mailboxes, then the domain.
fn sender_kind(from: &str) -> Option<&'static str> {
    let (mailbox, domain) = address(from)?;
    let base = mailbox.split(['+', '.', '-', '_']).next().unwrap_or("");
    if let Some((_, kind)) = ROLE_MAILBOXES
        .iter()
        .find(|(m, _)| *m == mailbox || *m == base)
    {
        return Some(kind);
    }
    domain_kind(&domain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mail(from: &str, subject: &str, body: &str) -> Option<&'static str> {
        classify(
            "m.eml",
            &[],
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
            classify("n.md", &[], None, "Termin beim Arzt am Montag", &json!({})),
            Some("health")
        );
    }

    #[test]
    fn folders_and_tags_file_items_as_sensitive() {
        let none = json!({});
        let c = |key: &str, tags: &[&str]| {
            let tags: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
            classify(key, &tags, None, "notes", &none)
        };
        assert_eq!(c("0:Health/2026-05-12.md", &[]), Some("health"));
        assert_eq!(c("Notes/Finance/x.md", &[]), Some("financial"));
        assert_eq!(c("a.md", &["#finance"]), Some("financial"));
        // The file name alone is not a folder.
        assert_eq!(c("Notes/health.md", &[]), None);
        assert_eq!(c("Notes/Projects/Harbor.md", &["harbor"]), None);
    }

    #[test]
    fn review_cases_are_caught() {
        assert_eq!(
            mail(
                "a@gmail.com",
                "Hi",
                "Dr. Patel wants to discuss your MRI next week"
            ),
            Some("health")
        );
        assert_eq!(
            mail("a@gmail.com", "Hi", "new medications from Monday"),
            Some("health")
        );
        assert_eq!(
            mail("a@gmail.com", "Hi", "renew my driver\u{2019}s license"),
            Some("identity")
        );
        assert_eq!(mail("a@x.example", "Card", "hello"), None);
        assert_eq!(
            mail(
                "Northshore <alerts@northshorebank.example>",
                "Card",
                "hello"
            ),
            Some("financial")
        );
        // The user's own reply to a therapist (no quote): the recipient's domain counts.
        let reply = json!({
            "from": "Mara <mara@mailbox.example>",
            "to": ["Jonas Albers <jonas@albers-counselling.example>"],
            "subject": "Re: Thursday",
        });
        assert_eq!(
            classify("r.eml", &[], Some("Re: Thursday"), "See you then", &reply),
            Some("health")
        );
        // "Confidential" counts in a subject, not in a body footer.
        assert_eq!(
            mail("a@x.example", "Confidential: offer", "hi"),
            Some("legal")
        );
        assert_eq!(
            mail("a@x.example", "Lunch", "This email is confidential."),
            None
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
            (
                "x <hrm@x.example>",
                "therapyx",
                "thanks for being patient, bonus points",
            ),
            (
                "Acme <noreply@acme.example>",
                "Reset your password",
                "Click the link to reset your password.",
            ),
            (
                "Ops <people@acme.example>",
                "Team lunch",
                "Pizza on Friday.",
            ),
        ] {
            assert_eq!(mail(from, subject, body), None, "{subject}");
        }
    }
}
