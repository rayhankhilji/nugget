use nugget_connector::Content;
use serde_json::json;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, ResponseTemplate};

use super::*;
use crate::CalendarFactory;

const API: &str = "/calendar/v3";

fn gcal(h: &Harness, settings: Value) -> Arc<dyn Connector> {
    h.build(
        &CalendarFactory::new()
            .with_endpoints(h.endpoints())
            .with_retry(fast_retry(1)),
        settings,
    )
}

fn primary() -> Value {
    json!({"client_id": "cid", "calendars": ["primary"]})
}

fn event(id: &str, summary: &str) -> Value {
    json!({
        "kind": "calendar#event", "id": id, "status": "confirmed",
        "etag": format!("\"etag-{id}\""), "updated": "2025-10-01T12:00:00.000Z",
        "created": "2025-09-01T12:00:00.000Z",
        "htmlLink": format!("https://www.google.com/calendar/event?eid={id}"),
        "summary": summary,
        "description": "Agenda:<br>1. <b>Roadmap</b> &amp; risks",
        "location": "Room 4, Main St",
        "organizer": {"email": "boss@example.com", "displayName": "The Boss"},
        "attendees": [
            {"email": "me@example.com", "self": true, "responseStatus": "accepted"},
            {"email": "bob@example.com", "displayName": "Bob", "responseStatus": "tentative", "optional": true},
            {"email": "room@resource.calendar.google.com", "resource": true, "responseStatus": "accepted"}
        ],
        "start": {"dateTime": "2025-11-03T10:00:00+01:00", "timeZone": "Europe/Paris"},
        "end": {"dateTime": "2025-11-03T11:00:00+01:00", "timeZone": "Europe/Paris"},
        "hangoutLink": "https://meet.google.com/abc-defg-hij",
        "iCalUID": format!("{id}@google.com"),
    })
}

fn cancelled(id: &str) -> Value {
    json!({"id": id, "status": "cancelled"})
}

fn page(items: Vec<Value>, next_page: Option<&str>, sync_token: Option<&str>) -> ResponseTemplate {
    let mut body = json!({
        "kind": "calendar#events", "summary": "Work", "timeZone": "Europe/Paris", "items": items
    });
    if let Some(p) = next_page {
        body["nextPageToken"] = p.into();
    }
    if let Some(s) = sync_token {
        body["nextSyncToken"] = s.into();
    }
    ResponseTemplate::new(200).set_body_json(body)
}

