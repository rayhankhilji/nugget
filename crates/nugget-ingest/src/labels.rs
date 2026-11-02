//! Privacy labels from the deterministic rules alone, for hosts that index items without a local
//! triage model (Nugget Cloud). Same fields and meaning as the pipeline writes with triage off and
//! `assume_unflagged_safe` on: a PII finding, a connector flag or a sensitivity rule makes the item
//! *confirmed* sensitive (`sensitive_rule`, `triage.state = "skipped"`); anything else is not
//! sensitive (`triage.state = "unknown"`, reason `no_model`).

use serde_json::{Map, Value, json};

use crate::{pii, sensitivity, triage};

/// `sensitive`, `sensitive_rule`, `pii`, `junk` and `triage` metadata for one item. `extracted`
/// holds mail headers (`from`, `to`, `cc`, `subject`) where there are any.
pub fn rule_labels(
    key: &str,
    tags: &[String],
    title: Option<&str>,
    text: &str,
    extracted: &Value,
    connector_sensitive: bool,
) -> Map<String, Value> {
    let found = pii::scan_all([title.unwrap_or(""), text]);
    let rule = if !found.is_empty() {
        Some("pii")
    } else if connector_sensitive {
        Some("connector")
    } else {
        sensitivity::classify(key, tags, title, text, extracted)
    };
    let mut meta = Map::new();
    meta.insert("sensitive".into(), Value::Bool(rule.is_some()));
    meta.insert("junk".into(), Value::Bool(false));
    match rule {
        Some(r) => {
            meta.insert("sensitive_rule".into(), json!(r));
            meta.insert(
                "triage".into(),
                json!({"state": triage::SKIPPED, "reason": r}),
            );
        }
        None => {
            meta.insert(
                "triage".into(),
                json!({"state": triage::UNKNOWN, "reason": "no_model"}),
            );
        }
    }
    if !found.is_empty() {
        let p: Map<String, Value> = [
            ("card", found.card),
            ("ssn", found.ssn),
            ("iban", found.iban),
            ("secret", found.secret),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(k, n)| (k.to_owned(), json!(n)))
        .collect();
        meta.insert("pii".into(), Value::Object(p));
    }
    meta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_confirm_and_unflagged_is_safe() {
        let ex =
            json!({"from": "Dr Patel <reception@northside-clinic.example>", "subject": "Results"});
        let m = rule_labels("m1", &[], Some("Results"), "Your appointment", &ex, false);
        assert_eq!(m["sensitive"], true);
        assert!(m["sensitive_rule"].is_string());
        assert_eq!(m["triage"]["state"], "skipped");

        let m = rule_labels(
            "m2",
            &[],
            Some("Card"),
            "card 4111 1111 1111 1111",
            &json!({}),
            false,
        );
        assert_eq!(m["sensitive_rule"], "pii");
        assert_eq!(m["pii"]["card"], 1);

        let m = rule_labels("m3", &[], Some("Lunch?"), "Friday works", &json!({}), false);
        assert_eq!(m["sensitive"], false);
        assert_eq!(m["triage"]["reason"], "no_model");
        assert!(m.get("sensitive_rule").is_none());

        let m = rule_labels("m4", &[], None, "hi", &json!({}), true);
        assert_eq!(m["sensitive_rule"], "connector");
    }
}
