//! Knowledge-graph storage (docs/m2-spec.md, M2-A): the shared `GraphStore` conformance suite
//! plus SQLite-specific behaviour (persistence, encryption, delete paths, merge edge cases).

use std::path::PathBuf;

use nugget_core::ids::{EntityId, ItemKey, ItemRef, SourceId};
use nugget_graph::{
    ClaimObject, ClaimStatus, EntityKind, EntityPatch, EntityQuery, Evidence, GraphStore,
    Identifier, Mention, NewClaim, NewEntity,
};
use nugget_store::{
    ENTITY_ID_PREFIX, Error, NewChunk, NewItem, SqliteGraph, Store, StoreKey, content_hash,
};
use serde_json::json;

fn mem() -> SqliteGraph {
    SqliteGraph::open_in_memory().unwrap()
}

fn iref(source: &str, key: &str) -> ItemRef {
    ItemRef {
        source: SourceId::new(source),
        key: ItemKey::new(key),
    }
}

fn ev(source: &str, key: &str) -> Evidence {
    Evidence {
        item: iref(source, key),
        chunk_id: Some(1),
        quote: Some("quote".into()),
    }
}

fn entity(g: &dyn GraphStore, kind: EntityKind, name: &str) -> EntityId {
    g.create_entity(&NewEntity::new(kind, name)).unwrap()
}

fn claim(s: &EntityId, pred: &str, object: ClaimObject, evidence: Vec<Evidence>) -> NewClaim {
    NewClaim {
        subject: s.clone(),
        predicate: pred.into(),
        object,
        text: format!("{s} {pred}"),
        confidence: 0.5,
        status: ClaimStatus::Active,
        evidence,
        valid_from: None,
        valid_to: None,
    }
}

fn mention(e: &EntityId, source: &str, key: &str, surface: &str) -> Mention {
    Mention {
        entity: e.clone(),
        item: iref(source, key),
        chunk_id: Some(1),
        surface: surface.into(),
        confidence: 0.7,
    }
}

fn db_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("data").join("nugget.db")
}

#[test]
fn conformance_in_memory() {
    nugget_graph::conformance::run(&|| Box::new(mem()));
}

#[test]
fn conformance_encrypted_file() {
    let dir = tempfile::tempdir().unwrap();
    let n = std::sync::atomic::AtomicUsize::new(0);
    nugget_graph::conformance::run(&|| {
        let i = n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = dir.path().join(format!("g{i}.db"));
        Box::new(
            Store::open(path, &StoreKey::from_bytes([3u8; 32]))
                .unwrap()
                .graph(),
        )
    });
}

#[test]
fn reference_implementation_still_passes() {
    // Guards against the suite drifting from the reference while this crate tracks it.
    nugget_graph::conformance::run(&|| Box::new(nugget_graph::MemGraph::new()));
}

#[test]
fn entity_ids_are_random_and_stable_across_rename_and_merge() {
    let g = mem();
    let a = entity(&g, EntityKind::Person, "Ada");
    let b = entity(&g, EntityKind::Person, "Ada L.");
    assert!(a.as_str().starts_with(ENTITY_ID_PREFIX));
    assert_ne!(a, b);
    g.update_entity(
        &a,
        &EntityPatch {
            name: Some("Ada Lovelace".into()),
            kind: None,
            attributes: Some(json!({"born": 1815})),
        },
    )
    .unwrap();
    g.merge(&a, &b).unwrap();
    let e = g.get_entity(&a).unwrap().unwrap();
    assert_eq!(e.id, a);
    assert_eq!(e.name, "Ada Lovelace");
    assert_eq!(e.attributes, json!({"born": 1815}));
    assert_eq!(e.aliases, vec!["Ada".to_string(), "Ada L.".to_string()]);
    assert_eq!(e.merged_ids, vec![b.clone()]);
    // The tombstone resolves, but is not listed and not found by name as itself.
    assert_eq!(g.get_entity(&b).unwrap().unwrap().id, a);
    assert_eq!(g.find_by_name("ada l.", None).unwrap(), vec![a.clone()]);
    assert_eq!(g.counts().unwrap().entities, 1);
    let listed = g.list_entities(&EntityQuery::default()).unwrap();
    assert_eq!(listed.len(), 1);
}