#[tokio::test]
async fn sync_without_tokens_is_unauthorized() {
    let h = Harness::new().await;
    let conn = gcal(&h, primary());
    assert!(is_unauthorized(
        &run(&conn, None, &RecSink::default()).await
    ));
    assert!(h.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn full_sync_pages_window_and_stores_sync_token() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .and(path(format!("{API}/calendars/primary/events")))
        .and(query_param_is_missing("pageToken"))
        .and(query_param_is_missing("syncToken"))
        .and(query_param("singleEvents", "true"))
        .respond_with(page(vec![event("e1", "Planning")], Some("pg2"), None))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/calendars/primary/events")))
        .and(query_param("pageToken", "pg2"))
        .respond_with(page(
            vec![event("e2", "Retro"), cancelled("e0")],
            None,
            Some("st1"),
        ))
        .mount(&h.server)
        .await;
    let conn = gcal(&h, primary());
    let sink = RecSink::default();
    let out = run(&conn, None, &sink).await.unwrap();
    assert_eq!(
        sink.summary(),
        vec!["+primary/e1", "+primary/e2", "-primary/e0"]
    );
    assert_eq!(
        out,
        json!({"v": 1, "calendars": {"primary": {"sync_token": "st1"}}})
    );

    // Mid-calendar checkpoint keeps the page token and the window.
    let cps = sink.checkpoints.lock().unwrap().clone();
    let mid = &cps[0]["calendars"]["primary"];
    assert_eq!(mid["page_token"], "pg2");
    let min = time::OffsetDateTime::parse(
        mid["time_min"].as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let max = time::OffsetDateTime::parse(
        mid["time_max"].as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    assert_eq!((max - min).whole_days(), 730);
    // The second page reuses the same window.
    let reqs = h.server.received_requests().await.unwrap();
    let q0: HashMap<_, _> = reqs[0].url.query_pairs().into_owned().collect();
    let q1: HashMap<_, _> = reqs[1].url.query_pairs().into_owned().collect();
    assert_eq!(q0["timeMin"], q1["timeMin"]);
    assert_eq!(q0["timeMax"], mid["time_max"].as_str().unwrap());

    let doc = &sink.upserts()[0];
    assert_eq!(doc.mime, "text/plain");
    assert_eq!(doc.title.as_deref(), Some("Planning"));
    // `<etag>#<start>:<end>`: the span lets a resync tell old events from deleted ones.
    assert_eq!(
        doc.version.as_deref(),
        Some("\"etag-e1\"#1762160400:1762164000")
    );
    assert_eq!(
        doc.url.as_deref(),
        Some("https://www.google.com/calendar/event?eid=e1")
    );
    let Content::Text(text) = &doc.content else {
        panic!("text content")
    };
    assert!(text.starts_with("Planning\n"), "{text}");
    assert!(
        text.contains("When: 2025-11-03 10:00 – 11:00 (Europe/Paris, UTC+01:00)"),
        "{text}"
    );
    assert!(text.contains("Where: Room 4, Main St"));
    assert!(text.contains("Organizer: The Boss <boss@example.com>"));
    assert!(
        text.contains(
            "Attendees: me@example.com (accepted); Bob <bob@example.com> (maybe, optional)"
        ),
        "{text}"
    );
    assert!(!text.contains("room@resource"));
    assert!(text.contains("Calendar: Work"));
    assert!(text.contains("Agenda:\n1. Roadmap & risks"), "{text}");
    assert_eq!(doc.metadata["calendar_id"], "primary");
    assert_eq!(doc.metadata["event_id"], "e1");
    assert_eq!(doc.metadata["time_zone"], "Europe/Paris");
    assert_eq!(doc.metadata["all_day"], false);
    assert_eq!(doc.metadata["attendees"].as_array().unwrap().len(), 3);
    assert_eq!(doc.metadata["organizer"]["email"], "boss@example.com");
    assert_eq!(doc.modified_at.unwrap().year(), 2025);
    h.assert_egress("sync:gcal").await;
}

#[tokio::test]
async fn incremental_sync_uses_sync_token_and_deletes_cancelled() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .and(path(format!("{API}/calendars/primary/events")))
        .and(query_param("syncToken", "st1"))
        .and(query_param_is_missing("timeMin"))
        .and(query_param_is_missing("timeMax"))
        .respond_with(page(
            vec![event("e1", "Planning (moved)"), cancelled("e2")],
            None,
            Some("st2"),
        ))
        .expect(1)
        .mount(&h.server)
        .await;
    let conn = gcal(&h, primary());
    let sink = RecSink::default();
    let out = run(
        &conn,
        Some(json!({"v": 1, "calendars": {"primary": {"sync_token": "st1"}}})),
        &sink,
    )
    .await
    .unwrap();
    assert_eq!(sink.summary(), vec!["+primary/e1", "-primary/e2"]);
    assert_eq!(out["calendars"]["primary"], json!({"sync_token": "st2"}));
    h.assert_egress("sync:gcal").await;
}

#[tokio::test]
async fn gone_410_triggers_full_resync_and_verifies_missing() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    h.know("primary/e1", "\"etag-e1\"");
    h.know("primary/old", "\"x\""); // outside the window, still exists
    h.know("primary/deleted", "\"y\""); // gone during the gap
    h.know("primary/cxl", "\"z\""); // cancelled during the gap
    Mock::given(method("GET"))
        .and(path(format!("{API}/calendars/primary/events")))
        .and(query_param("syncToken", "stale"))
        .respond_with(ResponseTemplate::new(410).set_body_json(json!({
            "error": {"code": 410, "message": "Sync token is no longer valid, a full sync is required.",
                      "errors": [{"reason": "fullSyncRequired"}]}
        })))
        .expect(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/calendars/primary/events")))
        .and(query_param_is_missing("syncToken"))
        .and(query_param_is_missing("pageToken"))
        .respond_with(page(vec![event("e1", "Planning")], None, Some("st3")))
        .expect(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/calendars/primary/events/old")))
        .respond_with(ResponseTemplate::new(200).set_body_json(event("old", "Old")))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/calendars/primary/events/deleted")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/calendars/primary/events/cxl")))
        .respond_with(ResponseTemplate::new(200).set_body_json(cancelled("cxl")))
        .mount(&h.server)
        .await;

    let conn = gcal(&h, primary());
    let sink = RecSink::default();
    let out = run(
        &conn,
        Some(json!({"v": 1, "calendars": {"primary": {"sync_token": "stale"}}})),
        &sink,
    )
    .await
    .unwrap();
    let mut s = sink.summary();
    assert_eq!(s.remove(0), "+primary/e1");
    s.sort();
    assert_eq!(s, vec!["-primary/cxl", "-primary/deleted"]);
    assert_eq!(out["calendars"]["primary"], json!({"sync_token": "st3"}));
    h.assert_egress("sync:gcal").await;
}

