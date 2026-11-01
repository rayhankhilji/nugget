//! Deterministic extractors end to end: mail headers, markdown notes and contact cards,
//! calendar events, and cross-source resolution.

mod common;

use common::*;
use nugget_graph::{ClaimObject, EntityKind, Identifier};
use nugget_knowledge::{KnowledgeConfig, MeConfig};
use nugget_store::{NewItem, content_hash};
use serde_json::json;

fn me_cfg() -> KnowledgeConfig {
    KnowledgeConfig {
        me: MeConfig {
            name: Some("Tanuj".into()),
            emails: vec!["tanuj@trylumi.co".into()],
            infer_from_sent: true,
        },
        ..Default::default()
    }
}

#[tokio::test]
async fn mail_headers_make_people_orgs_and_claims() {
    let env = Env::new();
    Mail {
        from: "Alice Smith <alice@acme.com>",
        to: &["Tanuj <tanuj@trylumi.co>", "\"Jones, Bob\" <bob@gmail.com>"],
        cc: &["no-reply@acme.com"],
        ..Default::default()
    }
    .put(&env);
    let k = env.knowledge(me_cfg(), None);
    let stats = run(&k).await;
    assert_eq!(stats.items_processed, 1);
    assert_eq!(stats.items_failed, 0);

    let alice = env.entity(EntityKind::Person, "Alice Smith");
    assert!(
        alice
            .identifiers
            .contains(&Identifier::new("email", "alice@acme.com"))
    );
    let bob = env.entity(EntityKind::Person, "Bob Jones");
    assert!(bob.aliases.is_empty() || bob.aliases.iter().all(|a| a != "Jones, Bob"));

    // Organisation from the corporate domain only.
    let acme = env.entity(EntityKind::Organization, "Acme");
    assert!(
        acme.identifiers
            .contains(&Identifier::new("domain", "acme.com"))
    );
    assert!(env.try_entity(EntityKind::Organization, "Gmail").is_none());
    assert_eq!(env.entities(EntityKind::Organization).len(), 1);

    // works_at with a confidence penalty, for the corporate address only.
    let works = env.active(&alice, "works_at");
    assert_eq!(works.len(), 1);
    assert_eq!(works[0].object, ClaimObject::Entity(acme.id.clone()));
    assert!(works[0].confidence <= 0.5);
    assert!(env.active(&bob, "works_at").is_empty());

    // The automated sender is nobody.
    assert!(
        env.try_entity(EntityKind::Person, "no-reply@acme.com")
            .is_none()
    );

    // The user corresponded with both people; evidence quotes the header.
    let me = env.me();
    assert_eq!(me.name, "Tanuj");
    let corr = env.active(&me, "corresponded_with");
    assert_eq!(corr.len(), 2);
    let to_alice = corr
        .iter()
        .find(|c| c.object == ClaimObject::Entity(alice.id.clone()))
        .unwrap();
    assert_eq!(to_alice.evidence.len(), 1);
    let ev = &to_alice.evidence[0];
    assert_eq!(ev.item, item_ref("imap:me@corp.example", "INBOX/1"));
    assert_eq!(
        ev.quote.as_deref(),
        Some("From: Alice Smith <alice@acme.com>")
    );

    // Mentions recorded for each occurrence.
    assert!(!env.g().mentions_for(&alice.id, 10).unwrap().is_empty());
    // Every claim carries evidence.
    for c in env.all_claims() {
        assert!(!c.evidence.is_empty(), "claim without evidence: {c:?}");
    }
}