#[test]
fn merge_chains_flatten_and_resolve() {
    let g = mem();
    let a = entity(&g, EntityKind::Person, "A");
    let b = entity(&g, EntityKind::Person, "B");
    let c = entity(&g, EntityKind::Person, "C");
    g.merge(&b, &c).unwrap(); // c → b
    g.merge(&a, &b).unwrap(); // b → a, and c re-pointed to a
    for id in [&a, &b, &c] {
        assert_eq!(g.get_entity(id).unwrap().unwrap().id, a);
    }
    let e = g.get_entity(&a).unwrap().unwrap();
    let mut merged = e.merged_ids.clone();
    merged.sort();
    let mut want = vec![b.clone(), c.clone()];
    want.sort();
    assert_eq!(merged, want);
    // Every operation that takes an id follows tombstones.
    g.add_alias(&c, "Alpha").unwrap();
    g.add_claim(&claim(
        &c,
        "is",
        ClaimObject::Literal("x".into()),
        vec![ev("s", "k")],
    ))
    .unwrap();
    assert_eq!(g.claims_for(&b, false).unwrap()[0].subject, a);
    assert!(g.merge(&b, &c).is_err(), "both resolve to a");
    assert!(
        g.get_entity(&a)
            .unwrap()
            .unwrap()
            .aliases
            .contains(&"Alpha".into())
    );
}

#[test]
fn merge_moves_mentions_identifiers_and_object_claims() {
    let g = mem();
    let a = entity(&g, EntityKind::Organization, "Acme");
    let b = entity(&g, EntityKind::Organization, "ACME Inc");
    let sam = entity(&g, EntityKind::Person, "Sam");
    g.add_identifier(&b, &Identifier::new("domain", "Acme.test"))
        .unwrap();
    let c1 = g
        .add_claim(&claim(
            &sam,
            "works_at",
            ClaimObject::Entity(b.clone()),
            vec![ev("s", "1")],
        ))
        .unwrap();
    let c2 = g
        .add_claim(&claim(
            &sam,
            "works_at",
            ClaimObject::Entity(a.clone()),
            vec![ev("s", "2")],
        ))
        .unwrap();
    g.add_mention(&mention(&a, "s", "1", "Acme")).unwrap();
    g.add_mention(&mention(&b, "s", "1", "Acme")).unwrap();
    g.add_mention(&mention(&b, "s", "2", "ACME")).unwrap();
    let (_, before) = g.changes_since(0, 0).unwrap();

    g.merge(&a, &b).unwrap();

    let claims = g.claims_for(&a, false).unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].id, c1.min(c2), "the oldest claim survives");
    assert_eq!(claims[0].evidence.len(), 2);
    assert!(g.get_claim(c1.max(c2)).unwrap().is_none());
    assert_eq!(
        g.find_by_identifier(&Identifier::new("domain", "acme.test"))
            .unwrap(),
        Some(a.clone())
    );
    // The duplicate mention (same item, chunk and surface) folded.
    assert_eq!(g.mentions_for(&a, 0).unwrap().len(), 2);
    assert_eq!(g.mentions_for(&b, 0).unwrap().len(), 2, "old id resolves");
    // Survivor, tombstone and the claim's other end all enter the change feed.
    let (changed, _) = g.changes_since(before, 0).unwrap();
    for id in [&a, &b, &sam] {
        assert!(changed.contains(id), "{id} changed");
    }
    let s = g.list_entities(&EntityQuery::default()).unwrap();
    let acme = s.iter().find(|e| e.id == a).unwrap();
    assert_eq!((acme.claims, acme.mentions), (1, 2));
}

#[test]
fn merge_is_atomic() {
    let g = mem();
    let a = entity(&g, EntityKind::Person, "A");
    assert!(
        g.merge(&a, &EntityId::new("ent_missing")).is_err(),
        "unknown id"
    );
    assert!(g.get_entity(&a).unwrap().is_some());
    assert_eq!(g.counts().unwrap().entities, 1);
}

#[test]
fn list_entities_filters_orders_and_pages() {
    let g = mem();
    let names = ["bravo", "Alpha", "Charlie", "Éclair", "al"];
    let ids: Vec<EntityId> = names
        .iter()
        .map(|n| entity(&g, EntityKind::Topic, n))
        .collect();
    entity(&g, EntityKind::Person, "Alpine Person");
    g.add_alias(&ids[2], "Charles the Alchemist").unwrap();

    let all = g
        .list_entities(&EntityQuery {
            kind: Some(EntityKind::Topic),
            ..Default::default()
        })
        .unwrap();
    let got: Vec<&str> = all.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(got, ["al", "Alpha", "bravo", "Charlie", "Éclair"]);

    let q = |text: &str, kind: Option<EntityKind>| -> Vec<String> {
        g.list_entities(&EntityQuery {
            text: Some(text.into()),
            kind,
            ..Default::default()
        })
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect()
    };
    // Trigram path (≥ 3 chars), alias hits, case-insensitive including non-ASCII.
    assert_eq!(
        q("ALP", None),
        ["Alpha", "Alpine Person"].map(String::from).to_vec()
    );
    assert_eq!(q("alch", None), ["Charlie"].map(String::from).to_vec());
    assert_eq!(q("éCLA", None), ["Éclair"].map(String::from).to_vec());
    // Short needles fall back to a scan.
    assert_eq!(
        q("al", Some(EntityKind::Topic)),
        ["al", "Alpha", "Charlie"].map(String::from).to_vec()
    );
    assert!(q("zzz", None).is_empty());
    assert!(q("\"", None).is_empty(), "quotes are escaped");

    let page = g
        .list_entities(&EntityQuery {
            kind: Some(EntityKind::Topic),
            limit: 2,
            offset: 2,
            ..Default::default()
        })
        .unwrap();
    let got: Vec<&str> = page.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(got, ["bravo", "Charlie"]);
}