#[tokio::test]
async fn selected_calendars_are_listed_and_dropped_ones_removed() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    h.know("old@group.calendar.google.com/e1", "\"a\"");
    h.know("work#team@group.calendar.google.com/e5", "\"b\"");
    Mock::given(method("GET"))
        .and(path(format!("{API}/users/me/calendarList")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [
                {"id": "me@example.com", "summary": "Me", "selected": true, "primary": true},
                {"id": "work#team@group.calendar.google.com", "summary": "Team", "selected": true},
                {"id": "holidays@group.v.calendar.google.com", "summary": "Holidays"},
                {"id": "bad/id", "selected": true}
            ]
        })))
        .mount(&h.server)
        .await;
    // `#` and `@` in calendar ids are percent-encoded in the path.
    Mock::given(method("GET"))
        .and(path(format!("{API}/calendars/me@example.com/events")))
        .respond_with(page(vec![event("a1", "Mine")], None, Some("s-me")))
        .expect(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "{API}/calendars/work%23team@group.calendar.google.com/events"
        )))
        .respond_with(page(vec![], None, Some("s-team")))
        .expect(1)
        .mount(&h.server)
        .await;

    let conn = gcal(&h, json!({"client_id": "cid"}));
    let sink = RecSink::default();
    let prior = json!({"v": 1, "calendars": {
        "old@group.calendar.google.com": {"sync_token": "x"},
        "work#team@group.calendar.google.com": {"sync_token": "y"}
    }});
    let out = run(&conn, Some(prior), &sink).await.unwrap();
    assert_eq!(
        sink.summary(),
        vec!["-old@group.calendar.google.com/e1", "+me@example.com/a1"]
    );
    let cals = out["calendars"].as_object().unwrap();
    assert_eq!(cals.len(), 2);
    assert_eq!(cals["me@example.com"]["sync_token"], "s-me");
    assert_eq!(
        cals["work#team@group.calendar.google.com"]["sync_token"],
        "s-team"
    );
    h.assert_egress("sync:gcal").await;
}

