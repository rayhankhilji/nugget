//! A user's split is a constraint: re-extracting the sources never merges the two entities back,
//! and the facts and mentions the split moved stay on the split-off entity.

mod common;

use common::*;
use nugget_core::ids::EntityId;
use nugget_graph::{Claim, ClaimObject, EntityKind, Identifier, NewEntity};
use nugget_knowledge::{Knowledge, KnowledgeConfig};
use serde_json::json;

const TO: &[&str] = &["Robin <robin@gmail.com>"];

/// Claims referencing `e` whose evidence all comes from `key`.
fn claims_from(env: &Env, e: &EntityId, key: &str) -> Vec<Claim> {
    env.g()
        .claims_for(e, false)
        .unwrap()
        .into_iter()
        .filter(|c| {
            !c.evidence.is_empty() && c.evidence.iter().all(|ev| ev.item.key.as_str() == key)
        })
        .collect()
}

fn split(env: &Env, k: &Knowledge, from: &EntityId, claims: &[Claim], new: NewEntity) -> EntityId {
    let ids: Vec<_> = claims.iter().map(|c| c.id).collect();
    let to = env.g().split(from, &ids, &new).unwrap();
    k.record_split(from, &to).unwrap();
    to
}

fn live(env: &Env, id: &EntityId) -> EntityId {
    env.g().get_entity(id).unwrap().expect("alive").id
}

#[tokio::test]
async fn a_split_off_namesake_stays_split_with_its_facts() {
    let env = Env::new();
    for (key, title) in [
        ("projects/Fennel.md", "Fennel"),
        ("projects/Bramble.md", "Bramble"),
    ] {
        env.put(
            note(
                "files:notes",
                key,
                title,
                json!({"type": "project", "people": ["Jo Marsh"]}),
                &[],
                &[],
                "Planning.",
            ),
            &["Planning."],
        );
    }
    let k = env.knowledge(KnowledgeConfig::default(), None);
    run(&k).await;
    // Name only: both notes' Jo Marsh are one person.
    let jo = env.entity(EntityKind::Person, "Jo Marsh");
    assert_eq!(env.active(&jo, "works_on").len(), 2);

    // They are two people: the user splits Bramble's Jo off.
    let moved = claims_from(&env, &jo.id, "projects/Bramble.md");
    assert_eq!(moved.len(), 1, "{moved:?}");
    let other = split(
        &env,
        &k,
        &jo.id,
        &moved,
        NewEntity::new(EntityKind::Person, "Jo Marsh"),
    );

    // Re-extract everything (as after a reset or an extractor upgrade).
    k.reset().await.unwrap();
    let stats = run(&k).await;
    assert_eq!(stats.items_failed, 0);
    assert_eq!(stats.items_processed, 2);
    assert_eq!(stats.entities_merged, 0);
    let (a, b) = (live(&env, &jo.id), live(&env, &other));
    assert_eq!(a, jo.id);
    assert_eq!(b, other, "never merged back");
    let fennel = env.entity(EntityKind::Project, "Fennel");
    let bramble = env.entity(EntityKind::Project, "Bramble");
    let a_e = env.g().get_entity(&a).unwrap().unwrap();
    let b_e = env.g().get_entity(&b).unwrap().unwrap();
    let a_on = env.active(&a_e, "works_on");
    let b_on = env.active(&b_e, "works_on");
    assert_eq!(a_on.len(), 1, "{a_on:?}");
    assert_eq!(a_on[0].object, ClaimObject::Entity(fennel.id.clone()));
    assert_eq!(b_on.len(), 1, "{b_on:?}");
    assert_eq!(b_on[0].object, ClaimObject::Entity(bramble.id.clone()));
    assert_eq!(b_on[0].id, moved[0].id, "the moved fact keeps its id");
    // Mentions follow: Bramble's note mentions the split-off Jo only.
    let b_items: Vec<String> = env
        .g()
        .mentions_for(&b, 0)
        .unwrap()
        .into_iter()
        .map(|m| m.item.key.as_str().to_owned())
        .collect();
    assert!(!b_items.is_empty());
    assert!(
        b_items.iter().all(|k| k == "projects/Bramble.md"),
        "{b_items:?}"
    );
    assert!(
        env.g()
            .mentions_for(&a, 0)
            .unwrap()
            .iter()
            .all(|m| m.item.key.as_str() == "projects/Fennel.md")
    );
    assert_eq!(
        env.entities(EntityKind::Person)
            .iter()
            .filter(|e| e.name == "Jo Marsh")
            .count(),
        2
    );

    // An edit of Bramble's note is still about the split-off Jo.
    env.put(
        note(
            "files:notes",
            "projects/Bramble.md",
            "Bramble",
            json!({"type": "project", "people": ["Jo Marsh"]}),
            &[],
            &[],
            "Planning, again.",
        ),
        &["Planning, again."],
    );
    run(&k).await;
    assert_eq!(live(&env, &other), other);
    let b_e = env.g().get_entity(&other).unwrap().unwrap();
    assert_eq!(env.active(&b_e, "works_on")[0].id, moved[0].id);
}

