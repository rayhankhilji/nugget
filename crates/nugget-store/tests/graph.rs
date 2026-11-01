//! Knowledge-graph storage: the shared `GraphStore` conformance suite
//! plus SQLite-specific behaviour (persistence, encryption, delete paths, merge edge cases).

use std::path::PathBuf;

use nugget_core::ids::{EntityId, ItemKey, ItemRef, SourceId};
use nugget_graph::{
    ClaimObject, ClaimStatus, EntityKind, EntityPatch, EntityQuery, Evidence, GraphStore, Mention,
    NewClaim, NewEntity,
};
use nugget_store::{NewChunk, NewItem, SqliteGraph, Store, StoreKey, content_hash};

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
    assert_eq!(
        q("ALP", None),
        ["Alpha", "Alpine Person"].map(String::from).to_vec()
    );
    assert_eq!(q("alch", None), ["Charlie"].map(String::from).to_vec());
    assert_eq!(q("éCLA", None), ["Éclair"].map(String::from).to_vec());
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

    let (_, mark) = g.changes_since(0, 0).unwrap();
    assert!(!s.delete_item(&iref("mail", "nope")).unwrap());
    assert_eq!(g.changes_since(mark, 0).unwrap().0, Vec::<EntityId>::new());
}

#[test]
fn pruned_entities_stay_tombstoned_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = db_path(&dir);
    let key = StoreKey::from_bytes([3u8; 32]);
    let (gone, kept) = {
        let s = Store::open(&path, &key).unwrap();
        let g = s.graph();
        let gone = entity(&g, EntityKind::Topic, "Ephemeral");
        g.add_mention(&mention(&gone, "mail", "1", "eph")).unwrap();
        let kept = entity(&g, EntityKind::Topic, "Kept");
        g.add_mention(&mention(&kept, "mail", "2", "kept")).unwrap();
        let old = entity(&g, EntityKind::Topic, "Merged Away");
        g.merge(&gone, &old).unwrap();
        g.retract_item(&iref("mail", "1")).unwrap();
        assert_eq!(g.prune_orphans(None).unwrap(), vec![gone.clone()]);
        assert!(
            g.get_entity(&old).unwrap().is_none(),
            "merged ids follow to the tombstone"
        );
        (gone, kept)
    };
    let s = Store::open(&path, &key).unwrap();
    let g = s.graph();
    assert!(g.get_entity(&gone).unwrap().is_none());
    assert!(g.get_entity(&kept).unwrap().is_some());
    assert_eq!(g.counts().unwrap().entities, 1);
    assert!(g.list_entities(&EntityQuery::default()).unwrap().len() == 1);
    assert!(g.neighbourhood(&gone, 1, 0).is_err());
}

/// GG5-4 review: a claim the user replaced remembers the items it had evidence from and their
/// content then.
#[test]
fn user_replacement_remembers_item_versions() {
    let s = Store::open_in_memory().unwrap();
    let g = s.graph();
    let put = |text: &str| {
        let it = NewItem::new("files", "n.md", content_hash(text)).with_text(text);
        s.upsert_item_with_chunks(&it, &[NewChunk::text(text)])
            .unwrap();
    };
    put("Harbor is part of Acme.");
    let a = entity(&g, EntityKind::Project, "Harbor");
    let quoted = |q: &str| Evidence {
        item: iref("files", "n.md"),
        chunk_id: Some(1),
        quote: Some(q.into()),
    };
    let lit = ClaimObject::Literal("Acme".into());
    let old = g
        .add_claim(&claim(
            &a,
            "part_of",
            lit.clone(),
            vec![quoted("part of Acme")],
        ))
        .unwrap();
    let mut mine = claim(&a, "part_of", ClaimObject::Literal("mine".into()), vec![]);
    mine.status = ClaimStatus::UserAsserted;
    g.supersede_by_user(old, &mine).unwrap();

    assert!(
        g.replaced_by_user(old, &[quoted("Acme owns Harbor")])
            .unwrap()
    );
    g.retract_item(&iref("files", "n.md")).unwrap();
    g.add_evidence(old, &quoted("Acme owns Harbor")).unwrap();
    assert_eq!(
        g.get_claim(old).unwrap().unwrap().status,
        ClaimStatus::Superseded
    );

    put("Harbor is part of Acme.\n\nre-saved");
    assert!(g.replaced_by_user(old, &[quoted("part of Acme")]).unwrap());
    assert!(
        g.replaced_by_user(old, &[quoted("Acme runs Harbor")])
            .unwrap()
    );
    g.retract_item(&iref("files", "n.md")).unwrap();
    g.add_evidence(old, &quoted("Acme runs Harbor")).unwrap();
    assert_eq!(
        g.get_claim(old).unwrap().unwrap().status,
        ClaimStatus::Superseded
    );
    put("Harbor moved to Acme Labs.");
    assert!(
        !g.replaced_by_user(old, &[quoted("Harbor moved to Acme Labs")])
            .unwrap()
    );
    assert!(g.replaced_by_user(old, &[quoted("part of Acme")]).unwrap());
    g.retract_item(&iref("files", "n.md")).unwrap();
    g.add_evidence(old, &quoted("Harbor moved to Acme Labs"))
        .unwrap();
    assert_eq!(
        g.get_claim(old).unwrap().unwrap().status,
        ClaimStatus::Active
    );
    assert!(!g.replaced_by_user(old, &[quoted("part of Acme")]).unwrap());
}

/// GG5-13 review: the assertion time survives a store reopen and later evidence.
#[test]
fn assertion_time_is_kept() {
    let g = mem();
    let a = entity(&g, EntityKind::Person, "Dana");
    let mut mine = claim(
        &a,
        "works_at",
        ClaimObject::Literal("Corvid".into()),
        vec![],
    );
    mine.status = ClaimStatus::UserAsserted;
    let id = g.add_claim(&mine).unwrap();
    let at = g.asserted_at(id).unwrap().expect("asserted");
    let c = g.get_claim(id).unwrap().unwrap();
    assert_eq!(at, c.created_at);
    std::thread::sleep(std::time::Duration::from_millis(5));
    g.add_evidence(id, &ev("s", "1")).unwrap();
    assert!(g.get_claim(id).unwrap().unwrap().updated_at > at);
    assert_eq!(g.asserted_at(id).unwrap(), Some(at));
}