#[tokio::test]
async fn all_day_events_and_resumed_paging() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .and(path(format!("{API}/calendars/primary/events")))
        .and(query_param("pageToken", "pg9"))
        .and(query_param("timeMin", "2025-01-01T00:00:00Z"))
        .respond_with(page(
            vec![json!({
                "id": "allday", "status": "confirmed", "summary": "Offsite",
                "start": {"date": "2025-11-03"}, "end": {"date": "2025-11-05"},
                "updated": "2025-10-01T00:00:00Z"
            })],
            None,
            Some("st"),
        ))
        .expect(1)
        .mount(&h.server)
        .await;
    let conn = gcal(&h, primary());
    let sink = RecSink::default();
    let cursor = json!({"v": 1, "calendars": {"primary": {
        "time_min": "2025-01-01T00:00:00Z", "time_max": "2026-12-31T00:00:00Z", "page_token": "pg9"
    }}});
    let out = run(&conn, Some(cursor), &sink).await.unwrap();
    assert_eq!(out["calendars"]["primary"]["sync_token"], "st");
    let doc = &sink.upserts()[0];
    let Content::Text(text) = &doc.content else {
        panic!()
    };
    assert!(
        text.contains("When: 2025-11-03 – 2025-11-04 (all day)"),
        "{text}"
    );
    assert_eq!(doc.metadata["all_day"], true);
    // No etag: version falls back to `updated` (all-day dates at UTC midnight).
    assert_eq!(
        doc.version.as_deref(),
        Some("2025-10-01T00:00:00Z#1762128000:1762300800")
    );
}

#[tokio::test]
async fn calendar_rate_limits_are_retried_then_surface() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error": {
            "code": 403, "message": "Rate Limit Exceeded", "errors": [{"reason": "rateLimitExceeded"}]
        }})))
        .expect(2)
        .mount(&h.server)
        .await;
    let conn = gcal(&h, primary());
    let err = run(&conn, None, &RecSink::default()).await.unwrap_err();
    assert!(matches!(err, Error::RateLimited { .. }), "{err}");
    h.assert_egress("sync:gcal").await;
}

#[tokio::test]
async fn hostile_event_payloads_do_not_panic() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .and(path(format!("{API}/calendars/primary/events")))
        .respond_with(page(
            vec![
                json!({"id": "x1", "start": {"dateTime": "not a time"}, "end": {"date": "???"},
                       "attendees": [{}], "organizer": {}, "description": "<<<&&&"}),
                json!({"id": "../../etc", "summary": "bad id"}),
                json!({"id": "", "summary": "empty id"}),
                json!({"id": "x2", "summary": "a".repeat(100_000), "description": "b".repeat(200_000)}),
            ],
            None,
            Some("st"),
        ))
        .mount(&h.server)
        .await;
    let conn = gcal(&h, primary());
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+primary/x1", "+primary/x2"]);
    let docs = sink.upserts();
    let Content::Text(t) = &docs[1].content else {
        panic!()
    };
    assert!(t.len() < 30_000);

    let h2 = Harness::new().await;
    h2.seed_tokens("at", false);
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{\"items\": 5}"))
        .mount(&h2.server)
        .await;
    let conn = gcal(&h2, primary());
    assert!(matches!(
        run(&conn, None, &RecSink::default()).await,
        Err(Error::Invalid(_))
    ));
}

fn span_version(start: i64, end: i64) -> String {
    format!("\"etag\"#{start}:{end}")
}

fn now_secs() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[tokio::test]
async fn a_reset_cursor_drops_calendars_found_in_indexed_keys_and_reconciles() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let now = now_secs();
    let ancient = now - 2000 * 86_400;
    h.know("dropped@x/e1", &span_version(now, now + 60)); // calendar no longer selected
    h.know("primary/e1", &span_version(now, now + 60)); // listed again
    h.know("primary/stale", &span_version(now, now + 60)); // gone
    h.know("primary/oldacct", &span_version(ancient, ancient + 60)); // other account, ancient
    Mock::given(method("GET"))
        .and(path(format!("{API}/calendars/primary/events")))
        .and(query_param_is_missing("syncToken"))
        .respond_with(page(vec![event("e1", "Planning")], None, Some("st")))
        .expect(1)
        .mount(&h.server)
        .await;
    // In the window: checked. Outside it: out of scope, deleted without a request.
    for (id, n) in [("stale", 1), ("oldacct", 0)] {
        Mock::given(method("GET"))
            .and(path(format!("{API}/calendars/primary/events/{id}")))
            .respond_with(ResponseTemplate::new(404))
            .expect(n)
            .mount(&h.server)
            .await;
    }
    let conn = gcal(&h, primary());
    let sink = RecSink::default();
    // A settings update reset the cursor.
    let out = run(&conn, None, &sink).await.unwrap();
    let mut s = sink.summary();
    s.sort();
    assert_eq!(
        s,
        vec![
            "+primary/e1",
            "-dropped@x/e1",
            "-primary/oldacct",
            "-primary/stale"
        ]
    );
    assert_eq!(
        out,
        json!({"v": 1, "calendars": {"primary": {"sync_token": "st"}}})
    );
    let cps = sink.checkpoints.lock().unwrap().clone();
    assert!(
        cps.iter()
            .all(|c| c["calendars"].get("dropped@x").is_none())
    );
}