#[tokio::test]
async fn user_is_learned_from_sent_mail_and_merged() {
    let env = Env::new();
    // Received first: the user's own address appears as an ordinary recipient.
    Mail {
        key: "INBOX/1",
        from: "Alice Smith <alice@acme.com>",
        to: &["Tanuj Gupta <tanuj@trylumi.co>"],
        ..Default::default()
    }
    .put(&env);
    let k = env.knowledge(KnowledgeConfig::default(), None);
    run(&k).await;
    let me_before = env.me();
    assert_eq!(me_before.name, "Me");
    // Not known yet: a person entity holds the address, and the user only received the mail.
    assert!(env.try_entity(EntityKind::Person, "Tanuj Gupta").is_some());

    // A message in the sent folder reveals the address.
    Mail {
        key: "Sent/1",
        from: "Tanuj Gupta <tanuj@trylumi.co>",
        to: &["Carol White <carol@globex.com>"],
        date: "2026-03-02T10:00:00Z",
        source_meta: json!({"folder": "[Gmail]/Sent Mail"}),
        ..Default::default()
    }
    .put(&env);
    run(&k).await;
    let me = env.me();
    assert_eq!(me.id, me_before.id);
    assert_eq!(me.name, "Tanuj Gupta");
    assert!(
        me.identifiers
            .contains(&Identifier::new("email", "tanuj@trylumi.co"))
    );
    // The stand-in person was merged into the user; no self-loops remain.
    let gupta = env.entities(EntityKind::Person);
    assert_eq!(gupta.iter().filter(|e| e.name.contains("Tanuj")).count(), 1);
    for c in env.g().claims_for(&me.id, false).unwrap() {
        assert!(!(c.subject == me.id && c.object == ClaimObject::Entity(me.id.clone())));
    }
    let carol = env.entity(EntityKind::Person, "Carol White");
    assert!(
        env.active(&me, "corresponded_with")
            .iter()
            .any(|c| c.object == ClaimObject::Entity(carol.id.clone()))
    );
    assert!(k.status().me.is_some());
}

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

fn cal_cfg() -> KnowledgeConfig {
    KnowledgeConfig {
        me: MeConfig {
            emails: vec!["tanuj@trylumi.co".into()],
            ..Default::default()
        },
        ..Default::default()
    }
}

const ICS_TEXT: &str = "Event: Quarterly planning\nStart: 2026-04-10 09:00\nEnd: 2026-04-10 11:00\nLocation: Acme HQ, Room 4\nOrganizer: Alice Smith <alice@acme.com>\nAttendees: Tanuj <tanuj@trylumi.co> (accepted), Bob Jones <bob@acme.com> (declined), Carol White <carol@globex.com> (accepted)";

fn planning() -> NewItem {
    ics(
        "caldav:work",
        "cal/planning.ics",
        json!([{
            "kind": "event",
            "summary": "Quarterly planning",
            "uid": "plan-123@acme.com",
            "start": "2026-04-10T09:00:00",
            "end": "2026-04-10T11:00:00",
            "timezone": "Europe/Berlin",
            "all_day": false,
            "location": "Acme HQ, Room 4",
            "organizer": "Alice Smith <alice@acme.com>",
            "attendees": [
                "Tanuj <tanuj@trylumi.co> (accepted)",
                "Bob Jones <bob@acme.com> (declined)",
                "Carol White <carol@globex.com> (accepted)"
            ]
        }]),
        ICS_TEXT,
    )
}

#[tokio::test]
async fn ics_event_attendees_place_and_times() {
    let env = Env::new();
    env.put(planning(), &[ICS_TEXT]);
    let k = env.knowledge(cal_cfg(), None);
    let stats = run(&k).await;
    assert_eq!(stats.items_failed, 0);

    let ev = env.entity(EntityKind::Event, "Quarterly planning");
    assert_eq!(ev.attributes["start"], "2026-04-10T09:00:00");
    assert_eq!(ev.attributes["end"], "2026-04-10T11:00:00");
    assert_eq!(ev.attributes["timezone"], "Europe/Berlin");
    assert!(
        ev.identifiers
            .contains(&Identifier::new("ical_uid", "plan-123@acme.com"))
    );

    let alice = env.entity(EntityKind::Person, "Alice Smith");
    let organized = env.active(&alice, "organized");
    assert_eq!(organized.len(), 1);
    assert_eq!(organized[0].object, ClaimObject::Entity(ev.id.clone()));
    assert!(organized[0].valid_from.is_some());
    // The evidence points at the chunk that names her.
    assert!(organized[0].evidence[0].chunk_id.is_some());
    assert!(
        organized[0].evidence[0]
            .quote
            .as_deref()
            .unwrap()
            .contains("alice@acme.com")
    );

    let carol = env.entity(EntityKind::Person, "Carol White");
    assert_eq!(env.active(&carol, "attended").len(), 1);
    let bob = env.entity(EntityKind::Person, "Bob Jones");
    assert!(env.active(&bob, "attended").is_empty());
    assert_eq!(env.active(&bob, "invited_to").len(), 1);

    let me = env.me();
    assert_eq!(env.active(&me, "attended").len(), 1);

    let place = env.entity(EntityKind::Place, "Acme HQ, Room 4");
    let at = env.active(&ev, "located_at");
    assert_eq!(at.len(), 1);
    assert_eq!(at[0].object, ClaimObject::Entity(place.id));
}

