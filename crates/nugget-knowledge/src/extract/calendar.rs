//! Calendar events: `text/calendar` items (`metadata.extracted.events`, as the ics extractor
//! writes them) and Google Calendar items (`metadata.source` with `event_id` / `attendees`).
//! An event entity with time attributes, attendees as people with `attended` / `invited_to`,
//! the organiser with `organized`, and the location as a place.

use nugget_graph::EntityKind;
use serde_json::{Value, json};

use super::{ItemCtx, parse_when};
use crate::spec::{ClaimSpec, ERef, EntitySpec, Extraction, OSpec};
use crate::text::{Address, is_automated, parse_address, truncate_chars};

/// Events per item at most.
const MAX_EVENTS: usize = 200;
/// Attendees per event at most.
const MAX_ATTENDEES: usize = 200;

/// One event in a source-neutral shape.
#[derive(Default)]
struct Event {
    summary: Option<String>,
    uid: Option<String>,
    start: Option<String>,
    end: Option<String>,
    all_day: Option<bool>,
    timezone: Option<String>,
    rrule: Option<String>,
    status: Option<String>,
    location: Option<String>,
    calendar: Option<String>,
    organizer: Option<(Address, String)>,
    attendees: Vec<(Address, String)>,
}

fn s(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_owned)
}

fn from_ics(ctx: &ItemCtx<'_>) -> Vec<Event> {
    let Some(Value::Array(events)) = ctx.extracted("events") else {
        return Vec::new();
    };
    let calendar = s(ctx.extracted("calendar_name")).or_else(|| s(ctx.source_meta("calendar")));
    events
        .iter()
        .take(MAX_EVENTS)
        .filter(|e| e.get("kind").and_then(Value::as_str) != Some("task"))
        .map(|e| {
            let uid = s(e.get("uid")).map(|u| match s(e.get("recurrence_id")) {
                Some(r) => format!("{u}#{r}"),
                None => u,
            });
            Event {
                summary: s(e.get("summary")),
                uid,
                start: s(e.get("start")),
                end: s(e.get("end")),
                all_day: e.get("all_day").and_then(Value::as_bool),
                timezone: s(e.get("timezone")),
                rrule: s(e.get("rrule")),
                status: s(e.get("status")).map(|x| x.to_lowercase()),
                location: s(e.get("location")),
                calendar: calendar.clone(),
                organizer: s(e.get("organizer")).map(|o| (parse_address(&o), o)),
                attendees: super::strings(e.get("attendees"))
                    .into_iter()
                    .take(MAX_ATTENDEES)
                    .map(|a| (parse_address(&a), a))
                    .collect(),
            }
        })
        .collect()
}

fn gcal_person(v: &Value) -> Option<(Address, String)> {
    let email = s(v.get("email")).map(|e| e.to_lowercase());
    let name = s(v.get("name")).and_then(|n| crate::text::clean_person_name(&n));
    if email.is_none() && name.is_none() {
        return None;
    }
    let raw = match (&name, &email) {
        (Some(n), Some(e)) => format!("{n} <{e}>"),
        (Some(n), None) => n.clone(),
        (None, Some(e)) => e.clone(),
        (None, None) => String::new(),
    };
    let status = s(v.get("response")).map(|r| match r.as_str() {
        "needsAction" => "needs action".to_owned(),
        other => other.to_lowercase(),
    });
    Some((
        Address {
            name,
            email: email.filter(|e| crate::text::is_email(e)),
            status,
        },
        raw,
    ))
}

fn from_gcal(ctx: &ItemCtx<'_>) -> Vec<Event> {
    let Some(src) = ctx.meta("source") else {
        return Vec::new();
    };
    if src.get("event_id").is_none() && src.get("ical_uid").is_none() {
        return Vec::new();
    }
    let when = |k: &str| {
        src.get(k)
            .and_then(|t| s(t.get("date_time")).or_else(|| s(t.get("date"))))
    };
    let uid = s(src.get("ical_uid")).map(|u| match s(src.get("recurring_event_id")) {
        Some(_) => format!("{u}#{}", when("start").unwrap_or_default()),
        None => u,
    });
    let uid = uid.or_else(|| s(src.get("event_id")).map(|e| format!("gcal:{e}")));
    vec![Event {
        summary: ctx.item.title.clone(),
        uid,
        start: when("start"),
        end: when("end"),
        all_day: src.get("all_day").and_then(Value::as_bool),
        timezone: s(src.get("time_zone")),
        rrule: None,
        status: s(src.get("status")).map(|x| x.to_lowercase()),
        location: s(src.get("location")),
        calendar: s(src.get("calendar")),
        organizer: src.get("organizer").and_then(gcal_person),
        attendees: src
            .get("attendees")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter(|x| x.get("resource").and_then(Value::as_bool) != Some(true))
                    .take(MAX_ATTENDEES)
                    .filter_map(gcal_person)
                    .collect()
            })
            .unwrap_or_default(),
    }]
}

