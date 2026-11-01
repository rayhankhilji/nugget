//! Facts say what the sources say, as of when the sources are from (QA GG5): a note is dated by
//! its own date, not when the file was copied; the user's value of a single-valued fact stays
//! the one current value; a calendar acceptance is not attendance before the event; a recurring
//! event is not its first date; meeting attendees are not project members; Cc is not
//! correspondence.

mod common;

use common::*;
use nugget_graph::{Claim, ClaimObject, ClaimStatus, EntityKind, NewClaim};
use nugget_knowledge::{KnowledgeConfig, MeConfig};
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

fn at(s: &str) -> OffsetDateTime {
    OffsetDateTime::parse(s, &Rfc3339).unwrap()
}

fn cfg() -> KnowledgeConfig {
    KnowledgeConfig {
        me: MeConfig {
            name: Some("Mara Okafor".into()),
            emails: vec!["mara.okafor@brightwell.example".into()],
            infer_from_sent: false,
        },
        ..Default::default()
    }
}

/// The name of a claim's entity object.
fn object_name(env: &Env, c: &Claim) -> String {
    match &c.object {
        ClaimObject::Entity(id) => env.g().get_entity(id).unwrap().unwrap().name,
        ClaimObject::Literal(l) => l.clone(),
    }
}

/// Dana's person note (both her addresses), as of `date`, saying she works at `org`.
fn dana_note(env: &Env, date: &str, org: &str) {
    let body = format!("# Dana Whitfield\n\nWorks at {org}.");
    env.put(
        note(
            "files:notes",
            "Notes/People/Dana-Whitfield.md",
            "Dana Whitfield",
            json!({"type": "person", "date": date,
                   "email": ["dana@corvidlabs.example", "dana.whitfield@brightwell.example"],
                   "org": org}),
            &[],
            &[],
            &body,
        ),
        &[&body],
    );
}

/// Dana's mails: at Brightwell in March, at Corvid Labs from August (and her person note).
fn dana_mails(env: &Env) {
    dana_note(env, "2026-08-17", "Corvid Labs");
    Mail {
        source: "imap:mara",
        key: "INBOX/1",
        from: "Dana Whitfield <dana.whitfield@brightwell.example>",
        to: &["Mara Okafor <mara.okafor@brightwell.example>"],
        date: "2026-03-04T09:00:00Z",
        subject: "Dashboards",
        body: "The Harbor dashboards are ready for review.",
        ..Default::default()
    }
    .put(env);
    Mail {
        source: "imap:mara",
        key: "INBOX/2",
        from: "Dana Whitfield <dana@corvidlabs.example>",
        to: &["Mara Okafor <mara.okafor@brightwell.example>"],
        date: "2026-08-14T09:00:00Z",
        subject: "New job",
        body: "I've joined Corvid Labs as Head of Product.",
        ..Default::default()
    }
    .put(env);
}

/// GG5-6: an old person note copied in today (mtime = now) is dated by its frontmatter
/// `date:` (March), so it does not make Brightwell Dana's current employer again over her
/// August mail.
#[tokio::test]
async fn a_copied_old_note_keeps_its_own_date() {
    let env = Env::new();
    dana_mails(&env);
    let now = OffsetDateTime::now_utc();
    env.put(
        note(
            "files:notes",
            "Notes/People/Dana (Brightwell).md",
            "Dana Whitfield",
            json!({"type": "person", "date": "2026-03-01",
                   "email": "dana.whitfield@brightwell.example",
                   "org": "Brightwell Analytics"}),
            &[],
            &[],
            "# Dana Whitfield\n\nDesign Lead at Brightwell Analytics.",
        )
        .with_modified_at(now),
        &["# Dana Whitfield\n\nDesign Lead at Brightwell Analytics."],
    );
    let k = env.knowledge(cfg(), None);
    run(&k).await;

    let dana = env.entity(EntityKind::Person, "Dana Whitfield");
    let current = env.active(&dana, "works_at");
    assert_eq!(current.len(), 1, "{current:#?}");
    assert!(object_name(&env, &current[0]).starts_with("Corvid"));
    // Every Brightwell claim is history, dated by the note's own date, not the copy time.
    let old: Vec<_> = env
        .claims(&dana, "works_at")
        .into_iter()
        .filter(|c| object_name(&env, c).starts_with("Brightwell"))
        .collect();
    assert!(!old.is_empty());
    for c in &old {
        assert_eq!(c.status, ClaimStatus::Superseded, "{c:#?}");
        assert!(c.valid_from.is_some_and(|f| f < at("2026-04-01T00:00:00Z")));
    }
}

