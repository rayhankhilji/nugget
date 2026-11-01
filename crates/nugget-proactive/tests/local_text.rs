//! The English fallback of every card, brief line, action label and confirmation equals the
//! desktop's `en.json` template rendered with the structured args, so the app, the CLI and MCP
//! say the same thing.

mod common;

use std::collections::BTreeMap;

use common::*;
use nugget_proactive::candidate::LocalText;
use nugget_proactive::text::{Money, en_date, en_time};
use nugget_proactive::{CandidateKind, Proactive};
use serde_json::Value;
use time::{Date, Duration, Time};

fn catalog() -> BTreeMap<String, String> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../apps/desktop/src/i18n/locales/en.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The body of `{...}` starting at `s[0] == '{'`, and the rest after its closing brace.
fn group(s: &str) -> (&str, &str) {
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return (&s[1..i], &s[i + 1..]);
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced {s}")
}

/// `one {..} other {..}` → the branch for `pick`, else `other`.
fn branch<'a>(mut s: &'a str, pick: &str) -> &'a str {
    let mut other = "";
    while let Some(i) = s.find('{') {
        let label = s[..i].trim();
        let (body, rest) = group(&s[i..]);
        if label == pick {
            return body;
        }
        if label == "other" {
            other = body;
        }
        s = rest;
    }
    other
}

/// A tiny ICU subset, as English `Intl` renders it: `{x}`, plural one/other with `#`, select,
/// `date, medium`, `time, short`, and an `amount` with a `currency` arg.
fn render(t: &str, args: &BTreeMap<String, Value>) -> String {
    let text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        v => v.to_string(),
    };
    let mut out = String::new();
    let mut s = t;
    while let Some(i) = s.find('{') {
        out.push_str(&s[..i]);
        let (body, rest) = group(&s[i..]);
        s = rest;
        let mut parts = body.splitn(3, ',').map(str::trim);
        let name = parts.next().unwrap();
        let arg = args
            .get(name)
            .unwrap_or_else(|| panic!("no arg {name} for {t}"));
        out.push_str(&match (parts.next(), parts.next()) {
            (None, _) if name == "amount" => {
                let currency = args["currency"].as_str().unwrap().to_owned();
                let minor = (arg.as_f64().unwrap() * 100.0).round() as i64;
                Money { currency, minor }.to_string()
            }
            (None, _) => text(arg),
            (Some("plural"), Some(cases)) => {
                let n = arg.as_i64().unwrap();
                render(branch(cases, if n == 1 { "one" } else { "other" }), args)
                    .replace('#', &n.to_string())
            }
            (Some("select"), Some(cases)) => render(branch(cases, &text(arg)), args),
            (Some("date"), _) => {
                let iso = text(arg);
                let d = Date::parse(
                    &iso[..10],
                    &time::format_description::well_known::Iso8601::DATE,
                )
                .unwrap();
                en_date(d)
            }
            (Some("time"), _) => {
                let iso = text(arg);
                let (h, m) = (iso[11..13].parse().unwrap(), iso[14..16].parse().unwrap());
                en_time(Time::from_hms(h, m, 0).unwrap())
            }
            other => panic!("unsupported {other:?} in {t}"),
        });
    }
    out.push_str(s);
    out
}

fn check(cat: &BTreeMap<String, String>, lt: &LocalText, fallback: &str, seen: &mut Vec<String>) {
    let key = format!("today.text.{}", lt.key);
    let template = cat
        .get(&key)
        .unwrap_or_else(|| panic!("{key} missing in en.json"));
    assert_eq!(render(template, &lt.args), fallback, "{key}");
    seen.push(lt.key.clone());
}

#[tokio::test]
async fn english_fallbacks_match_the_en_catalog() {
    let cat = catalog();
    let f = fixture();
    put_mails(
        &f.store,
        "m-ask||lena@vendor.example|Signed form|4||||Could you send me the signed form?
m-solo|||Plan|2||||I'll send the plan by Friday.",
    );
    let todo = "- [ ] File the expense report (due 2026-10-29)";
    let note =
        nugget_store::NewItem::new("files:notes", "todo.md", nugget_store::content_hash(todo))
            .with_title("Admin")
            .with_mime("text/markdown")
            .with_text(todo)
            .with_modified_at(now() - Duration::days(2))
            .with_metadata(serde_json::json!({"mime": "text/markdown", "sensitive": false}));
    f.store.upsert_item(&note).unwrap();
    let p = Proactive::new(f.store.clone(), cfg());
    let mut seen = Vec::new();
    for at in [now(), now() + Duration::days(3)] {
        p.refresh(at).await.unwrap();
        for c in p.all_cards().unwrap() {
            check(&cat, c.title_text.as_ref().unwrap(), &c.title, &mut seen);
            if let Some(d) = &c.detail_text {
                check(&cat, d, &c.detail, &mut seen);
            }
            for a in &c.actions {
                check(&cat, &a.summary_text(), &a.preview().summary, &mut seen);
                if let Some(q) = a.confirmation_text() {
                    check(&cat, &q, a.requires_confirmation().unwrap(), &mut seen);
                }
            }
        }
        let b = p.brief(at).unwrap();
        for c in b.top.iter().chain(&b.overdue).chain(&b.renewals) {
            check(&cat, c.title_text.as_ref().unwrap(), &c.title, &mut seen);
            if let Some(s) = &c.summary_text {
                check(&cat, s, &c.summary, &mut seen);
            }
        }
    }
    // Every card kind was exercised.
    let kinds: Vec<CandidateKind> = p.all_cards().unwrap().iter().map(|c| c.kind).collect();
    for k in [
        CandidateKind::UnansweredThread,
        CandidateKind::Commitment,
        CandidateKind::MeetingPrep,
        CandidateKind::Renewal,
        CandidateKind::FollowUp,
    ] {
        assert!(kinds.contains(&k), "{k:?} not covered");
    }
    // Reviewed by hand, not reached by this fixture: `commitment.waiting`, `commitment.mine`,
    // `renewal.renews`, `renewal.trial` (as their dated or priced siblings minus that part) and
    // `todo.notes`. `confirm.risk` falls back to the policy's own, more specific reason.
    seen.sort();
    seen.dedup();
    for k in [
        "todo.note",
        "commitment.overdue",
        "renewal.renews_amount",
        "follow_up.title",
    ] {
        assert!(seen.iter().any(|s| s == k), "{k} not checked: {seen:?}");
    }
}
