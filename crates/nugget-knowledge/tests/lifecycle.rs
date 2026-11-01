//! Work queue and claim lifecycle: retraction on edit and delete, superseding, idempotent and
//! resumable runs, change notifications, rejected facts, extractor upgrades.

mod common;

use common::*;
use std::sync::Arc;

use nugget_core::ids::ItemRef;
use nugget_graph::{
    ClaimObject, ClaimStatus, EntityKind, Evidence, GraphStore, Identifier, Mention, NewClaim,
    NewEntity,
};
use nugget_knowledge::{
    EXTRACTOR_VERSION, ItemFeed, Knowledge, KnowledgeConfig, MeConfig, RunBudget,
};
use nugget_store::{NewChunk, NewItem, Store, content_hash};
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
    // Alice is left with nothing behind her: she is pruned with her retracted claims.
    assert!(stats.entities_pruned >= 1);
    assert!(env.g().get_entity(&alice.id).unwrap().is_none());
    assert!(env.try_entity(EntityKind::Person, "Alice Smith").is_none());
    let all = env.claims(&me, "corresponded_with");
    assert!(
        all.iter()
            .all(|c| c.object != ClaimObject::Entity(alice.id.clone()))
    );
    assert!(env.g().mentions_for(&alice.id, 0).unwrap().is_empty());
    assert!(
        env.g().get_entity(&me.id).unwrap().is_some(),
        "never the user"
    );
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
    assert_eq!(old.valid_to, Some(at("2026-02-01T00:00:00Z")));

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
    assert_eq!(active[0].valid_to, None);
}

#[tokio::test]
async fn a_rejected_fact_stays_rejected_after_its_source_is_re_extracted() {
    let env = Env::new();
    env.put(
        card("c1", "Acme", "2025-01-01T00:00:00Z"),
        &["Alice Smith, Acme"],
    );
    let k = env.knowledge(cfg(), None);
    run(&k).await;
    let alice = env.entity(EntityKind::Person, "Alice Smith");
    let works = env.active(&alice, "works_at");
    assert_eq!(works.len(), 1);
    let id = works[0].id;
    env.g().set_claim_status(id, ClaimStatus::Rejected).unwrap();

    // The source changes (same fact, new wording): it is retracted and extracted again.
    env.put(
        card("c1", "Acme", "2025-02-01T00:00:00Z"),
        &["Alice Smith, Acme (updated)"],
    );
    let stats = run(&k).await;
    assert_eq!(stats.items_processed, 1);
    let c = env.g().get_claim(id).unwrap().unwrap();
    assert_eq!(c.status, ClaimStatus::Rejected, "not revived");
    assert_eq!(c.evidence.len(), 1, "the new evidence is kept on it");
    assert!(env.active(&alice, "works_at").is_empty());

    // Even a full re-extraction (reset) and a second source with the same fact keep it.
    k.reset().await.unwrap();
    env.put(
        card("c2", "Acme", "2025-03-01T00:00:00Z"),
        &["Alice Smith at Acme"],
    );
    run(&k).await;
    let all = env.claims(&alice, "works_at");
    assert_eq!(all.len(), 1, "no duplicate next to the rejection");
    assert_eq!(all[0].id, id);
    assert_eq!(all[0].status, ClaimStatus::Rejected);
    assert_eq!(all[0].evidence.len(), 2);

    // Deleting every source keeps the rejection too (and the entity: a rejection is a user
    // decision, so it is not an orphan).
    env.store
        .delete_source(&nugget_core::ids::SourceId::new("contacts:phone"))
        .unwrap();
    run(&k).await;
    assert_eq!(
        env.g().get_claim(id).unwrap().unwrap().status,
        ClaimStatus::Rejected
    );
}

fn plain(key: &str, body: &str) -> NewItem {
    // No connector version: the full diff has to look at such items on every run.
    NewItem::new("notes", key, content_hash(body))
        .with_mime("text/markdown")
        .with_title(key)
        .with_metadata(
            json!({"source": {}, "mime": "text/markdown", "extracted": {},
                              "sensitive": false, "junk": false}),
        )
}

#[tokio::test]
async fn budget_cut_runs_continue_their_plan_and_then_see_what_changed_meanwhile() {
    let env = Env::new();
    for i in 0..6 {
        env.put(plain(&format!("n{i}"), &format!("body {i}")), &["text"]);
    }
    let k = env.knowledge(cfg(), None);
    let budget = RunBudget {
        max_items: 2,
        ..RunBudget::unlimited()
    };
    let first = k.run_once(budget).await.unwrap();
    assert_eq!((first.planned, first.remaining), (6, 4));
    // Meanwhile: a processed item is edited, a planned one deleted, a new one added.
    env.put(plain("n0", "body 0, edited"), &["text"]);
    env.store.delete_item(&item_ref("notes", "n5")).unwrap();
    env.put(plain("n9", "new"), &["text"]);
    let second = k.run_once(budget).await.unwrap();
    assert_eq!((second.planned, second.remaining), (4, 2));
    let third = k.run_once(budget).await.unwrap();
    assert_eq!(third.remaining, 0);
    assert_eq!(third.items_processed + third.items_deleted, 2);
    // The changes made during the drain are planned next.
    let next = run(&k).await;
    assert_eq!(next.items_processed, 2, "n0 and n9");
    assert_eq!(run(&k).await.planned, 0);

    // A reset drops what a cut run left behind.
    for i in 0..4 {
        env.put(plain(&format!("n{i}"), &format!("again {i}")), &["text"]);
    }
    let cut = k.run_once(budget).await.unwrap();
    assert!(cut.remaining > 0);
    k.reset().await.unwrap();
    let all = run(&k).await;
    assert_eq!(all.planned, 6, "every item after a reset");
    assert_eq!(all.remaining, 0);
}

