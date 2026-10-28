//! M2 wave 2 contract adoption: rejected facts, `valid_to` on supersede, the item change log,
//! provenance of identifiers, and batch writes on the SQLite graph.

mod common;

use std::sync::Arc;

use common::*;
use nugget_graph::{ClaimObject, ClaimStatus, EntityKind, GraphStore, Identifier};
use nugget_knowledge::{Knowledge, KnowledgeConfig, MeConfig, RunBudget};
use nugget_store::{NewChunk, NewItem, Store, content_hash};
use serde_json::json;

fn cfg() -> KnowledgeConfig {
    KnowledgeConfig {
        me: MeConfig {
            name: Some("Tanuj".into()),
            emails: vec!["tanuj@trylumi.co".into()],
            infer_from_sent: true,
        },
        ..Default::default()
    }
}

fn card(key: &str, org: &str, modified: &str) -> NewItem {
    NewItem::new(
        "contacts:phone",
        key,
        content_hash(format!("{key}{org}{modified}")),
    )
    .with_mime("application/json")
    .with_version(format!("{key}{org}{modified}"))
    .with_modified_at(
        time::OffsetDateTime::parse(modified, &time::format_description::well_known::Rfc3339)
            .unwrap(),
    )
    .with_metadata(json!({
        "source": {"contact": {"name": "Alice Smith", "emails": ["alice@example.org"],
                                "organization": org}},
        "sensitive": false
    }))
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

#[tokio::test]
async fn superseding_sets_valid_to_and_reactivation_clears_it() {
    let env = Env::new();
    env.put(
        card("old", "Acme", "2025-01-01T00:00:00Z"),
        &["Alice Smith, Acme"],
    );
    let k = env.knowledge(cfg(), None);
    run(&k).await;
    env.put(
        card("new", "Globex", "2026-02-01T00:00:00Z"),
        &["Alice Smith, Globex"],
    );
    run(&k).await;
    let alice = env.entity(EntityKind::Person, "Alice Smith");
    let acme = env.entity(EntityKind::Organization, "Acme");
    let old = env
        .claims(&alice, "works_at")
        .into_iter()
        .find(|c| c.object == ClaimObject::Entity(acme.id.clone()))
        .unwrap();
    assert_eq!(old.status, ClaimStatus::Superseded);
    assert_eq!(
        old.valid_to,
        Some(
            time::OffsetDateTime::parse(
                "2026-02-01T00:00:00Z",
                &time::format_description::well_known::Rfc3339
            )
            .unwrap()
        ),
        "valid until the newer value's evidence"
    );

    // The newer card goes away: the old value is current again, open-ended.
    env.store
        .delete_item(&item_ref("contacts:phone", "new"))
        .unwrap();
    let stats = run(&k).await;
    assert_eq!(stats.claims_reactivated, 1);
    let back = env.g().get_claim(old.id).unwrap().unwrap();
    assert_eq!(back.status, ClaimStatus::Active);
    assert_eq!(back.valid_to, None);
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
async fn runs_after_the_first_only_look_at_changed_and_deleted_items() {
    let env = Env::new();
    for i in 0..5 {
        env.put(plain(&format!("n{i}"), &format!("body {i}")), &["text"]);
    }
    let k = env.knowledge(cfg(), None);
    let first = run(&k).await;
    assert_eq!(first.planned, 5);
    let idle = run(&k).await;
    assert_eq!(
        idle.planned, 0,
        "nothing changed: nothing planned (no full diff)"
    );

    env.put(plain("n1", "body 1, edited"), &["text"]);
    env.store.delete_item(&item_ref("notes", "n2")).unwrap();
    env.put(plain("n9", "new"), &["text"]);
    let next = run(&k).await;
    assert_eq!(next.planned, 3);
    assert_eq!(next.items_deleted, 1);
    assert_eq!(next.items_processed, 2);

    // A budget cut keeps the cursor: the rest is picked up by the next run.
    for i in 0..3 {
        env.put(plain(&format!("n{i}"), &format!("third {i}")), &["text"]);
    }
    let cut = k
        .run_once(RunBudget {
            max_items: 1,
            ..RunBudget::unlimited()
        })
        .await
        .unwrap();
    assert_eq!(cut.items_processed, 1);
    assert_eq!(cut.remaining, 2);
    let rest = run(&k).await;
    assert_eq!(rest.items_processed, 2);
    assert_eq!(rest.items_unchanged + rest.items_processed, rest.planned);
    assert_eq!(run(&k).await.planned, 0);
}

#[tokio::test]
async fn identifiers_record_the_items_that_supplied_them() {
    let env = Env::new();
    Mail {
        key: "1",
        from: "Alice Smith <alice@acme.com>",
        ..Default::default()
    }
    .put(&env);
    let k = env.knowledge(cfg(), None);
    run(&k).await;
    let alice = env.entity(EntityKind::Person, "Alice Smith");
    let p = env.g().entity_provenance(&alice.id).unwrap().unwrap();
    let (_, items) = p
        .identifiers
        .iter()
        .find(|(i, _)| *i == Identifier::new("email", "alice@acme.com"))
        .unwrap();
    assert_eq!(items, &vec![item_ref("imap:me@corp.example", "1")]);
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
