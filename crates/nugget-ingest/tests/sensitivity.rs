//! QA GG4-6: GG4's persona mail (therapy, clinic, bank fraud alert, HR salary letter,
//! verification code) must be marked sensitive at ingest, with or without a local model, and
//! ordinary mail must not be. Fixtures: `tests/fixtures/gg4/` (from `qa/GG4/persona/world`).

mod common;

use common::*;
use nugget_connector::{Change, Content, Document};
use nugget_core::capability::{Locality, Tier};
use serde_json::json;

const SENSITIVE: &[(&str, &[u8])] = &[
    ("s6-lab", include_bytes!("fixtures/gg4/s6-lab.eml")),
    ("s6-nurse", include_bytes!("fixtures/gg4/s6-nurse.eml")),
    ("s6-bank", include_bytes!("fixtures/gg4/s6-bank.eml")),
    ("s6-hr", include_bytes!("fixtures/gg4/s6-hr.eml")),
    ("s6-therapy", include_bytes!("fixtures/gg4/s6-therapy.eml")),
    ("s6-pw", include_bytes!("fixtures/gg4/s6-pw.eml")),
];

const ORDINARY: &[(&str, &[u8])] = &[
    (
        "s1-dana-ask",
        include_bytes!("fixtures/gg4/s1-dana-ask.eml"),
    ),
    ("s7-mom", include_bytes!("fixtures/gg4/s7-mom.eml")),
    ("n-news1", include_bytes!("fixtures/gg4/n-news1.eml")),
    ("n-promo", include_bytes!("fixtures/gg4/n-promo.eml")),
];

fn mail(key: &str, raw: &[u8]) -> Step {
    let mut d = Document::new(key, "message/rfc822", Content::Bytes(raw.to_vec()));
    d.version = Some("1".into());
    Step::Send(Change::Upsert(d))
}

async fn ingest(h: &Harness) {
    let mut script: Vec<Step> = SENSITIVE
        .iter()
        .chain(ORDINARY)
        .map(|(k, raw)| mail(k, raw))
        .collect();
    script.push(Step::Return(json!("done")));
    h.factory.state("mail").script(script);
    h.add("mail").await;
    eventually("pass", || h.cursor("mail") == Some(json!("done"))).await;
}

#[tokio::test]
async fn gg4_sensitive_mail_is_flagged_without_a_model() {
    let h = start(Opts::default()).await;
    ingest(&h).await;
    for (key, _) in SENSITIVE {
        let it = h.item("mail", key).unwrap();
        assert_eq!(
            it.metadata["sensitive"],
            json!(true),
            "{key}: {}",
            it.metadata
        );
        assert!(it.metadata["sensitive_rule"].is_string(), "{key}");
    }
    for (key, _) in ORDINARY {
        let it = h.item("mail", key).unwrap();
        assert_eq!(
            it.metadata["sensitive"],
            json!(false),
            "{key}: {}",
            it.metadata
        );
    }
    // Sensitive is not hidden: the therapy mail is found by local search.
    let hits = h.store.search_text("Thursday session", 10).unwrap();
    assert!(
        hits.iter()
            .any(|hit| format!("{hit:?}").contains("s6-therapy")),
        "{hits:?}"
    );
    h.ingest.shutdown().await;
}

#[tokio::test]
async fn gg4_sensitive_mail_is_flagged_even_when_the_model_under_flags() {
    // The fake model labels these mails not sensitive (no marker word): the rules still hold,
    // and the model is not asked about them at all.
    let local = FakeProvider::new("local-chat", Locality::OnDevice);
    let h = start(Opts {
        router: Some(router(&[(
            local.clone(),
            vec![Tier::Triage, Tier::Embedding],
        )])),
        ..Default::default()
    })
    .await;
    ingest(&h).await;
    eventually_async("triage settled", || {
        let ingest = h.ingest.clone();
        async move { ingest.status().await.unwrap().triage.unwrap().pending == 0 }
    })
    .await;
    for (key, _) in SENSITIVE {
        let it = h.item("mail", key).unwrap();
        assert_eq!(
            it.metadata["sensitive"],
            json!(true),
            "{key}: {}",
            it.metadata
        );
        assert_eq!(it.metadata["triage"]["state"], json!("skipped"), "{key}");
    }
    let asked = local.chats().join("\n");
    assert!(
        !asked.contains("metformin") && !asked.contains("118,500"),
        "{asked}"
    );
    for (key, _) in ORDINARY {
        let it = h.item("mail", key).unwrap();
        assert_eq!(it.metadata["triage"]["state"], json!("done"), "{key}");
        assert_eq!(it.metadata["sensitive"], json!(false), "{key}");
    }
    h.ingest.shutdown().await;
}
