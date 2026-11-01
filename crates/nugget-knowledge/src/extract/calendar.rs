//! Calendar events: `text/calendar` items (`metadata.extracted.events`, as the ics extractor writes
//! them) and Google Calendar items (`metadata.source` with `event_id` / `attendees`).

use nugget_graph::EntityKind;
use nugget_store::Chunk;
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::{ItemCtx, parse_when};
use crate::spec::{ClaimSpec, ERef, EntitySpec, Extraction, OSpec};
use crate::text::{Address, MAX_QUOTE_CHARS, is_automated, parse_address, truncate_chars};

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
    // A recurring event says how it repeats, not just its first date.
    let when = match (ev.rrule.as_deref(), ev.start.as_deref()) {
        (Some(r), Some(st)) => format!(" ({})", recurrence(r, st)),
        (None, Some(st)) => format!(" on {st}"),
        (Some(r), None) => format!(" ({})", recurrence(r, "")),
        (None, None) => String::new(),
    };
    let happened = has_ended(&ev, start, ctx.now);

    // A dial-in number or an address is not a place (security review 3, ER-7).
    if let Some(loc) = ev
        .location
        .as_deref()
        .filter(|l| !is_virtual(l) && crate::llm::values::sensitive_value(l).is_none())
    {
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
        let evidence = match locate_line(ctx.chunks, "organizer", &anchor) {
            Some((id, line)) => ctx.evidence(Some(id), &line),
            None => ctx.evidence_at(&anchor, &format!("Organizer: {raw}")),
        };
        ex.claim(ClaimSpec {
            subject: r,
            predicate: "organized".into(),
            object: OSpec::Entity(event),
            text: format!("{who} organized {summary}{when}."),
            confidence: 0.95,
            evidence,
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
        // Accepting is not attending.
        let (predicate, verb, confidence) = match status {
            "declined" => ("invited_to", "was invited to (declined)", 0.9),
            "accepted" if happened => ("attended", "attended", 0.85),
            "accepted" => ("invited_to", "accepted the invitation to", 0.9),
            "" if organizer && happened => ("attended", "attended", 0.85),
            "" if organizer => ("invited_to", "plans to attend", 0.9),
            _ => ("invited_to", "was invited to", 0.9),
        };
        if seen.contains(&r) && !organizer {
            continue;
        }
        seen.push(r);
        let who = ex.name_of(r, ctx.me_label);
        let anchor = a.email.clone().unwrap_or_else(|| raw.clone());
        // Quote the attendee line, not the first line naming the address (the organizer's).
        let evidence = match locate_line(ctx.chunks, "attendee", &anchor) {
            Some((id, line)) => ctx.evidence(Some(id), &line),
            None => ctx.evidence(None, &format!("Attendee: {raw}")),
        };
        ex.claim(ClaimSpec {
            subject: r,
            predicate: predicate.into(),
            object: OSpec::Entity(event),
            text: format!("{who} {verb} {summary}{when}."),
            confidence,
            evidence,
            valid_from: start,
        });
    }
}

/// Whether the event has ended by `now`.
fn has_ended(ev: &Event, start: Option<OffsetDateTime>, now: OffsetDateTime) -> bool {
    let end = ev.end.as_deref().and_then(parse_when).or_else(|| {
        start.map(|s| {
            if ev.all_day == Some(true) {
                s + time::Duration::days(1)
            } else {
                s
            }
        })
    });
    end.is_some_and(|e| e <= now)
}

/// How a recurring event repeats, in words, from its `RRULE` and first start.
fn recurrence(rrule: &str, start: &str) -> String {
    let mut freq = None;
    let mut interval = 1u32;
    let mut count = None;
    let mut until = None;
    for part in rrule.split(';') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => freq = Some(v.trim().to_ascii_uppercase()),
            "INTERVAL" => interval = v.trim().parse().unwrap_or(1).max(1),
            "COUNT" => count = v.trim().parse::<u32>().ok(),
            "UNTIL" => until = parse_when(v.trim()),
            _ => {}
        }
    }
    let unit = match freq.as_deref() {
        Some("DAILY") => Some(("daily", "days")),
        Some("WEEKLY") => Some(("weekly", "weeks")),
        Some("MONTHLY") => Some(("monthly", "months")),
        Some("YEARLY") => Some(("yearly", "years")),
        _ => None,
    };
    let mut out = match unit {
        Some((once, _)) if interval == 1 => once.to_owned(),
        Some((_, many)) => format!("every {interval} {many}"),
        None => "repeats".to_owned(),
    };
    if !start.is_empty() {
        out.push_str(" from ");
        out.push_str(start.get(..10).unwrap_or(start));
    }
    if let Some(u) = until {
        out.push_str(&format!(" until {}", u.date()));
    }
    if let Some(n) = count {
        out.push_str(&format!(", {n} times"));
    }
    out
}

/// The first line (in chunk order) that starts with `prefix` and contains `needle`, both
/// case-insensitive, with its chunk id.
fn locate_line(chunks: &[Chunk], prefix: &str, needle: &str) -> Option<(i64, String)> {
    let n = needle.trim().to_lowercase();
    if n.is_empty() {
        return None;
    }
    chunks.iter().find_map(|c| {
        c.text.lines().find_map(|l| {
            let lower = l.trim_start().to_lowercase();
            (lower.starts_with(prefix) && lower.contains(&n))
                .then(|| (c.id, truncate_chars(l.trim(), MAX_QUOTE_CHARS)))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::recurrence;

    #[test]
    fn recurrences_read_as_words() {
        assert_eq!(
            recurrence("FREQ=WEEKLY;BYDAY=TU,WE,TH", "2026-03-10T09:30:00Z"),
            "weekly from 2026-03-10"
        );
        assert_eq!(
            recurrence("FREQ=WEEKLY;COUNT=8", "2026-06-17T17:00:00Z"),
            "weekly from 2026-06-17, 8 times"
        );
        assert_eq!(
            recurrence(
                "FREQ=MONTHLY;INTERVAL=2;UNTIL=20261231T000000Z",
                "2026-01-05"
            ),
            "every 2 months from 2026-01-05 until 2026-12-31"
        );
        assert_eq!(recurrence("X-UNKNOWN", ""), "repeats");
    }
}