#[tokio::test]
async fn resync_verifies_in_window_events_first_and_carries_the_rest() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let now = now_secs();
    let ancient = now - 2000 * 86_400;
    for i in 0..600 {
        h.know(&format!("primary/g{i:04}"), &span_version(now, now + 60));
    }
    for i in 0..30 {
        h.know(
            &format!("primary/a{i:04}"),
            &span_version(ancient, ancient + 60),
        );
    }
    Mock::given(method("GET"))
        .and(path(format!("{API}/calendars/primary/events")))
        .and(query_param("syncToken", "stale"))
        .respond_with(ResponseTemplate::new(410))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/calendars/primary/events")))
        .and(query_param_is_missing("syncToken"))
        .respond_with(page(vec![], None, Some("st1")))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/calendars/primary/events")))
        .and(query_param("syncToken", "st1"))
        .respond_with(page(vec![], None, Some("st2")))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(format!(
            "^{API}/calendars/primary/events/g[0-9]+$"
        )))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.server)
        .await;
    // Outside the window: expected to be missing from the listing, never checked.
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(format!(
            "^{API}/calendars/primary/events/a[0-9]+$"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(event("a0000", "Old")))
        .expect(0)
        .mount(&h.server)
        .await;

    let conn = gcal(&h, primary());
    let sink = RecSink::default();
    let out = run(
        &conn,
        Some(json!({"v": 1, "calendars": {"primary": {"sync_token": "stale"}}})),
        &sink,
    )
    .await
    .unwrap();
    assert_eq!(sink.summary().len(), 500);
    let state = &out["calendars"]["primary"];
    assert_eq!(state["sync_token"], "st1");
    assert_eq!(state["verify"].as_array().unwrap().len(), 100);

    // The next incremental pass checks the rest.
    let sink = RecSink::default();
    let out = run(&conn, Some(out), &sink).await.unwrap();
    assert_eq!(sink.summary().len(), 100);
    assert_eq!(
        out,
        json!({"v": 1, "calendars": {"primary": {"sync_token": "st2"}}})
    );
}

#[tokio::test]
async fn an_interrupted_resync_lists_again_instead_of_taking_listed_events_for_missing() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let now = now_secs();
    h.know("primary/e1", &span_version(now, now + 60)); // listed on page 1 before the crash
    Mock::given(method("GET"))
        .and(path(format!("{API}/calendars/primary/events")))
        .and(query_param_is_missing("pageToken"))
        .and(query_param("timeMin", "2025-01-01T00:00:00Z"))
        .respond_with(page(vec![event("e1", "Planning")], Some("pg2"), None))
        .expect(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/calendars/primary/events")))
        .and(query_param("pageToken", "pg2"))
        .respond_with(page(vec![event("e2", "Retro")], None, Some("st")))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/calendars/primary/events/e1")))
        .respond_with(ResponseTemplate::new(404))
        .expect(0)
        .mount(&h.server)
        .await;
    let conn = gcal(&h, primary());
    let sink = RecSink::default();
    let cursor = json!({"v": 1, "calendars": {"primary": {
        "time_min": "2025-01-01T00:00:00Z", "time_max": "2030-01-01T00:00:00Z",
        "page_token": "pg2", "resync": true
    }}});
    let out = run(&conn, Some(cursor), &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+primary/e1", "+primary/e2"]);
    assert_eq!(out["calendars"]["primary"], json!({"sync_token": "st"}));
}
