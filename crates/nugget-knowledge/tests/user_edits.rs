//! A user's vault edit of a single-valued fact against re-extraction (GG5-4) and newer sources
//! (GG5-13). The vault records an edit as `GraphStore::supersede_by_user` on the old claim plus
//! a `user_asserted` claim; the session must keep the user's value current until a strictly
//! newer source says otherwise.

mod common;

use common::*;
use nugget_graph::{ClaimObject, ClaimStatus, EntityKind, NewClaim};
use nugget_knowledge::{KnowledgeConfig, MeConfig};
use serde_json::json;

fn cfg() -> KnowledgeConfig {
    KnowledgeConfig {
        me: MeConfig {
            emails: vec!["me@example.org".into()],
            ..Default::default()
        },
        ..Default::default()
    }
}

fn card(key: &str, org: &str, modified: &str, version: &str) -> nugget_store::NewItem {
    nugget_store::NewItem::new(
        "contacts:phone",
        key,
        nugget_store::content_hash(format!("{key}{org}{version}")),
    )
    .with_mime("application/json")
    .with_version(format!("{key}{org}{version}"))
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
async fn the_users_edit_wins_over_re_extraction_until_a_newer_source() {
    let env = Env::new();
    env.put(
        card("old", "Acme", "2025-01-01T00:00:00Z", "1"),
        &["Alice Smith, Acme"],
    );
    let k = env.knowledge(cfg(), None);
    run(&k).await;
    let alice = env.entity(EntityKind::Person, "Alice Smith");
    let acme = env.active(&alice, "works_at");
    assert_eq!(acme.len(), 1);
    let acme = acme[0].clone();

    // The user rewrites the line in the vault (no link: a literal value).
    env.g().supersede_by_user(acme.id).unwrap();
    let mine = env
        .g()
        .add_claim(&NewClaim {
            subject: alice.id.clone(),
            predicate: "works_at".into(),
            object: ClaimObject::Literal("Alice Smith is CTO at Acme (fixed by me)".into()),
            text: "Alice Smith is CTO at Acme (fixed by me)".into(),
            confidence: 1.0,
            status: ClaimStatus::UserAsserted,
            evidence: Vec::new(),
            valid_from: None,
            valid_to: None,
        })
        .unwrap();

    let current = |env: &Env| -> Vec<i64> {
        let mut v: Vec<i64> = env
            .active(&alice, "works_at")
            .iter()
            .map(|c| c.id)
            .collect();
        v.sort_unstable();
        v
    };

    // Everything is extracted again: the edit stands.
    k.reset().await.unwrap();
    run(&k).await;
    assert_eq!(current(&env), vec![mine]);
    let old = env.g().get_claim(acme.id).unwrap().unwrap();
    assert_eq!(old.status, ClaimStatus::Superseded);
    assert!(!old.evidence.is_empty(), "the evidence is kept");

    // The same source is saved again, now dated after the edit, still saying the same: the edit
    // still stands (the source did not change that fact).
    env.put(
        card("old", "Acme", "2099-01-01T00:00:00Z", "2"),
        &["Alice Smith, Acme"],
    );
    run(&k).await;
    assert_eq!(current(&env), vec![mine]);
    assert_eq!(
        env.g().get_claim(acme.id).unwrap().unwrap().status,
        ClaimStatus::Superseded
    );

    // An older source with another value: history, the user's value stays current.
    env.put(
        card("older", "Initech", "2024-01-01T00:00:00Z", "1"),
        &["Alice Smith, Initech"],
    );
    run(&k).await;
    assert_eq!(current(&env), vec![mine]);

    // A strictly newer source (dated after the edit) with another value supersedes the user's.
    env.put(
        card("new", "Globex", "2099-06-01T00:00:00Z", "1"),
        &["Alice Smith, Globex"],
    );
    run(&k).await;
    let globex = env.entity(EntityKind::Organization, "Globex");
    let active = env.active(&alice, "works_at");
    assert_eq!(active.len(), 1, "{active:?}");
    assert_eq!(active[0].object, ClaimObject::Entity(globex.id.clone()));
    assert_eq!(
        env.g().get_claim(mine).unwrap().unwrap().status,
        ClaimStatus::Superseded
    );
}