#[tokio::test]
async fn a_trusted_record_does_not_merge_a_split_back() {
    let env = Env::new();
    Mail {
        key: "1",
        to: TO,
        from: "Priya Castellane <priya@oldco.test>",
        date: "2026-09-01T10:00:00Z",
        ..Default::default()
    }
    .put(&env);
    Mail {
        key: "2",
        to: TO,
        from: "Priya Castellane <priya@newco.test>",
        date: "2026-10-05T10:00:00Z",
        body: "Hello from Newco.",
        ..Default::default()
    }
    .put(&env);
    // A note that (wrongly) lists both addresses as one person's.
    env.put(
        note(
            "files:notes",
            "people/Priya Castellane.md",
            "Priya Castellane",
            json!({"type": "person",
                   "email": ["priya@newco.test", "priya@oldco.test"]}),
            &[],
            &[],
            "Met at the conference.",
        ),
        &["Met at the conference."],
    );
    let k = env.knowledge(KnowledgeConfig::default(), None);
    run(&k).await;
    let priya = env.entity(EntityKind::Person, "Priya Castellane");
    assert_eq!(
        priya
            .identifiers
            .iter()
            .filter(|i| i.scheme == "email")
            .count(),
        2
    );

    // Two people: the Newco one is split off with her address and the second mail's facts.
    let moved = claims_from(&env, &priya.id, "2");
    assert!(!moved.is_empty());
    let mut new = NewEntity::new(EntityKind::Person, "Priya Castellane");
    new.identifiers = vec![Identifier::new("email", "priya@newco.test")];
    let other = split(&env, &k, &priya.id, &moved, new);

    k.reset().await.unwrap();
    let stats = run(&k).await;
    assert_eq!(stats.items_failed, 0);
    assert_eq!(stats.entities_merged, 0, "{stats:?}");
    let a = env.g().get_entity(&priya.id).unwrap().unwrap();
    let b = env.g().get_entity(&other).unwrap().unwrap();
    assert_ne!(a.id, b.id, "never merged back");
    assert!(
        b.identifiers
            .contains(&Identifier::new("email", "priya@newco.test"))
    );
    assert!(
        !a.identifiers
            .contains(&Identifier::new("email", "priya@newco.test"))
    );
    let newco = env.entity(EntityKind::Organization, "Newco");
    let to_newco = |e| {
        env.active(e, "works_at")
            .into_iter()
            .filter(|c| c.object == ClaimObject::Entity(newco.id.clone()))
            .count()
    };
    assert_eq!(to_newco(&b), 1);
    assert_eq!(to_newco(&a), 0);
    for c in &moved {
        let now = env.g().get_claim(c.id).unwrap().expect("kept");
        assert!(now.status.is_active(), "{now:?}");
        assert!(
            now.subject == b.id || now.object == ClaimObject::Entity(b.id.clone()),
            "{now:?}"
        );
    }
}

#[tokio::test]
async fn merging_by_hand_lifts_the_constraint() {
    let env = Env::new();
    for (key, title) in [
        ("projects/Fennel.md", "Fennel"),
        ("projects/Bramble.md", "Bramble"),
    ] {
        env.put(
            note(
                "files:notes",
                key,
                title,
                json!({"type": "project", "people": ["Jo Marsh"]}),
                &[],
                &[],
                "Planning.",
            ),
            &["Planning."],
        );
    }
    let k = env.knowledge(KnowledgeConfig::default(), None);
    run(&k).await;
    let jo = env.entity(EntityKind::Person, "Jo Marsh");
    let moved = claims_from(&env, &jo.id, "projects/Bramble.md");
    let other = split(
        &env,
        &k,
        &jo.id,
        &moved,
        NewEntity::new(EntityKind::Person, "Jo Marsh"),
    );
    // The user changes their mind.
    env.g().merge(&jo.id, &other).unwrap();
    k.reset().await.unwrap();
    let stats = run(&k).await;
    assert_eq!(stats.items_failed, 0);
    let jo = env.entity(EntityKind::Person, "Jo Marsh");
    assert_eq!(env.active(&jo, "works_on").len(), 2);
}