#[tokio::test]
async fn same_person_from_mail_and_calendar_is_one_entity() {
    let env = Env::new();
    Mail {
        from: "Carol White <carol@globex.com>",
        to: &["tanuj@trylumi.co"],
        ..Default::default()
    }
    .put(&env);
    env.put(planning(), &[ICS_TEXT]);
    // A Google Calendar event (another source) with Carol under a different display name.
    let gcal = NewItem::new("gcal:primary", "evt-9", content_hash("evt-9"))
        .with_title("Globex sync")
        .with_mime("text/plain")
        .with_version("etag-1")
        .with_metadata(json!({
            "source": {
                "event_id": "evt-9",
                "ical_uid": "evt-9@google.com",
                "start": {"date_time": "2026-05-01T15:00:00Z"},
                "end": {"date_time": "2026-05-01T15:30:00Z"},
                "location": "https://meet.google.com/abc",
                "organizer": {"email": "tanuj@trylumi.co", "name": "Tanuj"},
                "attendees": [
                    {"email": "carol@globex.com", "name": "C. White", "response": "accepted"},
                    {"email": "room-1@resource.calendar.google.com", "name": "Room 1", "resource": true}
                ]
            },
            "mime": "text/plain",
            "sensitive": false
        }));
    env.put(
        gcal,
        &["Globex sync\nAttendees: C. White <carol@globex.com> (accepted)"],
    );
    let k = env.knowledge(cal_cfg(), None);
    run(&k).await;

    let carols: Vec<_> = env
        .entities(EntityKind::Person)
        .into_iter()
        .filter(|e| {
            e.identifiers
                .contains(&Identifier::new("email", "carol@globex.com"))
        })
        .collect();
    assert_eq!(carols.len(), 1, "one Carol across mail, ics and gcal");
    let carol = &carols[0];
    assert!(carol.aliases.iter().any(|a| a == "C. White") || carol.name == "C. White");
    // Two events attended, one mail correspondence.
    assert_eq!(env.active(carol, "attended").len(), 2);
    let me = env.me();
    assert!(
        env.active(&me, "corresponded_with")
            .iter()
            .any(|c| c.object == ClaimObject::Entity(carol.id.clone()))
    );
    // The user organised the gcal event; the meeting link is not a place; resources are not people.
    let sync = env.entity(EntityKind::Event, "Globex sync");
    assert_eq!(env.active(&me, "organized").len(), 1);
    assert!(env.active(&sync, "located_at").is_empty());
    assert!(env.try_entity(EntityKind::Person, "Room 1").is_none());
    // Mentions from three items.
    let mentions = env.g().mentions_for(&carol.id, 0).unwrap();
    let items: std::collections::HashSet<_> = mentions.iter().map(|m| m.item.clone()).collect();
    assert_eq!(items.len(), 3);
}

/// Security review 3, ER-7: an event's location that is a dial-in number or an address is not
/// a place: no Place entity (a vault page) named after it, no claim sentence carrying it.
#[tokio::test]
async fn a_phone_number_or_address_as_location_is_not_a_place() {
    let env = Env::new();
    for (key, summary, loc) in [
        (
            "cal/dial.ics",
            "Vendor call",
            "Dial-in +44 20 7946 0958, PIN 4471",
        ),
        ("cal/mail.ics", "Offsite", "ana.ruiz@lumenfold.example"),
        ("cal/room.ics", "Standup", "Room 4"),
    ] {
        let text = format!("Event: {summary}\nLocation: {loc}");
        env.put(
            ics(
                "caldav:work",
                key,
                json!([{"kind": "event", "summary": summary, "uid": key,
                        "start": "2026-04-10T09:00:00", "location": loc}]),
                &text,
            ),
            &[&text],
        );
    }
    let k = env.knowledge(cal_cfg(), None);
    assert_eq!(run(&k).await.items_failed, 0);
    let places: Vec<String> = env
        .entities(EntityKind::Place)
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(places, ["Room 4"]);
    for c in env.all_claims() {
        assert!(
            !c.text.contains("7946") && !c.text.contains('@'),
            "{}",
            c.text
        );
    }
}