/// GG5-13: after the user edits Dana's employer, a newer note saying she joined Kestrel
/// Robotics does not become a second current employer: the user's value stays current and the
/// newer extracted value is superseded (history the user can restore).
#[tokio::test]
async fn the_users_value_stays_the_one_current_value() {
    let env = Env::new();
    dana_mails(&env);
    let k = env.knowledge(cfg(), None);
    run(&k).await;
    let dana = env.entity(EntityKind::Person, "Dana Whitfield");
    let corvid = env.entity(EntityKind::Organization, "Corvid Labs");
    // The vault edit: the user's own wording of the employer.
    let mine = env
        .g()
        .add_claim(&NewClaim {
            subject: dana.id.clone(),
            predicate: "works_at".into(),
            object: ClaimObject::Entity(corvid.id.clone()),
            text: "Dana Whitfield is Head of Product at Corvid Labs (fixed by me)".into(),
            confidence: 1.0,
            status: ClaimStatus::UserAsserted,
            evidence: Vec::new(),
            valid_from: None,
            valid_to: None,
        })
        .unwrap();
    // Her person note, re-saved: she joined Kestrel Robotics.
    dana_note(&env, "2026-10-30", "Kestrel Robotics");
    run(&k).await;

    let current = env.active(&dana, "works_at");
    assert_eq!(
        current.len(),
        1,
        "exactly one current employer: {current:#?}"
    );
    assert_eq!(current[0].id, mine);
    assert_eq!(current[0].status, ClaimStatus::UserAsserted);
    let kestrel = env.entity(EntityKind::Organization, "Kestrel Robotics");
    let k_claim = env
        .claims(&dana, "works_at")
        .into_iter()
        .find(|c| c.object == ClaimObject::Entity(kestrel.id.clone()))
        .expect("the newer value is kept as history");
    assert_eq!(k_claim.status, ClaimStatus::Superseded);
}

const LAUNCH_TEXT: &str = "Event: Harbor launch day\nStart: 2026-12-03 09:00\nEnd: 2026-12-03 17:00\nOrganizer: Mara Okafor <mara.okafor@brightwell.example>\nAttendees: Mara Okafor <mara.okafor@brightwell.example> (accepted), Robert Chen <robert.chen@brightwell.example> (accepted)";
const STANDUP_TEXT: &str = "Event: Harbor stand-up\nStart: 2026-03-10 09:30\nEnd: 2026-03-10 09:45\nRepeats: FREQ=WEEKLY;BYDAY=TU,WE,TH\nOrganizer: Mara Okafor <mara.okafor@brightwell.example>\nAttendees: Mara Okafor <mara.okafor@brightwell.example> (accepted), Sam Patel <sam.patel@brightwell.example> (accepted)";

fn calendar(env: &Env) {
    env.put(
        ics(
            "files:cal",
            "Calendar/2026-12-03-harbor-launch.ics",
            json!([{
                "kind": "event", "summary": "Harbor launch day", "uid": "launch@bw",
                "start": "2026-12-03T09:00:00Z", "end": "2026-12-03T17:00:00Z",
                "organizer": "Mara Okafor <mara.okafor@brightwell.example>",
                "attendees": ["Mara Okafor <mara.okafor@brightwell.example> (accepted)",
                              "Robert Chen <robert.chen@brightwell.example> (accepted)"]
            }]),
            LAUNCH_TEXT,
        ),
        &[LAUNCH_TEXT],
    );
    env.put(
        ics(
            "files:cal",
            "Calendar/2026-03-10-harbor-standup.ics",
            json!([{
                "kind": "event", "summary": "Harbor stand-up", "uid": "standup@bw",
                "start": "2026-03-10T09:30:00Z", "end": "2026-03-10T09:45:00Z",
                "rrule": "FREQ=WEEKLY;BYDAY=TU,WE,TH",
                "organizer": "Mara Okafor <mara.okafor@brightwell.example>",
                "attendees": ["Mara Okafor <mara.okafor@brightwell.example> (accepted)",
                              "Sam Patel <sam.patel@brightwell.example> (accepted)"]
            }]),
            STANDUP_TEXT,
        ),
        &[STANDUP_TEXT],
    );
}

