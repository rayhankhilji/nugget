//! QA GG4-6 and GG5-2 (GG5 cases at the end). GG4's persona mail (therapy, clinic, bank fraud alert, HR salary letter,
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
    // No rule matches ordinary mail and no model can decide: unknown counts as sensitive.
    for (key, _) in ORDINARY {
        let it = h.item("mail", key).unwrap();
        assert!(
            it.metadata.get("sensitive_rule").is_none(),
            "{key}: {}",
            it.metadata
        );
        assert_eq!(
            it.metadata["sensitive"],
            json!(true),
            "{key}: {}",
            it.metadata
        );
        assert_eq!(it.metadata["triage"]["state"], json!("pending"), "{key}");
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
    // The fake model labels these mails not sensitive (no marker word): the rules still hold.
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
        // The (local) model labelled it not sensitive; the rule still wins.
        assert_eq!(it.metadata["triage"]["state"], json!("done"), "{key}");
        assert_eq!(it.metadata["triage"]["sensitive"], json!(false), "{key}");
        assert!(it.metadata["sensitive_rule"].is_string(), "{key}");
    }
    for (key, _) in ORDINARY {
        let it = h.item("mail", key).unwrap();
        assert_eq!(it.metadata["triage"]["state"], json!("done"), "{key}");
        assert_eq!(it.metadata["sensitive"], json!(false), "{key}");
    }
    h.ingest.shutdown().await;
}

const GG5_SENSITIVE: &[(&str, &str, &[u8])] = &[
    (
        "0:Finance/2026-04-30-payslip-april.md",
        "text/markdown",
        include_bytes!("fixtures/gg5/2026-04-30-payslip-april.md"),
    ),
    (
        "0:Finance/2026-05-21-new-debit-card.eml",
        "message/rfc822",
        include_bytes!("fixtures/gg5/2026-05-21-new-debit-card.eml"),
    ),
    (
        "0:Finance/2026-07-01-mortgage-statement.md",
        "text/markdown",
        include_bytes!("fixtures/gg5/2026-07-01-mortgage-statement.md"),
    ),
    (
        "0:Health/2026-05-12-gp-visit.md",
        "text/markdown",
        include_bytes!("fixtures/gg5/2026-05-12-gp-visit.md"),
    ),
    (
        "0:Health/2026-06-17-counselling.ics",
        "text/calendar",
        include_bytes!("fixtures/gg5/2026-06-17-counselling.ics"),
    ),
];

const GG5_ORDINARY: &[(&str, &str, &[u8])] = &[
    (
        "0:Mail/Inbox/2026-04-09-thursday-climbing.eml",
        "message/rfc822",
        include_bytes!("fixtures/gg5/2026-04-09-thursday-climbing.eml"),
    ),
    (
        "0:Notes/Projects/Harbor.md",
        "text/markdown",
        include_bytes!("fixtures/gg5/Harbor.md"),
    ),
    (
        "0:Calendar/2026-03-10-harbor-standup.ics",
        "text/calendar",
        include_bytes!("fixtures/gg5/2026-03-10-harbor-standup.ics"),
    ),
];

/// GG5-2: health and finance files are flagged by rules (content, folders, tags), the rest
/// count as sensitive until triage decides, and nothing reaches an allowed cloud embedder
/// while no local triage model exists.
#[tokio::test]
async fn gg5_health_and_finance_files_are_flagged_and_unknown_stays_sensitive() {
    let cloud = FakeProvider::new("cloud", Locality::Cloud);
    let h = start(Opts {
        cfg: nugget_ingest::IngestConfig {
            allow_cloud_embeddings: true,
            ..cfg()
        },
        router: Some(router(&[(cloud.clone(), vec![Tier::Embedding])])),
        ..Default::default()
    })
    .await;
    let mut script: Vec<Step> = GG5_SENSITIVE
        .iter()
        .chain(GG5_ORDINARY)
        .map(|(k, mime, raw)| {
            let mut d = Document::new(*k, *mime, Content::Bytes(raw.to_vec()));
            d.version = Some("1".into());
            Step::Send(Change::Upsert(d))
        })
        .collect();
    script.push(Step::Return(json!("done")));
    h.factory.state("files").script(script);
    h.add("files").await;
    eventually("pass", || h.cursor("files") == Some(json!("done"))).await;
    for (key, _, _) in GG5_SENSITIVE {
        let it = h.item("files", key).unwrap();
        assert_eq!(
            it.metadata["sensitive"],
            json!(true),
            "{key}: {}",
            it.metadata
        );
        assert!(
            it.metadata["sensitive_rule"].is_string(),
            "{key}: {}",
            it.metadata
        );
    }
    for (key, _, _) in GG5_ORDINARY {
        let it = h.item("files", key).unwrap();
        assert!(
            it.metadata.get("sensitive_rule").is_none(),
            "{key}: {}",
            it.metadata
        );
        assert_eq!(
            it.metadata["sensitive"],
            json!(true),
            "unknown is sensitive: {key}"
        );
    }
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let sent = cloud.embedded().join("\n");
    for secret in [
        "ferrous",
        "NCU-4471",
        "7,450",
        "Albers",
        "4000 0566",
        "Bouldering",
    ] {
        assert!(!sent.contains(secret), "{secret} reached the cloud");
    }
    // Sensitive items are still found locally.
    assert_eq!(h.store.search_text("ferrous sulfate", 10).unwrap().len(), 1);
    h.ingest.shutdown().await;
}
