//! Behaviour every [`GraphStore`] must show. Implementations run [`run`] in their tests:
//!
//! ```ignore
//! nugget_graph::conformance::run(&|| Box::new(MyGraph::open_in_memory().unwrap()));
//! ```

use nugget_core::ids::{EntityId, ItemKey, ItemRef, SourceId};

use crate::GraphStore;
use crate::types::*;

/// Run every check against fresh stores from `make`. Panics on the first failure.
pub fn run(make: &dyn Fn() -> Box<dyn GraphStore>) {
    entities_and_lookups(make().as_ref());
    identifiers_are_unique(make().as_ref());
    claims_dedupe_and_need_evidence(make().as_ref());
    retracting_an_item_retracts_unsupported_claims(make().as_ref());
    merge_moves_everything_and_old_ids_resolve(make().as_ref());
    neighbourhood_and_limits(make().as_ref());
    change_feed_advances(make().as_ref());
}

fn item(key: &str) -> ItemRef {
    ItemRef {
        source: SourceId::new("files-1"),
        key: ItemKey::new(key),
    }
}

fn ev(key: &str) -> Evidence {
    Evidence {
        item: item(key),
        chunk_id: Some(1),
        quote: Some("quote".into()),
    }
}

fn person(g: &dyn GraphStore, name: &str) -> EntityId {
    g.create_entity(&NewEntity::new(EntityKind::Person, name))
        .unwrap()
}

fn claim(subject: &EntityId, pred: &str, object: ClaimObject, evidence: Vec<Evidence>) -> NewClaim {
    NewClaim {
        subject: subject.clone(),
        predicate: pred.into(),
        object,
        text: format!("{subject} {pred}"),
        confidence: 0.8,
        status: ClaimStatus::Active,
        evidence,
        valid_from: None,
        valid_to: None,
    }
}