#[tokio::test]
async fn the_sqlite_graph_gets_the_same_results_with_batched_writes() {
    let store = Store::open_in_memory().unwrap();
    let graph = Arc::new(store.graph());
    for (key, from) in [
        ("1", "Alice Smith <alice@acme.com>"),
        ("2", "Alice Smith <alice@acme.com>"),
        ("3", "Bob Jones <bob@acme.com>"),
    ] {
        let item = Mail {
            key,
            from,
            ..Default::default()
        }
        .item();
        store
            .upsert_item_with_chunks(&item, &[NewChunk::text("Hi there")])
            .unwrap();
    }
    let k = Knowledge::new(store.clone(), graph.clone(), None, cfg());
    let stats = k.run_once(RunBudget::unlimited()).await.unwrap();
    assert_eq!(stats.items_processed, 3);
    assert_eq!(stats.items_failed, 0);
    let g: &dyn GraphStore = graph.as_ref();
    let alice = g
        .find_by_identifier(&Identifier::new("email", "alice@acme.com"))
        .unwrap()
        .unwrap();
    let claims = g.claims_for(&alice, false).unwrap();
    let corr: Vec<_> = claims
        .iter()
        .filter(|c| c.predicate == "corresponded_with")
        .collect();
    assert_eq!(corr.len(), 1, "one claim for the triple across two items");
    assert_eq!(corr[0].evidence.len(), 2);
    assert!(corr[0].confidence > 0.9, "aggregated after the batch");
    assert_eq!(g.mentions_for(&alice, 0).unwrap().len(), 2);
}

const TO: &[&str] = &["Robin <robin@gmail.com>"];

fn mention(entity: &nugget_core::ids::EntityId, item: &ItemRef, surface: &str) -> Mention {
    Mention {
        entity: entity.clone(),
        item: item.clone(),
        chunk_id: None,
        surface: surface.into(),
        confidence: 0.6,
    }
}

#[tokio::test]
async fn an_upgrade_from_v1_folds_and_prunes_old_duplicates() {
    const { assert!(EXTRACTOR_VERSION > 1) };
    let env = Env::new();
    Mail {
        key: "1",
        to: TO,
        from: "Bart Fenwick <bart@cindergate.test>",
        body: "See you at the offsite.",
        ..Default::default()
    }
    .put(&env);
    let k = env.knowledge(KnowledgeConfig::default(), None);
    run(&k).await;
    let bart = env.entity(EntityKind::Person, "Bart Fenwick");
    let org = env.entity(EntityKind::Organization, "Cindergate");
    let item = item_ref("imap:me@corp.example", "1");

    // What an older extractor left behind from the same mail: a name-only twin of Bart with its
    // own mention and claim, and an entity the current extractor no longer finds.
    let g = env.g();
    let twin = g
        .create_entity(&NewEntity::new(EntityKind::Person, "Bart Fenwick"))
        .unwrap();
    g.add_mention(&mention(&twin, &item, "Bart Fenwick"))
        .unwrap();
    g.add_claim(&NewClaim {
        subject: twin.clone(),
        predicate: "works_at".into(),
        object: ClaimObject::Entity(org.id.clone()),
        text: "Bart Fenwick works at Cindergate.".into(),
        confidence: 0.5,
        status: ClaimStatus::Active,
        evidence: vec![Evidence {
            item: item.clone(),
            chunk_id: None,
            quote: None,
        }],
        valid_from: None,
        valid_to: None,
    })
    .unwrap();
    let stray = g
        .create_entity(&NewEntity::new(EntityKind::Topic, "offsite"))
        .unwrap();
    g.add_mention(&mention(&stray, &item, "offsite")).unwrap();

    // Same extractor version: nothing changed, nothing is redone, the duplicates stay.
    let stats = run(&k).await;
    assert_eq!(stats.items_processed, 0);
    let barts = |env: &Env| {
        env.entities(EntityKind::Person)
            .into_iter()
            .filter(|e| e.name == "Bart Fenwick")
            .count()
    };
    assert_eq!(barts(&env), 2);

    // The state as version 1 left it: the next run is an upgrade run.
    let mut state = ItemFeed::get_state(&env.store, "knowledge")
        .unwrap()
        .expect("state");
    assert_eq!(state["version"], json!(EXTRACTOR_VERSION));
    state["version"] = json!(1);
    ItemFeed::set_state(&env.store, "knowledge", state).unwrap();

    let stats = run(&k).await;
    assert_eq!(stats.items_failed, 0);
    assert_eq!(stats.items_processed, 1, "every item is extracted again");
    assert_eq!(barts(&env), 1, "the twin is gone");
    match env.g().get_entity(&twin).unwrap() {
        None => {}
        Some(e) => assert_eq!(e.id, bart.id, "the twin is folded into Bart"),
    }
    assert!(
        env.g().get_entity(&stray).unwrap().is_none(),
        "nothing supports the stray entity any more: pruned"
    );
    let bart = env.entity(EntityKind::Person, "Bart Fenwick");
    let jobs = env.active(&bart, "works_at");
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0].object, ClaimObject::Entity(org.id.clone()));
    let state = ItemFeed::get_state(&env.store, "knowledge")
        .unwrap()
        .unwrap();
    assert_eq!(state["version"], json!(EXTRACTOR_VERSION));

    // Upgraded: the next run has nothing to do.
    let stats = run(&k).await;
    assert_eq!(stats.items_processed, 0);
}
