//! An extractor upgrade re-extracts everything and cleans up what an older extractor left.

mod common;

use common::*;
use nugget_core::ids::ItemRef;
use nugget_graph::{ClaimObject, ClaimStatus, EntityKind, Evidence, Mention, NewClaim, NewEntity};
use nugget_knowledge::{EXTRACTOR_VERSION, ItemFeed, KnowledgeConfig};
use serde_json::json;

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

#[tokio::test]
async fn an_upgrade_retracts_items_deleted_meanwhile() {
    let env = Env::new();
    Mail {
        key: "1",
        to: TO,
        from: "Bart Fenwick <bart@cindergate.test>",
        ..Default::default()
    }
    .put(&env);
    let k = env.knowledge(KnowledgeConfig::default(), None);
    run(&k).await;
    let bart = env.entity(EntityKind::Person, "Bart Fenwick");

    // The mail disappears while the pipeline is not looking (no change-log position kept) and
    // the next run is an upgrade run.
    env.store
        .delete_item(&item_ref("imap:me@corp.example", "1"))
        .unwrap();
    let mut state = ItemFeed::get_state(&env.store, "knowledge")
        .unwrap()
        .unwrap();
    state["version"] = json!(1);
    state["item_cursor"] = serde_json::Value::Null;
    ItemFeed::set_state(&env.store, "knowledge", state).unwrap();

    let stats = run(&k).await;
    assert_eq!(stats.items_deleted, 1, "{stats:?}");
    assert!(env.g().get_entity(&bart.id).unwrap().is_none(), "pruned");
}