/// Locations that are links or call placeholders rather than places.
fn is_virtual(loc: &str) -> bool {
    let l = loc.to_lowercase();
    l.contains("://")
        || l.starts_with("www.")
        || [
            "zoom",
            "google meet",
            "microsoft teams",
            "teams meeting",
            "webex",
            "skype",
            "hangout",
            "online",
            "virtual",
            "phone call",
            "tbd",
            "tba",
        ]
        .iter()
        .any(|k| l == *k || l.starts_with(&format!("{k} ")) || l.contains(&format!("{k} meeting")))
}

pub(super) fn extract(ctx: &ItemCtx<'_>, ex: &mut Extraction) {
    let mut events = from_ics(ctx);
    if events.is_empty() {
        events = from_gcal(ctx);
    }
    for ev in events {
        if ev.status.as_deref() == Some("cancelled") {
            continue;
        }
        one(ctx, ex, ev);
    }
}

fn person_ref(ctx: &ItemCtx<'_>, ex: &mut Extraction, a: &Address) -> Option<ERef> {
    if let Some(e) = &a.email {
        if ctx.is_me(e, &ex.me_emails.clone()) {
            return Some(ERef::Me);
        }
        if is_automated(e) {
            return None;
        }
    }
    let (name, weak) = match (&a.name, &a.email) {
        (Some(n), _) => (n.clone(), false),
        (None, Some(e)) => (e.clone(), true),
        (None, None) => return None,
    };
    let anchor = a.email.clone().unwrap_or_else(|| name.clone());
    let chunk = crate::text::locate(ctx.chunks, &anchor).map(|(id, _)| id);
    let mut spec = EntitySpec::new(EntityKind::Person, name.clone()).mention(&name, chunk, 0.9);
    if let Some(e) = &a.email {
        spec = spec.ident("email", e);
    }
    spec.weak_name = weak;
    Some(ex.entity(spec))
}

fn one(ctx: &ItemCtx<'_>, ex: &mut Extraction, ev: Event) {
    let summary = ev
        .summary
        .clone()
        .map(|x| truncate_chars(&x, 200))
        .unwrap_or_else(|| "Untitled event".into());
    let summary_chunk = crate::text::locate(ctx.chunks, &summary).map(|(id, _)| id);
    let mut spec = EntitySpec::new(EntityKind::Event, summary.clone())
        .mention(&summary, summary_chunk, 0.95)
        .attr("start", json!(ev.start))
        .attr("end", json!(ev.end))
        .attr("all_day", json!(ev.all_day))
        .attr("timezone", json!(ev.timezone))
        .attr("rrule", json!(ev.rrule))
        .attr("status", json!(ev.status))
        .attr("location", json!(ev.location))
        .attr("calendar", json!(ev.calendar));
    if let Some(uid) = &ev.uid {
        spec = spec.ident("ical_uid", uid);
    }
    let event = ex.entity(spec);
    let start = ev.start.as_deref().and_then(parse_when);
    let when = ev
        .start
        .as_deref()
        .map(|x| format!(" on {x}"))
        .unwrap_or_default();

    if let Some(loc) = ev.location.as_deref().filter(|l| !is_virtual(l)) {
        let loc = truncate_chars(loc, 200);
        let place = ex.entity(EntitySpec::new(EntityKind::Place, loc.clone()).mention(
            &loc,
            summary_chunk,
            0.8,
        ));
        ex.claim(ClaimSpec {
            subject: event,
            predicate: "located_at".into(),
            object: OSpec::Entity(place),
            text: format!("{summary} takes place at {loc}."),
            confidence: 0.9,
            evidence: ctx.evidence_at(&loc, &format!("Location: {loc}")),
            valid_from: None,
        });
    }

    let mut seen: Vec<ERef> = Vec::new();
    if let Some((a, raw)) = &ev.organizer
        && let Some(r) = person_ref(ctx, ex, a)
    {
        let who = ex.name_of(r, ctx.me_label);
        let anchor = a.email.clone().unwrap_or_else(|| raw.clone());
        ex.claim(ClaimSpec {
            subject: r,
            predicate: "organized".into(),
            object: OSpec::Entity(event),
            text: format!("{who} organized {summary}{when}."),
            confidence: 0.95,
            evidence: ctx.evidence_at(&anchor, &format!("Organizer: {raw}")),
            valid_from: start,
        });
        seen.push(r);
    }
    for (a, raw) in &ev.attendees {
        let Some(r) = person_ref(ctx, ex, a) else {
            continue;
        };
        let organizer = seen.first() == Some(&r);
        let status = a.status.as_deref().unwrap_or("");
        let (predicate, verb, confidence) = match status {
            "declined" => ("invited_to", "was invited to (declined)", 0.9),
            "accepted" => ("attended", "attended", 0.85),
            "" if organizer => ("attended", "attended", 0.85),
            _ => ("invited_to", "was invited to", 0.9),
        };
        if seen.contains(&r) && !organizer {
            continue;
        }
        seen.push(r);
        let who = ex.name_of(r, ctx.me_label);
        let anchor = a.email.clone().unwrap_or_else(|| raw.clone());
        ex.claim(ClaimSpec {
            subject: r,
            predicate: predicate.into(),
            object: OSpec::Entity(event),
            text: format!("{who} {verb} {summary}{when}."),
            confidence,
            evidence: ctx.evidence_at(&anchor, &format!("Attendee: {raw}")),
            valid_from: start,
        });
    }
}