#[test]
fn renames_keep_name_search_in_step() {
    let g = mem();
    let a = entity(&g, EntityKind::Project, "Project Falcon");
    g.update_entity(
        &a,
        &EntityPatch {
            name: Some("Osprey".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(g.find_by_name("osprey", None).unwrap(), vec![a.clone()]);
    assert_eq!(
        g.find_by_name("project falcon", None).unwrap(),
        vec![a.clone()]
    );
    let hits = g
        .list_entities(&EntityQuery {
            text: Some("osp".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(hits.len(), 1);
    let e = g
        .update_entity(
            &a,
            &EntityPatch {
                kind: Some(EntityKind::Topic),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(e.kind, EntityKind::Topic);
    assert!(
        g.find_by_name("osprey", Some(EntityKind::Project))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn claims_round_trip_every_field() {
    let g = mem();
    let a = entity(&g, EntityKind::Commitment, "Send the report");
    let t = time::OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
    let mut new = claim(
        &a,
        "due_on",
        ClaimObject::Literal("2027-01-15".into()),
        vec![
            Evidence {
                item: iref("mail", "m1"),
                chunk_id: None,
                quote: None,
            },
            Evidence {
                item: iref("mail", "m1"),
                chunk_id: None,
                quote: Some("by Jan 15".into()),
            },
            ev("mail", "m1"),
            ev("mail", "m1"),
        ],
    );
    new.valid_from = Some(t);
    new.confidence = 0.9;
    let id = g.add_claim(&new).unwrap();
    let c = g.get_claim(id).unwrap().unwrap();
    assert_eq!(c.predicate, "due_on");
    assert_eq!(c.object, ClaimObject::Literal("2027-01-15".into()));
    assert_eq!(c.confidence, 0.9);
    assert_eq!(c.valid_from, Some(t));
    assert_eq!(c.valid_to, None);
    assert_eq!(
        c.evidence.len(),
        3,
        "exact duplicates collapse; NULLs are distinct"
    );
    assert!(g.get_claim(id + 1000).unwrap().is_none());

    // Dedupe raises confidence and can promote to user_asserted.
    let mut again = claim(
        &a,
        "due_on",
        ClaimObject::Literal("2027-01-15".into()),
        vec![],
    );
    again.status = ClaimStatus::UserAsserted;
    again.confidence = 1.0;
    assert_eq!(g.add_claim(&again).unwrap(), id);
    let c = g.get_claim(id).unwrap().unwrap();
    assert_eq!(c.status, ClaimStatus::UserAsserted);
    assert_eq!(c.confidence, 1.0);

    // Superseded claims are not deduped against.
    g.set_claim_status(id, ClaimStatus::Superseded).unwrap();
    let fresh = g
        .add_claim(&claim(
            &a,
            "due_on",
            ClaimObject::Literal("2027-01-15".into()),
            vec![ev("mail", "m2")],
        ))
        .unwrap();
    assert_ne!(fresh, id);
    assert!(g.set_claim_status(999_999, ClaimStatus::Retracted).is_err());
    assert!(g.add_evidence(999_999, &ev("x", "y")).is_err());
    assert!(
        g.add_claim(&claim(
            &EntityId::new("ent_nope"),
            "p",
            ClaimObject::Literal("x".into()),
            vec![ev("x", "y")]
        ))
        .is_err()
    );
}

#[test]
fn user_asserted_claims_survive_retraction() {
    let g = mem();
    let a = entity(&g, EntityKind::Person, "A");
    let mut c = claim(
        &a,
        "likes",
        ClaimObject::Literal("tea".into()),
        vec![ev("s", "1")],
    );
    c.status = ClaimStatus::UserAsserted;
    let id = g.add_claim(&c).unwrap();
    assert_eq!(g.retract_item(&iref("s", "1")).unwrap(), vec![id]);
    let c = g.get_claim(id).unwrap().unwrap();
    assert_eq!(c.status, ClaimStatus::UserAsserted);
    assert!(c.evidence.is_empty());
    assert!(g.retract_item(&iref("s", "1")).unwrap().is_empty());
}

fn stored_item(s: &Store, source: &str, key: &str) -> ItemRef {
    let it = NewItem::new(source, key, content_hash(key)).with_text(key);
    s.upsert_item_with_chunks(&it, &[NewChunk::text(key)])
        .unwrap();
    it.item
}

#[test]
fn deleting_items_and_sources_retracts_their_provenance() {
    let s = Store::open_in_memory().unwrap();
    let g = s.graph();
    let a1 = stored_item(&s, "mail", "1");
    let a2 = stored_item(&s, "mail", "2");
    let f1 = stored_item(&s, "files", "1");
    let sam = entity(&g, EntityKind::Person, "Sam");
    let only_mail1 = g
        .add_claim(&claim(
            &sam,
            "a",
            ClaimObject::Literal("1".into()),
            vec![ev("mail", "1")],
        ))
        .unwrap();
    let mail_and_files = g
        .add_claim(&claim(
            &sam,
            "b",
            ClaimObject::Literal("2".into()),
            vec![ev("mail", "2"), ev("files", "1")],
        ))
        .unwrap();
    let only_mail2 = g
        .add_claim(&claim(
            &sam,
            "c",
            ClaimObject::Literal("3".into()),
            vec![ev("mail", "2")],
        ))
        .unwrap();
    for (src, key) in [("mail", "1"), ("mail", "2"), ("files", "1")] {
        g.add_mention(&mention(&sam, src, key, "Sam")).unwrap();
    }
    let (_, mark) = g.changes_since(0, 0).unwrap();

    assert!(s.delete_item(&a1).unwrap());
    assert_eq!(
        g.get_claim(only_mail1).unwrap().unwrap().status,
        ClaimStatus::Retracted
    );
    assert_eq!(g.mentions_for(&sam, 0).unwrap().len(), 2);
    let (changed, mark) = g.changes_since(mark, 0).unwrap();
    assert_eq!(changed, vec![sam.clone()], "delete enters the change feed");

    assert_eq!(s.delete_source(&SourceId::new("mail")).unwrap(), 1);
    assert_eq!(
        g.get_claim(only_mail2).unwrap().unwrap().status,
        ClaimStatus::Retracted
    );
    let kept = g.get_claim(mail_and_files).unwrap().unwrap();
    assert_eq!(kept.status, ClaimStatus::Active);
    assert_eq!(kept.evidence, vec![ev("files", "1")]);
    let m = g.mentions_for(&sam, 0).unwrap();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].item, f1);
    assert_eq!(g.changes_since(mark, 0).unwrap().0, vec![sam.clone()]);
    assert!(s.get_item(&a2).unwrap().is_none());

    // Deleting an unknown item is still a no-op for the graph.
    let (_, mark) = g.changes_since(0, 0).unwrap();
    assert!(!s.delete_item(&iref("mail", "nope")).unwrap());
    assert_eq!(g.changes_since(mark, 0).unwrap().0, Vec::<EntityId>::new());
}

#[test]
fn neighbourhood_follows_both_directions_and_skips_inactive() {
    let g = mem();
    let [a, b, c, d] = ["A", "B", "C", "D"].map(|n| entity(&g, EntityKind::Person, n));
    g.add_claim(&claim(
        &a,
        "knows",
        ClaimObject::Entity(b.clone()),
        vec![ev("s", "1")],
    ))
    .unwrap();
    g.add_claim(&claim(
        &c,
        "manages",
        ClaimObject::Entity(a.clone()),
        vec![ev("s", "1")],
    ))
    .unwrap();
    let dead = g
        .add_claim(&claim(
            &a,
            "knows",
            ClaimObject::Entity(d.clone()),
            vec![ev("s", "2")],
        ))
        .unwrap();
    g.add_claim(&claim(
        &a,
        "likes",
        ClaimObject::Literal("tea".into()),
        vec![ev("s", "1")],
    ))
    .unwrap();
    g.set_claim_status(dead, ClaimStatus::Superseded).unwrap();
    let v = g.neighbourhood(&a, 2, 100).unwrap();
    let mut names: Vec<_> = v.nodes.iter().map(|n| n.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["A", "B", "C"]);
    assert_eq!(v.nodes[0].depth, 0);
    assert_eq!(v.edges.len(), 2);
    assert!(v.edges.iter().any(|e| e.from == c && e.to == a));
    let v = g.neighbourhood(&a, 0, 100).unwrap();
    assert_eq!((v.nodes.len(), v.edges.len()), (1, 0));
    assert!(g.neighbourhood(&EntityId::new("ent_x"), 1, 10).is_err());
}

#[test]
fn change_feed_is_monotonic_and_pages() {
    let g = mem();
    let ids: Vec<EntityId> = (0..5)
        .map(|i| entity(&g, EntityKind::Topic, &format!("t{i}")))
        .collect();
    let (page, high) = g.changes_since(0, 2).unwrap();
    assert_eq!(page, ids[..2]);
    let (rest, high2) = g.changes_since(high, 0).unwrap();
    assert_eq!(rest, ids[2..]);
    assert!(high2 > high);
    // Touching an entity again moves it to the end of the feed.
    g.add_alias(&ids[0], "zero").unwrap();
    let (again, high3) = g.changes_since(high2, 0).unwrap();
    assert_eq!(again, vec![ids[0].clone()]);
    assert!(high3 > high2);
    let (all, _) = g.changes_since(0, 0).unwrap();
    assert_eq!(all.len(), 5, "one entry per entity");
    // A failed mutation records nothing.
    assert!(g.add_alias(&EntityId::new("ent_none"), "x").is_err());
    assert_eq!(g.changes_since(high3, 0).unwrap().0, Vec::<EntityId>::new());
}

#[test]
fn identifiers_conflict_on_create_and_after_merge() {
    let g = mem();
    let mut new = NewEntity::new(EntityKind::Person, "A");
    new.identifiers = vec![
        Identifier::new("email", "a@x.test"),
        Identifier::new("Email", "A@X.test"),
    ];
    let a = g.create_entity(&new).unwrap();
    assert_eq!(g.get_entity(&a).unwrap().unwrap().identifiers.len(), 1);
    let mut dup = NewEntity::new(EntityKind::Person, "B");
    dup.identifiers = vec![Identifier::new("email", "A@x.TEST")];
    assert!(matches!(g.create_entity(&dup), Err(Error::Invalid(_))));
    assert_eq!(
        g.counts().unwrap().entities,
        1,
        "failed create left nothing"
    );
    // Adding an identifier an entity already has is fine.
    g.add_identifier(&a, &Identifier::new("email", "a@x.test"))
        .unwrap();
}

#[test]
fn graph_persists_in_the_encrypted_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    let key = StoreKey::from_bytes([9u8; 32]);
    let (a, c) = {
        let g = Store::open(&path, &key).unwrap().graph();
        let mut new = NewEntity::new(EntityKind::Person, "Secret Person");
        new.identifiers = vec![Identifier::new("email", "secret@example.test")];
        let a = g.create_entity(&new).unwrap();
        let c = g
            .add_claim(&claim(
                &a,
                "lives_in",
                ClaimObject::Literal("Hidden Valley".into()),
                vec![ev("s", "1")],
            ))
            .unwrap();
        (a, c)
    };
    let raw = std::fs::read(&path).unwrap();
    for needle in [
        &b"Secret Person"[..],
        b"Hidden Valley",
        b"secret@example.test",
    ] {
        assert!(
            !raw.windows(needle.len()).any(|w| w == needle),
            "plaintext graph data on disk"
        );
    }
    let g = Store::open(&path, &key).unwrap().graph();
    assert_eq!(g.get_entity(&a).unwrap().unwrap().name, "Secret Person");
    assert_eq!(
        g.get_claim(c).unwrap().unwrap().object,
        ClaimObject::Literal("Hidden Valley".into())
    );
    assert!(Store::open(&path, &StoreKey::from_bytes([1u8; 32])).is_err());
}

#[test]
fn graph_is_shareable_across_threads() {
    let g = mem();
    let handles: Vec<_> = (0..4)
        .map(|t| {
            let g = g.clone();
            std::thread::spawn(move || {
                for i in 0..25 {
                    let e = g
                        .create_entity(&NewEntity::new(EntityKind::Topic, format!("t{t}-{i}")))
                        .unwrap();
                    g.add_claim(&claim(
                        &e,
                        "p",
                        ClaimObject::Literal("v".into()),
                        vec![ev("s", "1")],
                    ))
                    .unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let n = g.counts().unwrap();
    assert_eq!((n.entities, n.claims), (100, 100));
    assert_eq!(g.retract_item(&iref("s", "1")).unwrap().len(), 100);
    assert_eq!(g.counts().unwrap().claims, 0);
}
