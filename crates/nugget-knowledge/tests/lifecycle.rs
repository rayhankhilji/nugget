//! Work queue and claim lifecycle: retraction on edit and delete, superseding, idempotent and
//! resumable runs, change notifications.

mod common;

use common::*;
use nugget_graph::{ClaimObject, ClaimStatus, EntityKind};
use nugget_knowledge::{KnowledgeConfig, MeConfig, RunBudget};
use serde_json::json;

fn cfg() -> KnowledgeConfig {
    KnowledgeConfig {
        me: MeConfig {
            emails: vec!["tanuj@trylumi.co".into()],
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test]
async fn rerunning_is_idempotent() {
    let env = Env::new();
    Mail::default().put(&env);
    env.put(
        note(
            "files:notes",
            "p.md",
            "Apollo",
            json!({"type": "project"}),
            &["rust"],
            &["Missing"],
            "x #rust",
        ),
        &["x #rust"],
    );
    let k = env.knowledge(cfg(), None);
    let first = run(&k).await;
    assert_eq!(first.items_processed, 2);
    let counts = env.g().counts().unwrap();
    let claims = env.all_claims();

    let second = run(&k).await;
    assert_eq!(second.planned, 0, "nothing changed: nothing to do");
    assert_eq!(second.items_processed, 0);
    assert_eq!(second.changed_entities, 0);
    assert_eq!(env.g().counts().unwrap(), counts);

    // Forced re-extraction of everything is idempotent too, and claim ids are stable.
    k.reset().await.unwrap();
    let third = run(&k).await;
    assert_eq!(third.items_processed, 2);
    assert_eq!(third.entities_created, 0);
    assert_eq!(third.claims_added, 0);
    assert_eq!(third.claims_retracted, 0);
    let after = env.g().counts().unwrap();
    assert_eq!(after.entities, counts.entities);
    assert_eq!(after.claims, counts.claims);
    let mut ids: Vec<_> = claims.iter().map(|c| c.id).collect();
    let mut ids_after: Vec<_> = env.all_claims().iter().map(|c| c.id).collect();
    ids.sort();
    ids_after.sort();
    assert_eq!(ids, ids_after);
    // Mentions were replaced, not duplicated.
    assert_eq!(after.mentions, counts.mentions);
}

#[tokio::test]
async fn edits_and_deletes_retract_evidence() {
    let env = Env::new();
    Mail {
        key: "1",
        from: "Alice Smith <alice@acme.com>",
        ..Default::default()
    }
    .put(&env);
    Mail {
        key: "2",
        from: "Alice Smith <alice@acme.com>",
        date: "2026-03-05T10:00:00Z",
        body: "Second message",
        ..Default::default()
    }
    .put(&env);
    let k = env.knowledge(cfg(), None);
    run(&k).await;
    let alice = env.entity(EntityKind::Person, "Alice Smith");
    let me = env.me();
    let corr = env.active(&me, "corresponded_with");
    assert_eq!(corr.len(), 1);
    assert_eq!(corr[0].evidence.len(), 2);
    let base = corr[0].confidence;
    // Independent evidence raises confidence.
    assert!(base > 0.9);

    let mut rx = k.subscribe();

    // Edit message 2: it now comes from Bob.
    Mail {
        key: "2",
        from: "Bob Jones <bob@acme.com>",
        date: "2026-03-05T10:00:00Z",
        body: "Edited",
        ..Default::default()
    }
    .put(&env);
    let stats = run(&k).await;
    assert_eq!(stats.items_processed, 1);
    let changed = rx.try_recv().unwrap();
    assert!(changed.contains(&alice.id));
    let corr = env.active(&me, "corresponded_with");
    assert_eq!(corr.len(), 2);
    let with_alice = corr
        .iter()
        .find(|c| c.object == ClaimObject::Entity(alice.id.clone()))
        .unwrap();
    assert_eq!(with_alice.evidence.len(), 1);
    assert_eq!(
        with_alice.evidence[0].item,
        item_ref("imap:me@corp.example", "1")
    );
    let mentions = env.g().mentions_for(&alice.id, 0).unwrap();
    assert!(mentions.iter().all(|m| m.item.key.as_str() == "1"));

    // Delete message 1: Alice loses her last evidence.
    env.store
        .delete_item(&item_ref("imap:me@corp.example", "1"))
        .unwrap();
    let stats = run(&k).await;
    assert_eq!(stats.items_deleted, 1);
    assert!(
        stats.claims_retracted >= 2,
        "corresponded_with and works_at"
    );
    let all = env.claims(&me, "corresponded_with");
    let alice_claim = all
        .iter()
        .find(|c| c.object == ClaimObject::Entity(alice.id.clone()))
        .unwrap();
    assert_eq!(alice_claim.status, ClaimStatus::Retracted);
    assert!(env.active(&alice, "works_at").is_empty());
    assert!(env.g().mentions_for(&alice.id, 0).unwrap().is_empty());
    // Bob's claims are untouched.
    let bob = env.entity(EntityKind::Person, "Bob Jones");
    assert_eq!(env.active(&bob, "works_at").len(), 1);

    // Deleting everything of a source retracts it all.
    env.store
        .delete_source(&nugget_core::ids::SourceId::new("imap:me@corp.example"))
        .unwrap();
    run(&k).await;
    assert!(env.active(&me, "corresponded_with").is_empty());
    assert!(env.active(&bob, "works_at").is_empty());
}

#[tokio::test]
async fn newer_evidence_supersedes_single_valued_claims() {
    let env = Env::new();
    // Newer mail first, so the older one arrives later and becomes history immediately.
    Mail {
        key: "b",
        from: "Alice Smith <alice@globex.com>",
        date: "2026-06-01T10:00:00Z",
        ..Default::default()
    }
    .put(&env);
    Mail {
        key: "a",
        from: "Alice Smith <alice@acme.com>",
        date: "2025-01-01T10:00:00Z",
        ..Default::default()
    }
    .put(&env);
    let k = env.knowledge(cfg(), None);
    run(&k).await;
    // Different addresses: two Alices, never merged on name. Merge them by contact card later;
    // here we check each address' org.
    let alices: Vec<_> = env
        .entities(EntityKind::Person)
        .into_iter()
        .filter(|e| e.name == "Alice Smith")
        .collect();
    assert_eq!(alices.len(), 2);

    // A person with an employer change on one identity: a contact record dated later.
    let env = Env::new();
    let card = |key: &str, org: &str, modified: &str| {
        nugget_store::NewItem::new(
            "contacts:phone",
            key,
            nugget_store::content_hash(format!("{key}{org}")),
        )
        .with_mime("application/json")
        .with_version(format!("{key}{org}"))
        .with_modified_at(
            time::OffsetDateTime::parse(modified, &time::format_description::well_known::Rfc3339)
                .unwrap(),
        )
        .with_metadata(json!({
            "source": {"contact": {"name": "Alice Smith", "emails": ["alice@example.org"],
                                    "organization": org}},
            "sensitive": false
        }))
    };
    env.put(
        card("old", "Acme", "2025-01-01T00:00:00Z"),
        &["Alice Smith, Acme"],
    );
    let k = env.knowledge(cfg(), None);
    run(&k).await;
    let alice = env.entity(EntityKind::Person, "Alice Smith");
    let acme = env.entity(EntityKind::Organization, "Acme");
    assert_eq!(env.active(&alice, "works_at").len(), 1);

    env.put(
        card("new", "Globex", "2026-02-01T00:00:00Z"),
        &["Alice Smith, Globex"],
    );
    let stats = run(&k).await;
    assert_eq!(stats.claims_superseded, 1);
    let globex = env.entity(EntityKind::Organization, "Globex");
    let active = env.active(&alice, "works_at");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].object, ClaimObject::Entity(globex.id.clone()));
    let old = env
        .claims(&alice, "works_at")
        .into_iter()
        .find(|c| c.object == ClaimObject::Entity(acme.id.clone()))
        .unwrap();
    assert_eq!(old.status, ClaimStatus::Superseded);

    // An even older record arrives: recorded as history, with an end date.
    env.put(
        card("older", "Initech", "2024-01-01T00:00:00Z"),
        &["Alice Smith, Initech"],
    );
    run(&k).await;
    let initech = env.entity(EntityKind::Organization, "Initech");
    let hist = env
        .claims(&alice, "works_at")
        .into_iter()
        .find(|c| c.object == ClaimObject::Entity(initech.id.clone()))
        .unwrap();
    assert_eq!(hist.status, ClaimStatus::Superseded);
    assert!(hist.valid_to.is_some());
    assert_eq!(
        env.active(&alice, "works_at")[0].object,
        ClaimObject::Entity(globex.id.clone())
    );

    // The newer record is deleted: the previous value comes back.
    env.store
        .delete_item(&item_ref("contacts:phone", "new"))
        .unwrap();
    let stats = run(&k).await;
    assert_eq!(stats.claims_reactivated, 1);
    let active = env.active(&alice, "works_at");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].object, ClaimObject::Entity(acme.id));
}

