//! For each draft: would II3's history keep it (`looks_sensitive` false), and does the daemon's
//! own PII scan (used for items) see a secret in it?
fn main() {
    let drafts = [
        ("card-plain", "Sure, use my card 4111111111111111 for the deposit."),
        ("card-spaces", "Sure, use my card 4111 1111 1111 1111 for the deposit."),
        ("card-dashes", "Card: 4111-1111-1111-1111, exp 09/28."),
        ("amex", "It's 3782 822463 10005 if you need it."),
        ("iban-de-spaced", "Bitte überweise an DE89 3704 0044 0532 0130 00, danke!"),
        ("iban-gb-compact", "Pay GB33BUKB20201555555555 by Friday."),
        ("ssn-dashed", "My SSN is 123-45-6789 for the form."),
        ("api-key", "Here's the key: sk-live-4f9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c"),
        ("aws-key", "Use AKIAIOSFODNN7EXAMPLE with the secret I sent."),
        ("password", "The Wi-Fi password is Kestrel!Moon42"),
        ("phone", "Call me on +44 7700 900123 tonight."),
        ("benign", "Thanks Priya, I'll send the Q3 report by Thursday."),
        ("benign-number", "Order #123456789 has shipped."),
    ];
    println!("label\thistory_keeps\tpii_scan_flags\ttext");
    for (label, t) in drafts {
        let keeps = nugget_policy::redact::mask_digits(t) == t;
        let pii = nugget_ingest::scan_pii(t);
        println!("{label}\t{keeps}\t{}\t{t}", if pii.is_empty() { "none".to_string() } else { format!("card={} ssn={} iban={} secret={}", pii.card, pii.ssn, pii.iban, pii.secret) });
    }
}
