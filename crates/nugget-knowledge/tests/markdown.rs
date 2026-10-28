//! Markdown notes: frontmatter, tags, wikilinks, contacts.

mod common;

use common::*;
use nugget_graph::{ClaimObject, EntityKind, Identifier};
use nugget_knowledge::KnowledgeConfig;
use nugget_store::{NewItem, content_hash};
use serde_json::json;

#[tokio::test]
async fn frontmatter_tags_and_wikilinks() {
    let env = Env::new();
    // A typed project note.
    env.put(
        note(
            "files:notes",
            "projects/Apollo.md",
            "Project Apollo",
            json!({"type": "project", "status": "active", "aliases": ["Apollo"],
                   "people": ["[[Alice Smith]]"], "tags": ["rust"]}),
            &["rust", "infra"],
            &["Search Revamp"],
            "We are rebuilding the indexer in #rust. See [[Search Revamp]].",
        ),
        &["We are rebuilding the indexer in #rust. See [[Search Revamp]]."],
    );
    // An untyped note that belongs to a project and links to Apollo by alias.
    env.put(
        note(
            "files:notes",
            "daily/2026-03-01.md",
            "2026-03-01",
            json!({"project": "Search Revamp"}),
            &["meetings"],
            &["Apollo"],
            "Synced with the team about [[Apollo]].",
        ),
        &["Synced with the team about [[Apollo]]."],
    );
    let k = env.knowledge(KnowledgeConfig::default(), None);
    let stats = run(&k).await;
    assert_eq!(stats.items_failed, 0);

    let apollo = env.entity(EntityKind::Project, "Project Apollo");
    assert_eq!(apollo.attributes["status"], "active");
    let rust = env.entity(EntityKind::Topic, "rust");
    assert!(rust.identifiers.contains(&Identifier::new("tag", "rust")));
    let topics = env.active(&apollo, "has_topic");
    assert_eq!(topics.len(), 2, "rust and infra");
    let alice = env.entity(EntityKind::Person, "Alice Smith");
    assert_eq!(env.active(&alice, "works_on").len(), 1);

    // The daily note's project gets its tag as a topic.
    let revamp = env.entity(EntityKind::Project, "Search Revamp");
    let meetings = env.entity(EntityKind::Topic, "meetings");
    assert!(
        env.active(&revamp, "has_topic")
            .iter()
            .any(|c| c.object == ClaimObject::Entity(meetings.id.clone()))
    );

    // Links: Apollo → Search Revamp (resolved after the second note created it), and
    // Search Revamp → Apollo (by alias, via the notes index).
    let a_refs = env.active(&apollo, "references");
    assert_eq!(a_refs.len(), 1);
    assert_eq!(a_refs[0].object, ClaimObject::Entity(revamp.id.clone()));
    assert_eq!(
        a_refs[0].evidence[0].item,
        item_ref("files:notes", "projects/Apollo.md")
    );
    assert!(
        a_refs[0].evidence[0]
            .quote
            .as_deref()
            .unwrap()
            .contains("[[Search Revamp]]")
    );
    let r_refs = env.active(&revamp, "references");
    assert_eq!(r_refs.len(), 1);
    assert_eq!(r_refs[0].object, ClaimObject::Entity(apollo.id.clone()));
    assert_eq!(k.status().pending_links, 0);

    // Hashtag evidence points at the chunk.
    let rust_claim = topics
        .iter()
        .find(|c| c.object == ClaimObject::Entity(rust.id.clone()))
        .unwrap();
    assert!(rust_claim.evidence[0].chunk_id.is_some());
}

#[tokio::test]
async fn unresolvable_links_wait_until_the_target_exists() {
    let env = Env::new();
    env.put(
        note(
            "files:notes",
            "a.md",
            "Alpha",
            json!({"type": "project"}),
            &[],
            &["Beta"],
            "Depends on [[Beta]].",
        ),
        &["Depends on [[Beta]]."],
    );
    let k = env.knowledge(KnowledgeConfig::default(), None);
    run(&k).await;
    let alpha = env.entity(EntityKind::Project, "Alpha");
    assert!(env.active(&alpha, "references").is_empty());
    assert_eq!(k.status().pending_links, 1);

    env.put(
        note(
            "files:notes",
            "b.md",
            "Beta",
            json!({"type": "topic"}),
            &[],
            &[],
            "Beta notes.",
        ),
        &["Beta notes."],
    );
    run(&k).await;
    let beta = env.entity(EntityKind::Topic, "Beta");
    let refs = env.active(&alpha, "references");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].object, ClaimObject::Entity(beta.id));
    assert_eq!(k.status().pending_links, 0);
}

#[tokio::test]
async fn vcards_merge_identities_they_assert_together() {
    let env = Env::new();
    // Two mails create two people (different addresses: never merged by name).
    Mail {
        key: "1",
        from: "Dana Scully <dana@fbi.gov>",
        ..Default::default()
    }
    .put(&env);
    Mail {
        key: "2",
        from: "Dana Scully <dana.s@gmail.com>",
        ..Default::default()
    }
    .put(&env);
    let k = env.knowledge(KnowledgeConfig::default(), None);
    run(&k).await;
    let danas = |env: &Env| {
        env.entities(EntityKind::Person)
            .into_iter()
            .filter(|e| e.name == "Dana Scully")
            .count()
    };
    assert_eq!(
        danas(&env),
        2,
        "conflicting addresses are not merged on name"
    );

    // A contact card lists both addresses: the two are one person.
    let vcf = "BEGIN:VCARD\nVERSION:3.0\nFN:Dana Scully\nEMAIL:dana@fbi.gov\nEMAIL:dana.s@gmail.com\nORG:FBI\nTITLE:Special Agent\nEND:VCARD\n";
    env.put(
        NewItem::new("files:contacts", "dana.vcf", content_hash(vcf))
            .with_mime("text/vcard")
            .with_text(vcf)
            .with_metadata(json!({"mime": "text/vcard", "sensitive": false})),
        &[vcf],
    );
    let stats = run(&k).await;
    assert_eq!(stats.entities_merged, 1);
    assert_eq!(danas(&env), 1);
    let dana = env.entity(EntityKind::Person, "Dana Scully");
    assert_eq!(dana.identifiers.len(), 2);
    let title = env.active(&dana, "has_title");
    assert_eq!(title.len(), 1);
    assert_eq!(
        title[0].object,
        ClaimObject::Literal("Special Agent".into())
    );
    assert!(
        title[0].evidence[0]
            .quote
            .as_deref()
            .unwrap()
            .contains("TITLE")
    );
    let fbi = env.entity(EntityKind::Organization, "FBI");
    // The contact card's employer: fbi.gov also produced "Fbi" from the domain, which is the same
    // organisation by name.
    assert!(
        env.active(&dana, "works_at")
            .iter()
            .any(|c| c.object == ClaimObject::Entity(fbi.id.clone()))
    );
}
