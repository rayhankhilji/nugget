use super::*;

#[test]
fn names_match_exactly_or_with_suffixes_and_cuts() {
    assert!(name_matches("Pay  $3", Some("Pay $3")));
    assert!(name_matches("Firewall", Some("Firewall (on)")));
    assert!(!name_matches("Fire", Some("Firewall (on)")));
    assert!(!name_matches("Pay", Some("Pay $3")));
    assert!(name_matches(
        "A very long message body that goes on",
        Some("A very long message…")
    ));
    assert!(!name_matches("Short", Some("Sh…")));
    assert!(!name_matches("x", None));
}
