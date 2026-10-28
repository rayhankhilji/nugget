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
    // M2 wave 2 additions.
    limit_zero_is_unlimited(make().as_ref());
    quotes_are_capped(make().as_ref());
    update_claim_patches_fields(make().as_ref());
    rejected_claims_stay_rejected(make().as_ref());
    reactivation_folds_into_the_oldest(make().as_ref());
    merge_folds_into_the_oldest_with_the_strongest_status(make().as_ref());
    detail_provenance_follows_items_and_merges(make().as_ref());
    split_moves_claims_identifiers_and_mentions(make().as_ref());
    split_failures_change_nothing(make().as_ref());
    prune_orphans_removes_only_orphans(make().as_ref());
    batches_apply_in_order_and_atomically(make().as_ref());
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
    let first = g
        .add_claim(&claim(
            &a,
            "works_at",
            ClaimObject::Entity(acme.clone()),
            vec![ev("a.md")],
        ))
        .unwrap();
    let second = g
        .add_claim(&claim(
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
    assert_eq!(claims[0].id, first, "the oldest claim survives a fold");
    assert!(
        g.get_claim(second).unwrap().is_none(),
        "folded claim is gone"
    );
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

fn mention(entity: &EntityId, key: &str) -> Mention {
    Mention {
        entity: entity.clone(),
        item: item(key),
        chunk_id: Some(1),
        surface: "x".into(),
        confidence: 0.9,
    }
}

fn status(g: &dyn GraphStore, id: ClaimId) -> ClaimStatus {
    g.get_claim(id).unwrap().unwrap().status
}

fn limit_zero_is_unlimited(g: &dyn GraphStore) {
    let hub = person(g, "Hub");
    let mut ids = vec![hub.clone()];
    for i in 0..5 {
        let p = person(g, &format!("P{i}"));
        g.add_claim(&claim(
            &hub,
            "knows",
            ClaimObject::Entity(p.clone()),
            vec![ev("x.md")],
        ))
        .unwrap();
        g.add_mention(&mention(&hub, &format!("m{i}.md"))).unwrap();
        ids.push(p);
    }
    let all = g
        .list_entities(&EntityQuery {
            limit: 0,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(all.len(), 6, "list_entities: limit 0 = unlimited");
    assert_eq!(g.mentions_for(&hub, 0).unwrap().len(), 5);
    let v = g.neighbourhood(&hub, 1, 0).unwrap();
    assert_eq!(v.nodes.len(), 6, "neighbourhood: limit 0 = unlimited");
    assert!(!v.truncated);
    let (changed, _) = g.changes_since(0, 0).unwrap();
    let changed: std::collections::HashSet<_> = changed.into_iter().collect();
    assert!(ids.iter().all(|i| changed.contains(i)));
}

fn quotes_are_capped(g: &dyn GraphStore) {
    let sam = person(g, "Sam");
    let long: String = "é".repeat(MAX_QUOTE_CHARS + 50);
    let mut e = ev("a.md");
    e.quote = Some(long.clone());
    let id = g
        .add_claim(&claim(
            &sam,
            "said",
            ClaimObject::Literal("hi".into()),
            vec![e.clone()],
        ))
        .unwrap();
    let q = g.get_claim(id).unwrap().unwrap().evidence[0]
        .quote
        .clone()
        .unwrap();
    assert_eq!(q.chars().count(), MAX_QUOTE_CHARS, "add_claim cuts quotes");
    let mut e2 = ev("b.md");
    e2.quote = Some(format!("b{long}"));
    g.add_evidence(id, &e2).unwrap();
    let c = g.get_claim(id).unwrap().unwrap();
    assert!(
        c.evidence
            .iter()
            .all(|e| e.quote.as_ref().unwrap().chars().count() <= MAX_QUOTE_CHARS)
    );
    // Re-adding the same long quote does not duplicate the (cut) evidence.
    g.add_evidence(id, &e).unwrap();
    assert_eq!(g.get_claim(id).unwrap().unwrap().evidence.len(), 2);
}

fn update_claim_patches_fields(g: &dyn GraphStore) {
    let sam = person(g, "Sam");
    let id = g
        .add_claim(&claim(
            &sam,
            "has_title",
            ClaimObject::Literal("CTO".into()),
            vec![ev("a.md")],
        ))
        .unwrap();
    let (_, before) = g.changes_since(0, 0).unwrap();
    let to = time::OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
    let c = g
        .update_claim(
            id,
            &ClaimPatch {
                confidence: Some(0.3),
                valid_to: Some(Some(to)),
                text: Some("Sam was CTO.".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(c.confidence, 0.3, "update_claim may lower confidence");
    assert_eq!(c.valid_to, Some(to));
    assert_eq!(c.valid_from, None);
    assert_eq!(c.text, "Sam was CTO.");
    assert_eq!(c.status, ClaimStatus::Active, "status unchanged");
    assert_eq!(g.get_claim(id).unwrap().unwrap(), c);
    assert_eq!(g.changes_since(before, 0).unwrap().0, vec![sam.clone()]);
    let c = g
        .update_claim(
            id,
            &ClaimPatch {
                valid_to: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(c.valid_to, None, "Some(None) clears a bound");
    assert_eq!(c.confidence, 0.3);
    for bad in [
        ClaimPatch {
            confidence: Some(1.5),
            ..Default::default()
        },
        ClaimPatch {
            text: Some("  ".into()),
            ..Default::default()
        },
    ] {
        assert!(matches!(
            g.update_claim(id, &bad),
            Err(nugget_core::Error::Invalid(_))
        ));
    }
    assert!(matches!(
        g.update_claim(id + 1000, &ClaimPatch::default()),
        Err(nugget_core::Error::NotFound(_))
    ));
}

fn rejected_claims_stay_rejected(g: &dyn GraphStore) {
    let sam = person(g, "Sam");
    let lit = ClaimObject::Literal("Oslo".into());
    let id = g
        .add_claim(&claim(&sam, "lives_in", lit.clone(), vec![ev("a.md")]))
        .unwrap();
    g.set_claim_status(id, ClaimStatus::Rejected).unwrap();
    assert!(!ClaimStatus::Rejected.is_active());
    assert!(g.claims_for(&sam, false).unwrap().is_empty());
    assert_eq!(g.counts().unwrap().claims, 0);

    // Re-extraction: retract the source, then add the same triple again.
    g.retract_item(&item("a.md")).unwrap();
    assert_eq!(
        status(g, id),
        ClaimStatus::Rejected,
        "losing its evidence does not turn a rejection into a retraction"
    );
    let again = g
        .add_claim(&claim(&sam, "lives_in", lit.clone(), vec![ev("a.md")]))
        .unwrap();
    assert_eq!(again, id, "the triple lands in the rejected claim");
    assert_eq!(status(g, id), ClaimStatus::Rejected);
    assert_eq!(g.get_claim(id).unwrap().unwrap().evidence.len(), 1);
    g.add_evidence(id, &ev("b.md")).unwrap();
    assert_eq!(
        status(g, id),
        ClaimStatus::Rejected,
        "evidence never revives it"
    );
    assert!(g.claims_for(&sam, false).unwrap().is_empty());

    // A rejection needs no evidence.
    let mut no_ev = claim(&sam, "likes", ClaimObject::Literal("jazz".into()), vec![]);
    no_ev.status = ClaimStatus::Rejected;
    let jazz = g.add_claim(&no_ev).unwrap();
    assert_eq!(status(g, jazz), ClaimStatus::Rejected);

    // The user changing their mind: restoring, or asserting the same fact.
    g.set_claim_status(id, ClaimStatus::Active).unwrap();
    assert_eq!(status(g, id), ClaimStatus::Active);
    let mut asserted = claim(&sam, "likes", ClaimObject::Literal("jazz".into()), vec![]);
    asserted.status = ClaimStatus::UserAsserted;
    assert_eq!(g.add_claim(&asserted).unwrap(), jazz);
    assert_eq!(status(g, jazz), ClaimStatus::UserAsserted);
    assert_eq!(
        ClaimStatus::parse("rejected"),
        Some(ClaimStatus::Rejected),
        "wire name"
    );
    assert_eq!(
        serde_json::to_value(ClaimStatus::Rejected).unwrap(),
        serde_json::json!("rejected")
    );
}

fn reactivation_folds_into_the_oldest(g: &dyn GraphStore) {
    let sam = person(g, "Sam");
    let tea = ClaimObject::Literal("tea".into());
    let a = g
        .add_claim(&claim(&sam, "likes", tea.clone(), vec![ev("a.md")]))
        .unwrap();
    g.retract_item(&item("a.md")).unwrap();
    assert_eq!(status(g, a), ClaimStatus::Retracted);
    let b = g
        .add_claim(&claim(&sam, "likes", tea.clone(), vec![ev("b.md")]))
        .unwrap();
    assert_ne!(a, b, "a retracted claim is not live: a new claim starts");
    g.add_evidence(a, &ev("c.md")).unwrap();
    let live: Vec<Claim> = g.claims_for(&sam, false).unwrap();
    assert_eq!(live.len(), 1, "no duplicate live triples");
    assert_eq!(live[0].id, a, "the oldest survives");
    assert_eq!(live[0].evidence.len(), 2);
    assert!(g.get_claim(b).unwrap().is_none());

    // Reactivating a superseded claim folds too.
    let coffee = ClaimObject::Literal("coffee".into());
    let c = g
        .add_claim(&claim(&sam, "likes", coffee.clone(), vec![ev("d.md")]))
        .unwrap();
    g.set_claim_status(c, ClaimStatus::Superseded).unwrap();
    let mut d_new = claim(&sam, "likes", coffee.clone(), vec![ev("e.md")]);
    d_new.confidence = 0.95;
    let d = g.add_claim(&d_new).unwrap();
    assert_ne!(c, d);
    g.set_claim_status(c, ClaimStatus::Active).unwrap();
    let survivor = g.get_claim(c).unwrap().unwrap();
    assert_eq!(survivor.status, ClaimStatus::Active);
    assert_eq!(survivor.evidence.len(), 2);
    assert_eq!(survivor.confidence, 0.95, "the higher confidence is kept");
    assert!(g.get_claim(d).unwrap().is_none());
    assert_eq!(g.claims_for(&sam, false).unwrap().len(), 2);

    // Reactivating next to a rejected twin keeps the rejection.
    let milk = ClaimObject::Literal("milk".into());
    let e = g
        .add_claim(&claim(&sam, "likes", milk.clone(), vec![ev("f.md")]))
        .unwrap();
    g.set_claim_status(e, ClaimStatus::Superseded).unwrap();
    let f = g
        .add_claim(&claim(&sam, "likes", milk.clone(), vec![ev("g.md")]))
        .unwrap();
    g.set_claim_status(f, ClaimStatus::Rejected).unwrap();
    g.set_claim_status(e, ClaimStatus::Active).unwrap();
    assert_eq!(status(g, e), ClaimStatus::Rejected, "rejected beats active");
    assert!(g.get_claim(f).unwrap().is_none());
}

fn merge_folds_into_the_oldest_with_the_strongest_status(g: &dyn GraphStore) {
    let a = person(g, "A");
    let b = person(g, "B");
    let lit = ClaimObject::Literal("chess".into());
    let mut on_b = claim(&b, "plays", lit.clone(), vec![ev("b.md")]);
    on_b.status = ClaimStatus::UserAsserted;
    let older = g.add_claim(&on_b).unwrap();
    let newer = g
        .add_claim(&claim(&a, "plays", lit.clone(), vec![ev("a.md")]))
        .unwrap();
    g.merge(&a, &b).unwrap();
    let c = g.get_claim(older).unwrap().unwrap();
    assert_eq!(c.subject, a);
    assert_eq!(c.status, ClaimStatus::UserAsserted);
    assert_eq!(c.evidence.len(), 2);
    assert!(g.get_claim(newer).unwrap().is_none());
}

fn detail_provenance_follows_items_and_merges(g: &dyn GraphStore) {
    let mut new = NewEntity::new(EntityKind::Person, "Sam Lee");
    new.aliases = vec!["Sammy".into()];
    new.identifiers = vec![Identifier::new("email", "sam@a.test")];
    let sam = g.create_entity(&new).unwrap();
    let alias = EntityDetail::Alias("SAMMY".into());
    let email = EntityDetail::Identifier(Identifier::new("email", "Sam@A.test"));
    g.add_detail_source(&sam, &alias, &item("a.md")).unwrap();
    g.add_detail_source(&sam, &email, &item("b.md")).unwrap();
    g.add_detail_source(&sam, &email, &item("a.md")).unwrap();
    g.add_detail_source(&sam, &email, &item("a.md")).unwrap();
    let p = g.entity_provenance(&sam).unwrap().unwrap();
    assert_eq!(p.aliases, vec![("Sammy".to_string(), vec![item("a.md")])]);
    assert_eq!(p.identifiers.len(), 1);
    assert_eq!(p.identifiers[0].0, Identifier::new("email", "sam@a.test"));
    let mut items = p.identifiers[0].1.clone();
    items.sort_by(|x, y| x.key.as_str().cmp(y.key.as_str()));
    assert_eq!(items, vec![item("a.md"), item("b.md")], "deduplicated");

    // An item's retraction drops what it supplied (the detail itself stays).
    g.retract_item(&item("a.md")).unwrap();
    let p = g.entity_provenance(&sam).unwrap().unwrap();
    assert_eq!(p.aliases, vec![("Sammy".to_string(), vec![])]);
    assert_eq!(p.identifiers[0].1, vec![item("b.md")]);

    // Merges carry provenance to the survivor.
    let mut other = NewEntity::new(EntityKind::Person, "S. Lee");
    other.identifiers = vec![Identifier::new("phone", "+1 555 0100")];
    let other = g.create_entity(&other).unwrap();
    g.add_detail_source(
        &other,
        &EntityDetail::Identifier(Identifier::new("phone", "+15550100")),
        &item("c.md"),
    )
    .unwrap();
    g.merge(&sam, &other).unwrap();
    let p = g.entity_provenance(&other).unwrap().unwrap();
    let phone = p
        .identifiers
        .iter()
        .find(|(i, _)| i.scheme == "phone")
        .unwrap();
    assert_eq!(phone.1, vec![item("c.md")]);
    assert!(
        p.aliases
            .iter()
            .any(|(a, items)| a == "S. Lee" && items.is_empty()),
        "a merged name becomes an alias with no recorded item"
    );
    assert!(
        g.entity_provenance(&EntityId::new("nope"))
            .unwrap()
            .is_none()
    );
}

fn split_moves_claims_identifiers_and_mentions(g: &dyn GraphStore) {
    let mut new = NewEntity::new(EntityKind::Person, "Sam Lee");
    new.aliases = vec!["Sammy".into(), "Sam".into()];
    new.identifiers = vec![
        Identifier::new("email", "sam@a.test"),
        Identifier::new("email", "sam@b.test"),
    ];
    let sam = g.create_entity(&new).unwrap();
    g.add_detail_source(
        &sam,
        &EntityDetail::Identifier(Identifier::new("email", "sam@b.test")),
        &item("b.md"),
    )
    .unwrap();
    let acme = g
        .create_entity(&NewEntity::new(EntityKind::Organization, "Acme"))
        .unwrap();
    let works = g
        .add_claim(&claim(
            &sam,
            "works_at",
            ClaimObject::Entity(acme.clone()),
            vec![ev("b.md")],
        ))
        .unwrap();
    let lives = g
        .add_claim(&claim(
            &sam,
            "lives_in",
            ClaimObject::Literal("Oslo".into()),
            vec![ev("a.md")],
        ))
        .unwrap();
    let knows = g
        .add_claim(&claim(
            &acme,
            "employs",
            ClaimObject::Entity(sam.clone()),
            vec![ev("b.md"), ev("a.md")],
        ))
        .unwrap();
    g.add_mention(&mention(&sam, "a.md")).unwrap();
    g.add_mention(&mention(&sam, "b.md")).unwrap();
    let (_, before) = g.changes_since(0, 0).unwrap();

    let mut spec = NewEntity::new(EntityKind::Person, "Samuel Lee");
    spec.aliases = vec!["Sammy".into()];
    spec.identifiers = vec![Identifier::new("email", "SAM@b.test")];
    let samuel = g.split(&sam, &[works, knows, works], &spec).unwrap();
    assert_ne!(samuel, sam);

    let old = g.get_entity(&sam).unwrap().unwrap();
    let new = g.get_entity(&samuel).unwrap().unwrap();
    assert_eq!(new.name, "Samuel Lee");
    assert_eq!(new.aliases, vec!["Sammy".to_string()]);
    assert_eq!(old.aliases, vec!["Sam".to_string()]);
    assert_eq!(
        new.identifiers,
        vec![Identifier::new("email", "sam@b.test")]
    );
    assert_eq!(
        old.identifiers,
        vec![Identifier::new("email", "sam@a.test")]
    );
    assert_eq!(
        g.find_by_identifier(&Identifier::new("email", "sam@b.test"))
            .unwrap(),
        Some(samuel.clone())
    );
    let p = g.entity_provenance(&samuel).unwrap().unwrap();
    assert_eq!(p.identifiers[0].1, vec![item("b.md")], "provenance moves");

    assert_eq!(g.get_claim(works).unwrap().unwrap().subject, samuel);
    assert_eq!(
        g.get_claim(knows).unwrap().unwrap().object,
        ClaimObject::Entity(samuel.clone())
    );
    assert_eq!(g.get_claim(lives).unwrap().unwrap().subject, sam);
    // b.md only supports moved claims; a.md also supports `lives_in`, which stays.
    let old_m: Vec<String> = g
        .mentions_for(&sam, 0)
        .unwrap()
        .into_iter()
        .map(|m| m.item.key.as_str().to_owned())
        .collect();
    let new_m: Vec<String> = g
        .mentions_for(&samuel, 0)
        .unwrap()
        .into_iter()
        .map(|m| m.item.key.as_str().to_owned())
        .collect();
    assert_eq!(old_m, vec!["a.md".to_string()]);
    assert_eq!(new_m, vec!["b.md".to_string()]);
    let mut changed = g.changes_since(before, 0).unwrap().0;
    changed.sort();
    let mut want = vec![sam, samuel, acme];
    want.sort();
    assert_eq!(changed, want);
}

fn split_failures_change_nothing(g: &dyn GraphStore) {
    let sam = person(g, "Sam");
    let kim = person(g, "Kim");
    g.add_identifier(&kim, &Identifier::new("email", "kim@x.test"))
        .unwrap();
    let on_kim = g
        .add_claim(&claim(
            &kim,
            "likes",
            ClaimObject::Literal("tea".into()),
            vec![ev("a.md")],
        ))
        .unwrap();
    let before = g.counts().unwrap();
    assert!(matches!(
        g.split(&sam, &[on_kim], &NewEntity::new(EntityKind::Person, "New")),
        Err(nugget_core::Error::Invalid(_))
    ));
    let mut stolen = NewEntity::new(EntityKind::Person, "New");
    stolen.identifiers = vec![Identifier::new("email", "kim@x.test")];
    assert!(matches!(
        g.split(&sam, &[], &stolen),
        Err(nugget_core::Error::Invalid(_))
    ));
    assert!(g.split(&sam, &[999_999], &stolen).is_err());
    assert_eq!(g.counts().unwrap(), before, "nothing created");
    assert!(g.find_by_name("New", None).unwrap().is_empty());
}

fn prune_orphans_removes_only_orphans(g: &dyn GraphStore) {
    let mentioned = person(g, "Mentioned Only");
    g.add_identifier(&mentioned, &Identifier::new("email", "m@x.test"))
        .unwrap();
    g.add_mention(&mention(&mentioned, "x.md")).unwrap();
    let claimed = person(g, "Claimed Only");
    let dead = g
        .add_claim(&claim(
            &claimed,
            "likes",
            ClaimObject::Literal("tea".into()),
            vec![ev("x.md")],
        ))
        .unwrap();
    let mut me = NewEntity::new(EntityKind::Person, "Me");
    me.attributes = serde_json::json!({"is_me": true});
    let me = g.create_entity(&me).unwrap();
    let mut pinned = NewEntity::new(EntityKind::Topic, "Pinned");
    pinned.attributes = serde_json::json!({"user_asserted": true});
    let pinned = g.create_entity(&pinned).unwrap();
    let rejecting = person(g, "Rejecting");
    let mut r = claim(
        &rejecting,
        "likes",
        ClaimObject::Literal("x".into()),
        vec![],
    );
    r.status = ClaimStatus::Rejected;
    g.add_claim(&r).unwrap();
    let historic = person(g, "Historic");
    let h = g
        .add_claim(&claim(
            &historic,
            "lives_in",
            ClaimObject::Literal("Rome".into()),
            vec![ev("y.md")],
        ))
        .unwrap();
    g.set_claim_status(h, ClaimStatus::Superseded).unwrap();
    let linked = person(g, "Linked");
    g.add_claim(&claim(
        &linked,
        "knows",
        ClaimObject::Entity(historic.clone()),
        vec![ev("y.md")],
    ))
    .unwrap();

    assert!(
        g.prune_orphans(None).unwrap().is_empty(),
        "nothing is orphaned yet"
    );
    g.retract_item(&item("x.md")).unwrap();
    let (_, before) = g.changes_since(0, 0).unwrap();
    assert!(
        g.prune_orphans(Some(std::slice::from_ref(&historic)))
            .unwrap()
            .is_empty()
    );
    let mut pruned = g.prune_orphans(None).unwrap();
    pruned.sort();
    let mut want = vec![mentioned.clone(), claimed.clone()];
    want.sort();
    assert_eq!(pruned, want);
    for id in &want {
        assert!(
            g.get_entity(id).unwrap().is_none(),
            "pruned ids resolve to nothing"
        );
    }
    assert!(
        g.get_claim(dead).unwrap().is_none(),
        "its retracted claims go"
    );
    assert!(g.find_by_name("Mentioned Only", None).unwrap().is_empty());
    assert!(
        g.list_entities(&EntityQuery::default())
            .unwrap()
            .iter()
            .all(|s| !want.contains(&s.id))
    );
    for keep in [&me, &pinned, &rejecting, &historic, &linked] {
        assert!(g.get_entity(keep).unwrap().is_some());
    }
    let changed = g.changes_since(before, 0).unwrap().0;
    assert!(
        want.iter().all(|id| changed.contains(id)),
        "pruning is a change"
    );
    assert!(g.prune_orphans(None).unwrap().is_empty());
    // The identifier is free again, and the old id is not reused.
    let again = g
        .create_entity(&{
            let mut n = NewEntity::new(EntityKind::Person, "Mentioned Again");
            n.identifiers = vec![Identifier::new("email", "m@x.test")];
            n
        })
        .unwrap();
    assert!(!want.contains(&again));
    assert_eq!(g.counts().unwrap().entities, 6);
}

fn batches_apply_in_order_and_atomically(g: &dyn GraphStore) {
    let sam = person(g, "Sam");
    let acme = g
        .create_entity(&NewEntity::new(EntityKind::Organization, "Acme"))
        .unwrap();
    let mut long = ev("a.md");
    long.quote = Some("q".repeat(MAX_QUOTE_CHARS * 2));
    let mut b = GraphBatch::new();
    b.push(GraphOp::CreateEntity {
        entity: NewEntity::new(EntityKind::Topic, "Boilers"),
    });
    b.push(GraphOp::AddClaim {
        claim: claim(
            &sam,
            "works_at",
            ClaimObject::Entity(acme.clone()),
            vec![long],
        ),
    });
    b.push(GraphOp::AddClaim {
        claim: claim(
            &sam,
            "works_at",
            ClaimObject::Entity(acme.clone()),
            vec![ev("b.md")],
        ),
    });
    b.push(GraphOp::AddMention {
        mention: mention(&sam, "a.md"),
    });
    b.push(GraphOp::AddAlias {
        entity: sam.clone(),
        alias: "Sammy".into(),
    });
    b.push(GraphOp::AddDetailSource {
        entity: sam.clone(),
        detail: EntityDetail::Alias("Sammy".into()),
        item: item("a.md"),
    });
    let out = g.apply_batch(&b).unwrap();
    assert_eq!(out.len(), b.len());
    let topic = out[0].entity().expect("entity result").clone();
    assert_eq!(g.get_entity(&topic).unwrap().unwrap().name, "Boilers");
    let c = out[1].claim().unwrap();
    assert_eq!(out[2], BatchResult::Claim(c), "later ops see earlier ones");
    assert_eq!(out[3], BatchResult::Done);
    let stored = g.get_claim(c).unwrap().unwrap();
    assert_eq!(stored.evidence.len(), 2);
    assert!(
        stored.evidence[0].quote.as_ref().unwrap().chars().count() <= MAX_QUOTE_CHARS,
        "batches cut quotes too"
    );
    assert_eq!(g.mentions_for(&sam, 0).unwrap().len(), 1);
    assert_eq!(
        g.entity_provenance(&sam).unwrap().unwrap().aliases,
        vec![("Sammy".to_string(), vec![item("a.md")])]
    );

    let mut b = GraphBatch::new();
    b.push(GraphOp::UpdateClaim {
        claim: c,
        patch: ClaimPatch {
            confidence: Some(0.1),
            ..Default::default()
        },
    });
    b.push(GraphOp::SetClaimStatus {
        claim: c,
        status: ClaimStatus::Superseded,
    });
    b.push(GraphOp::RetractItem { item: item("a.md") });
    let out = g.apply_batch(&b).unwrap();
    assert_eq!(out[2], BatchResult::Claims(vec![c]));
    let stored = g.get_claim(c).unwrap().unwrap();
    assert_eq!(
        (stored.confidence, stored.status, stored.evidence.len()),
        (0.1, ClaimStatus::Superseded, 1)
    );
    assert!(g.mentions_for(&sam, 0).unwrap().is_empty());

    // A failing op undoes the whole batch.
    let (_, seq) = g.changes_since(0, 0).unwrap();
    let before = g.counts().unwrap();
    let mut b = GraphBatch::new();
    b.push(GraphOp::AddAlias {
        entity: sam.clone(),
        alias: "Doomed".into(),
    });
    b.push(GraphOp::CreateEntity {
        entity: NewEntity::new(EntityKind::Topic, "Doomed Topic"),
    });
    b.push(GraphOp::AddEvidence {
        claim: c,
        evidence: ev("c.md"),
    });
    b.push(GraphOp::AddClaim {
        claim: claim(
            &EntityId::new("ent_missing"),
            "likes",
            ClaimObject::Literal("x".into()),
            vec![ev("c.md")],
        ),
    });
    assert!(matches!(
        g.apply_batch(&b),
        Err(nugget_core::Error::NotFound(_))
    ));
    assert!(
        !g.get_entity(&sam)
            .unwrap()
            .unwrap()
            .aliases
            .contains(&"Doomed".to_string())
    );
    assert!(g.find_by_name("Doomed Topic", None).unwrap().is_empty());
    assert_eq!(g.get_claim(c).unwrap().unwrap().evidence.len(), 1);
    assert_eq!(g.counts().unwrap(), before);
    assert!(
        g.changes_since(seq, 0).unwrap().0.is_empty(),
        "a failed batch leaves no change-feed entries"
    );
    assert!(g.apply_batch(&GraphBatch::new()).unwrap().is_empty());
}
