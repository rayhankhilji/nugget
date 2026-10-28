//! Mail headers: people, organisations, works_at, corresponded_with, the user.

mod common;

use common::*;
use nugget_graph::{ClaimObject, EntityKind, Identifier};
use nugget_knowledge::{KnowledgeConfig, MeConfig};
use serde_json::json;

fn me_cfg() -> KnowledgeConfig {
    KnowledgeConfig {
        me: MeConfig {
            name: Some("Tanuj".into()),
            emails: vec!["tanuj@trylumi.co".into()],
            infer_from_sent: true,
        },
        ..Default::default()
    }
}

#[tokio::test]
async fn mail_headers_make_people_orgs_and_claims() {
    let env = Env::new();
    Mail {
        from: "Alice Smith <alice@acme.com>",
        to: &["Tanuj <tanuj@trylumi.co>", "\"Jones, Bob\" <bob@gmail.com>"],
        cc: &["no-reply@acme.com"],
        ..Default::default()
    }
    .put(&env);
    let k = env.knowledge(me_cfg(), None);
    let stats = run(&k).await;
    assert_eq!(stats.items_processed, 1);
    assert_eq!(stats.items_failed, 0);

    let alice = env.entity(EntityKind::Person, "Alice Smith");
    assert!(
        alice
            .identifiers
            .contains(&Identifier::new("email", "alice@acme.com"))
    );
    let bob = env.entity(EntityKind::Person, "Bob Jones");
    assert!(bob.aliases.is_empty() || bob.aliases.iter().all(|a| a != "Jones, Bob"));

    // Organisation from the corporate domain only.
    let acme = env.entity(EntityKind::Organization, "Acme");
    assert!(
        acme.identifiers
            .contains(&Identifier::new("domain", "acme.com"))
    );
    assert!(env.try_entity(EntityKind::Organization, "Gmail").is_none());
    assert_eq!(env.entities(EntityKind::Organization).len(), 1);

    // works_at with a confidence penalty, for the corporate address only.
    let works = env.active(&alice, "works_at");
    assert_eq!(works.len(), 1);
    assert_eq!(works[0].object, ClaimObject::Entity(acme.id.clone()));
    assert!(works[0].confidence <= 0.5);
    assert!(env.active(&bob, "works_at").is_empty());

    // The automated sender is nobody.
    assert!(
        env.try_entity(EntityKind::Person, "no-reply@acme.com")
            .is_none()
    );

    // The user corresponded with both people; evidence quotes the header.
    let me = env.me();
    assert_eq!(me.name, "Tanuj");
    let corr = env.active(&me, "corresponded_with");
    assert_eq!(corr.len(), 2);
    let to_alice = corr
        .iter()
        .find(|c| c.object == ClaimObject::Entity(alice.id.clone()))
        .unwrap();
    assert_eq!(to_alice.evidence.len(), 1);
    let ev = &to_alice.evidence[0];
    assert_eq!(ev.item, item_ref("imap:me@corp.example", "INBOX/1"));
    assert_eq!(
        ev.quote.as_deref(),
        Some("From: Alice Smith <alice@acme.com>")
    );

    // Mentions recorded for each occurrence.
    assert!(!env.g().mentions_for(&alice.id, 10).unwrap().is_empty());
    // Every claim carries evidence.
    for c in env.all_claims() {
        assert!(!c.evidence.is_empty(), "claim without evidence: {c:?}");
    }
}

#[tokio::test]
async fn user_is_learned_from_sent_mail_and_merged() {
    let env = Env::new();
    // Received first: the user's own address appears as an ordinary recipient.
    Mail {
        key: "INBOX/1",
        from: "Alice Smith <alice@acme.com>",
        to: &["Tanuj Gupta <tanuj@trylumi.co>"],
        ..Default::default()
    }
    .put(&env);
    let k = env.knowledge(KnowledgeConfig::default(), None);
    run(&k).await;
    let me_before = env.me();
    assert_eq!(me_before.name, "Me");
    // Not known yet: a person entity holds the address, and the user only received the mail.
    assert!(env.try_entity(EntityKind::Person, "Tanuj Gupta").is_some());

    // A message in the sent folder reveals the address.
    Mail {
        key: "Sent/1",
        from: "Tanuj Gupta <tanuj@trylumi.co>",
        to: &["Carol White <carol@globex.com>"],
        date: "2026-03-02T10:00:00Z",
        source_meta: json!({"folder": "[Gmail]/Sent Mail"}),
        ..Default::default()
    }
    .put(&env);
    run(&k).await;
    let me = env.me();
    assert_eq!(me.id, me_before.id);
    assert_eq!(me.name, "Tanuj Gupta");
    assert!(
        me.identifiers
            .contains(&Identifier::new("email", "tanuj@trylumi.co"))
    );
    // The stand-in person was merged into the user; no self-loops remain.
    let gupta = env.entities(EntityKind::Person);
    assert_eq!(gupta.iter().filter(|e| e.name.contains("Tanuj")).count(), 1);
    for c in env.g().claims_for(&me.id, false).unwrap() {
        assert!(!(c.subject == me.id && c.object == ClaimObject::Entity(me.id.clone())));
    }
    let carol = env.entity(EntityKind::Person, "Carol White");
    assert!(
        env.active(&me, "corresponded_with")
            .iter()
            .any(|c| c.object == ClaimObject::Entity(carol.id.clone()))
    );
    assert!(k.status().me.is_some());
}

#[tokio::test]
async fn gmail_sent_label_and_crowds() {
    let env = Env::new();
    let many: Vec<String> = (0..30)
        .map(|i| format!("P{i} Q <p{i}@corp{i}.com>"))
        .collect();
    let many_refs: Vec<&str> = many.iter().map(String::as_str).collect();
    Mail {
        source: "gmail:me",
        key: "m1",
        from: "Me Myself <me@gmail.com>",
        to: &many_refs,
        source_meta: json!({"labels": ["SENT"]}),
        ..Default::default()
    }
    .put(&env);
    let k = env.knowledge(KnowledgeConfig::default(), None);
    run(&k).await;
    let me = env.me();
    assert!(
        me.identifiers
            .contains(&Identifier::new("email", "me@gmail.com"))
    );
    // Over max_correspondents recipients: no corresponded_with for the crowd.
    assert!(env.active(&me, "corresponded_with").is_empty());
    // Freemail: no organisation for the user's own gmail address.
    assert!(env.try_entity(EntityKind::Organization, "Gmail").is_none());
}

#[tokio::test]
async fn unknown_user_links_only_the_sender() {
    let env = Env::new();
    Mail {
        from: "Dana Lee <dana@initech.com>",
        to: &["Eve Park <eve@initech.com>"],
        ..Default::default()
    }
    .put(&env);
    let cfg = KnowledgeConfig {
        me: MeConfig {
            infer_from_sent: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let k = env.knowledge(cfg, None);
    run(&k).await;
    let me = env.me();
    let corr = env.active(&me, "corresponded_with");
    assert_eq!(corr.len(), 1);
    let dana = env.entity(EntityKind::Person, "Dana Lee");
    assert_eq!(corr[0].object, ClaimObject::Entity(dana.id));
    assert!(corr[0].confidence < 0.9);
    // Both work at Initech, which exists once.
    assert_eq!(env.entities(EntityKind::Organization).len(), 1);
}
