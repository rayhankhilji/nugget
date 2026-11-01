//! `.ics` files through the real extractor (GG4-1 to GG4-4).

mod common;

use common::*;
use nugget_extract::{ExtractOptions, Source, extract};
use nugget_proactive::{CandidateKind, Proactive};
use nugget_store::{NewItem, Store, content_hash};
use serde_json::json;
use time::macros::datetime;

fn put_ics(store: &Store, key: &str, body: &str) {
    let ics = format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{body}END:VCALENDAR\r\n");
    let ex = extract(
        "text/calendar",
        Source::Bytes(ics.as_bytes()),
        &ExtractOptions::default(),
    )
    .unwrap();
    let mut item = NewItem::new("files:cal", key, content_hash(&ics))
        .with_mime("text/calendar")
        .with_text(ex.text)
        .with_metadata(json!({"extracted": ex.metadata, "sensitive": false}));
    if let Some(t) = ex.title {
        item = item.with_title(t);
    }
    store.upsert_item(&item).unwrap();
}

fn event(uid: &str, extra: &str) -> String {
    format!("BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261001T000000Z\r\n{extra}END:VEVENT\r\n")
}

fn engine(store: Store) -> Proactive {
    Proactive::new(store, cfg())
}

/// Attendees, organizer, location and status are read per VEVENT: an external meeting gets a
/// prep card; cancelled and declined events leave the brief.
#[test]
fn ics_people_and_status() {
    let store = Store::open_in_memory().unwrap();
    let review = "SUMMARY:Northwind checkout review\r\nDTSTART:20261028T140000Z\r\nDTEND:20261028T150000Z\r\nLOCATION:Room 2\r\nORGANIZER;CN=Dana:mailto:dana@northwind.example\r\nATTENDEE;CN=Dana:mailto:dana@northwind.example\r\nATTENDEE;CN=Marcus:mailto:marcus@northwind.example\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:me@nugget.dev\r\n";
    put_ics(&store, "review.ics", &event("nw-1", review));
    let coffee = "SUMMARY:Coffee with Kelsey\r\nDTSTART:20261028T160000Z\r\n";
    put_ics(&store, "coffee.ics", &event("kc-1", coffee));
    put_ics(
        &store,
        "coffee-cancel.ics",
        &format!(
            "METHOD:CANCEL\r\n{}",
            event("kc-1", &format!("{coffee}SEQUENCE:1\r\n"))
        ),
    );
    let declined = "SUMMARY:Vendor pitch\r\nDTSTART:20261028T170000Z\r\nATTENDEE;PARTSTAT=DECLINED:mailto:me@nugget.dev\r\n";
    put_ics(&store, "pitch.ics", &event("vp-1", declined));

    let p = engine(store);
    let prep: Vec<_> = p
        .candidates(now())
        .unwrap()
        .into_iter()
        .filter(|c| c.kind == CandidateKind::MeetingPrep)
        .collect();
    assert_eq!(prep.len(), 1);
    assert!(
        prep[0]
            .people
            .contains(&"marcus@northwind.example".to_owned())
    );
    let brief = p.brief(now()).unwrap();
    let titles: Vec<&str> = brief.meetings.iter().map(|m| m.title.as_str()).collect();
    assert_eq!(titles, ["Northwind checkout review"]);
    assert_eq!(brief.meetings[0].location.as_deref(), Some("Room 2"));
    assert_eq!(brief.meetings[0].item.key.as_str(), "review.ics");
}

/// A moved meeting (same UID, higher SEQUENCE) is listed once, at its new time.
#[test]
fn ics_moved_meeting_once() {
    let store = Store::open_in_memory().unwrap();
    put_ics(
        &store,
        "qa-v0.ics",
        &event("qa-1", "SUMMARY:Design QA\r\nDTSTART:20261028T130000Z\r\n"),
    );
    put_ics(
        &store,
        "qa-v1.ics",
        &event(
            "qa-1",
            "SUMMARY:Design QA\r\nSEQUENCE:1\r\nDTSTART:20261028T150000Z\r\n",
        ),
    );
    let brief = engine(store).brief(now()).unwrap();
    let starts: Vec<_> = brief.meetings.iter().map(|m| m.start).collect();
    assert_eq!(starts, [datetime!(2026-10-28 15:00 UTC)]);
}

/// A weekday RRULE repeats in its own zone, minus EXDATEs and moved instances.
#[test]
fn ics_recurring_standup() {
    let store = Store::open_in_memory().unwrap();
    let standup = "SUMMARY:Standup\r\nDTSTART;TZID=America/New_York:20261005T093000\r\nDTEND;TZID=America/New_York:20261005T094500\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR\r\nEXDATE;TZID=America/New_York:20261029T093000\r\n";
    put_ics(&store, "standup.ics", &event("su-1", standup));
    put_ics(
        &store,
        "standup-move.ics",
        &event(
            "su-1",
            "SUMMARY:Standup\r\nRECURRENCE-ID;TZID=America/New_York:20261030T093000\r\nDTSTART;TZID=America/New_York:20261030T110000\r\n",
        ),
    );
    let p = engine(store);
    let on = |t| {
        p.brief(t)
            .unwrap()
            .meetings
            .iter()
            .map(|m| m.start)
            .collect::<Vec<_>>()
    };
    // EDT until Nov 1, EST after: 09:30 local is 13:30Z, then 14:30Z.
    assert_eq!(on(now()), [datetime!(2026-10-28 13:30 UTC)]);
    assert!(on(datetime!(2026-10-29 09:00 UTC)).is_empty());
    assert_eq!(
        on(datetime!(2026-10-30 09:00 UTC)),
        [datetime!(2026-10-30 15:00 UTC)]
    );
    assert!(on(datetime!(2026-10-31 09:00 UTC)).is_empty());
    assert_eq!(
        on(datetime!(2026-11-02 09:00 UTC)),
        [datetime!(2026-11-02 14:30 UTC)]
    );
}

/// TZID is honoured, whatever the user's offset.
#[test]
fn ics_tzid() {
    let store = Store::open_in_memory().unwrap();
    put_ics(
        &store,
        "flight.ics",
        &event(
            "ba-178",
            "SUMMARY:BA178\r\nDTSTART;TZID=America/New_York:20261028T190000\r\nDTEND;TZID=Europe/London:20261029T070000\r\n",
        ),
    );
    let c = engine(store).corpus().unwrap();
    assert_eq!(c.events[0].start, datetime!(2026-10-28 23:00 UTC));
    assert_eq!(c.events[0].end, Some(datetime!(2026-10-29 07:00 UTC)));
}
