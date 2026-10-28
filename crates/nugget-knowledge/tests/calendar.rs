//! Calendar events (ics and Google Calendar shapes) and cross-source resolution.

mod common;

use common::*;
use nugget_graph::{ClaimObject, EntityKind, Identifier};
use nugget_knowledge::{KnowledgeConfig, MeConfig};
use nugget_store::{NewItem, content_hash};
use serde_json::json;

fn cfg() -> KnowledgeConfig {
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
    let k = env.knowledge(cfg(), None);
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
    let k = env.knowledge(cfg(), None);
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