#[tokio::test]
async fn budgets_resume_where_they_stopped() {
    let env = Env::new();
    for i in 0..5 {
        let key = format!("m{i}");
        let from = format!("Person {i} <p{i}@corp{i}.com>");
        Mail {
            key: &key,
            from: &from,
            ..Default::default()
        }
        .put(&env);
    }
    let k = env.knowledge(cfg(), None);
    // `run_once` is `Send`: the daemon spawns it on its own task.
    let a = tokio::spawn({
        let k = k.clone();
        async move { k.run_once(RunBudget::items(2)).await }
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(a.items_processed, 2);
    assert_eq!(a.remaining, 3);
    let b = k.run_once(RunBudget::items(2)).await.unwrap();
    assert_eq!(b.items_processed, 2);
    assert_eq!(b.remaining, 1);
    let c = run(&k).await;
    assert_eq!(c.items_processed, 1);
    assert_eq!(c.remaining, 0);
    assert_eq!(
        env.entities(EntityKind::Person).len(),
        6,
        "five people and the user"
    );

    // A fresh handle over the same store continues from the persisted state.
    let k2 = env.knowledge(cfg(), None);
    assert_eq!(run(&k2).await.planned, 0);
    let st = k2.status();
    assert_eq!(st.runs, 1);
    assert_eq!(st.processed_total, 5);
    assert!(!st.running);
}