/// GG5-16: accepting a future event is not attending it; once the event is over (by the
/// pipeline's clock) it is. A weekly event is described as weekly, and an attendee's evidence
/// quotes the attendee line, not the organizer line.
#[tokio::test]
async fn calendar_claims_say_what_the_invite_says() {
    // Before the launch.
    let env = Env::new();
    calendar(&env);
    let k = env.knowledge(cfg(), None);
    k.set_clock(Some(at("2026-10-30T12:00:00Z")));
    run(&k).await;
    let launch = env.entity(EntityKind::Event, "Harbor launch day");
    let bob = env.entity(EntityKind::Person, "Robert Chen");
    assert!(env.active(&bob, "attended").is_empty());
    let invited = env.active(&bob, "invited_to");
    assert_eq!(invited.len(), 1);
    assert_eq!(invited[0].object, ClaimObject::Entity(launch.id.clone()));
    assert!(
        invited[0]
            .text
            .contains("accepted the invitation to Harbor launch day"),
        "{}",
        invited[0].text
    );
    let me = env.me();
    assert!(
        env.active(&me, "attended")
            .iter()
            .all(|c| c.object != ClaimObject::Entity(launch.id.clone()))
    );

    // The stand-up started in March: attended, as a weekly event.
    let sam = env.entity(EntityKind::Person, "Sam Patel");
    let standup = env.active(&sam, "attended");
    assert_eq!(standup.len(), 1);
    assert!(
        standup[0].text.contains("weekly from 2026-03-10") && !standup[0].text.contains(" on "),
        "{}",
        standup[0].text
    );
    // The user (organizer and attendee) attended: the quote is the attendee line.
    let mine = env.active(&me, "attended");
    assert_eq!(mine.len(), 1);
    let quote = mine[0].evidence[0].quote.as_deref().unwrap();
    assert!(quote.starts_with("Attendees:"), "{quote}");
    let organized = env.active(&me, "organized");
    assert!(organized.iter().all(|c| {
        c.evidence[0]
            .quote
            .as_deref()
            .unwrap()
            .starts_with("Organizer:")
    }));

    // After the launch, the same invite means attendance.
    let env = Env::new();
    calendar(&env);
    let k = env.knowledge(cfg(), None);
    k.set_clock(Some(at("2026-12-10T12:00:00Z")));
    run(&k).await;
    let bob = env.entity(EntityKind::Person, "Robert Chen");
    assert_eq!(env.active(&bob, "attended").len(), 1);
    assert!(env.active(&bob, "invited_to").is_empty());
}

/// GG5-16: a person only copied on a mail did not correspond with the user.
#[tokio::test]
async fn cc_alone_is_not_correspondence() {
    let env = Env::new();
    Mail {
        source: "imap:mara",
        key: "INBOX/9",
        from: "Robert Chen <robert.chen@brightwell.example>",
        to: &["Mara Okafor <mara.okafor@brightwell.example>"],
        cc: &["Dana Whitfield <dana@corvidlabs.example>"],
        date: "2026-09-01T09:00:00Z",
        subject: "Harbor",
        body: "Launch plan attached.",
        ..Default::default()
    }
    .put(&env);
    let k = env.knowledge(cfg(), None);
    run(&k).await;
    let me = env.me();
    let bob = env.entity(EntityKind::Person, "Robert Chen");
    let dana = env.entity(EntityKind::Person, "Dana Whitfield");
    let corr = env.active(&me, "corresponded_with");
    assert!(
        corr.iter()
            .any(|c| c.object == ClaimObject::Entity(bob.id.clone()))
    );
    assert!(
        corr.iter()
            .all(|c| c.object != ClaimObject::Entity(dana.id.clone())),
        "{corr:#?}"
    );
}

/// GG5-16: the people a meeting note lists are not the project's members.
#[tokio::test]
async fn meeting_note_people_are_not_project_members() {
    let env = Env::new();
    let body = "# 1:1 with Bob\n\n- Harbor: backfill green.";
    env.put(
        note(
            "files:notes",
            "Notes/Meetings/2026-06-03-1on1-with-Bob.md",
            "1:1 with Bob",
            json!({"date": "2026-06-03", "people": ["Robert Chen"], "project": "Harbor",
                   "team": ["Sam Patel"]}),
            &["one-on-one"],
            &[],
            body,
        ),
        &[body],
    );
    let k = env.knowledge(cfg(), None);
    run(&k).await;
    let bob = env.entity(EntityKind::Person, "Robert Chen");
    assert!(env.active(&bob, "works_on").is_empty());
    assert_eq!(env.active(&bob, "related_to").len(), 1);
    let sam = env.entity(EntityKind::Person, "Sam Patel");
    assert_eq!(env.active(&sam, "works_on").len(), 1);
    // The note is dated by its own date.
    let c = &env.active(&sam, "works_on")[0];
    assert_eq!(c.valid_from, Some(at("2026-06-03T00:00:00Z")));
}