fn entities_and_lookups(g: &dyn GraphStore) {
    let mut new = NewEntity::new(EntityKind::Person, "Samantha Lee");
    new.aliases = vec!["Sam".into(), "samantha lee".into()];
    new.identifiers = vec![Identifier::new("email", " Sam@Example.COM ")];
    let id = g.create_entity(&new).unwrap();
    let e = g.get_entity(&id).unwrap().unwrap();
    assert_eq!(e.name, "Samantha Lee");
    assert_eq!(
        e.aliases,
        vec!["Sam".to_string()],
        "alias equal to the name is dropped"
    );
    assert_eq!(
        g.find_by_identifier(&Identifier::new("EMAIL", "sam@example.com"))
            .unwrap(),
        Some(id.clone())
    );
    assert_eq!(g.find_by_name("  sam ", None).unwrap(), vec![id.clone()]);
    assert!(
        g.find_by_name("Sam", Some(EntityKind::Organization))
            .unwrap()
            .is_empty()
    );
    let renamed = g
        .update_entity(
            &id,
            &EntityPatch {
                name: Some("Sam Lee".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        renamed.aliases.contains(&"Samantha Lee".to_string()),
        "the old name becomes an alias"
    );
    assert!(
        g.create_entity(&NewEntity::new(EntityKind::Topic, "  "))
            .is_err()
    );
    let listed = g
        .list_entities(&EntityQuery {
            text: Some("lee".into()),
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert!(g.get_entity(&EntityId::new("nope")).unwrap().is_none());
}

fn identifiers_are_unique(g: &dyn GraphStore) {
    let a = person(g, "A");
    let b = person(g, "B");
    g.add_identifier(&a, &Identifier::new("phone", "+1 (555) 010-2000"))
        .unwrap();
    assert!(
        g.add_identifier(&b, &Identifier::new("phone", "+15550102000"))
            .is_err(),
        "a normalised identifier belongs to one entity"
    );
}

fn claims_dedupe_and_need_evidence(g: &dyn GraphStore) {
    let sam = person(g, "Sam");
    let acme = g
        .create_entity(&NewEntity::new(EntityKind::Organization, "Acme"))
        .unwrap();
    let c1 = g
        .add_claim(&claim(
            &sam,
            "works_at",
            ClaimObject::Entity(acme.clone()),
            vec![ev("a.md")],
        ))
        .unwrap();
    let c2 = g
        .add_claim(&claim(
            &sam,
            "works_at",
            ClaimObject::Entity(acme.clone()),
            vec![ev("b.md")],
        ))
        .unwrap();
    assert_eq!(c1, c2, "same triple folds into one claim");
    assert_eq!(g.get_claim(c1).unwrap().unwrap().evidence.len(), 2);
    assert!(
        g.add_claim(&claim(
            &sam,
            "likes",
            ClaimObject::Literal("tea".into()),
            vec![]
        ))
        .is_err(),
        "evidence required"
    );
    let mut asserted = claim(&sam, "likes", ClaimObject::Literal("tea".into()), vec![]);
    asserted.status = ClaimStatus::UserAsserted;
    g.add_claim(&asserted).unwrap();
    assert_eq!(
        g.claims_for(&acme, false).unwrap().len(),
        1,
        "object side too"
    );
    assert_eq!(g.claims_for(&sam, false).unwrap().len(), 2);
}

fn retracting_an_item_retracts_unsupported_claims(g: &dyn GraphStore) {
    let sam = person(g, "Sam");
    let one = g
        .add_claim(&claim(
            &sam,
            "lives_in",
            ClaimObject::Literal("Oslo".into()),
            vec![ev("a.md")],
        ))
        .unwrap();
    let two = g
        .add_claim(&claim(
            &sam,
            "speaks",
            ClaimObject::Literal("Norwegian".into()),
            vec![ev("a.md"), ev("b.md")],
        ))
        .unwrap();
    g.add_mention(&Mention {
        entity: sam.clone(),
        item: item("a.md"),
        chunk_id: Some(1),
        surface: "Sam".into(),
        confidence: 0.9,
    })
    .unwrap();
    let mut affected = g.retract_item(&item("a.md")).unwrap();
    affected.sort();
    assert_eq!(affected, vec![one, two]);
    assert_eq!(
        g.get_claim(one).unwrap().unwrap().status,
        ClaimStatus::Retracted
    );
    assert_eq!(
        g.get_claim(two).unwrap().unwrap().status,
        ClaimStatus::Active
    );
    assert!(g.mentions_for(&sam, 10).unwrap().is_empty());
    assert_eq!(g.claims_for(&sam, false).unwrap().len(), 1);
    assert_eq!(g.claims_for(&sam, true).unwrap().len(), 2);
    assert!(g.set_claim_status(one, ClaimStatus::Active).is_err());
    g.add_evidence(one, &ev("c.md")).unwrap();
    assert_eq!(
        g.get_claim(one).unwrap().unwrap().status,
        ClaimStatus::Active
    );
}

fn merge_moves_everything_and_old_ids_resolve(g: &dyn GraphStore) {
    let a = person(g, "Samantha Lee");
    let b = person(g, "S. Lee");
    let acme = g
        .create_entity(&NewEntity::new(EntityKind::Organization, "Acme"))
        .unwrap();
    g.add_identifier(&b, &Identifier::new("email", "sam@acme.test"))
        .unwrap();
    g.add_claim(&claim(
        &a,
        "works_at",
        ClaimObject::Entity(acme.clone()),
        vec![ev("a.md")],
    ))
    .unwrap();
    g.add_claim(&claim(
        &b,
        "works_at",
        ClaimObject::Entity(acme.clone()),
        vec![ev("b.md")],
    ))
    .unwrap();
    g.merge(&a, &b).unwrap();
    let e = g.get_entity(&b).unwrap().unwrap();
    assert_eq!(e.id, a, "old id resolves to the survivor");
    assert!(e.aliases.contains(&"S. Lee".to_string()));
    assert!(e.merged_ids.contains(&b));
    assert_eq!(
        g.find_by_identifier(&Identifier::new("email", "sam@acme.test"))
            .unwrap(),
        Some(a.clone())
    );
    let claims = g.claims_for(&a, false).unwrap();
    assert_eq!(claims.len(), 1, "duplicate triples fold on merge");
    assert_eq!(claims[0].evidence.len(), 2);
    assert!(g.merge(&a, &a).is_err());
    assert!(g.merge(&a, &b).is_err(), "b already resolves to a");
}

fn neighbourhood_and_limits(g: &dyn GraphStore) {
    let a = person(g, "A");
    let b = person(g, "B");
    let c = person(g, "C");
    let d = person(g, "D");
    for (s, o) in [(&a, &b), (&b, &c), (&c, &d)] {
        g.add_claim(&claim(
            s,
            "knows",
            ClaimObject::Entity(o.clone()),
            vec![ev("x.md")],
        ))
        .unwrap();
    }
    let v = g.neighbourhood(&b, 1, 100).unwrap();
    let mut names: Vec<_> = v.nodes.iter().map(|n| n.name.clone()).collect();
    names.sort();
    assert_eq!(names, ["A", "B", "C"]);
    assert_eq!(v.edges.len(), 2);
    assert!(!v.truncated);
    let v = g.neighbourhood(&a, 3, 2).unwrap();
    assert_eq!(v.nodes.len(), 2);
    assert!(v.truncated);
}

fn change_feed_advances(g: &dyn GraphStore) {
    let (_, start) = g.changes_since(0, 0).unwrap();
    let a = person(g, "A");
    let (changed, high) = g.changes_since(start, 100).unwrap();
    assert_eq!(changed, vec![a.clone()]);
    assert!(high > start);
    let (none, same) = g.changes_since(high, 100).unwrap();
    assert!(none.is_empty());
    assert_eq!(same, high);
    g.add_alias(&a, "Alpha").unwrap();
    assert_eq!(g.changes_since(high, 100).unwrap().0, vec![a]);
    let counts = g.counts().unwrap();
    assert_eq!(counts.entities, 1);
}
