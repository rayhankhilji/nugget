use nugget_connector::Content;
use serde_json::json;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, ResponseTemplate};

use super::*;
use crate::OutlookCalendarFactory;

const DELTA: &str = "/v1.0/me/calendarView/delta";

fn calendar(h: &Harness, settings: Value) -> Arc<dyn Connector> {
    h.build(
        &OutlookCalendarFactory::new()
            .with_endpoints(h.endpoints())
            .with_retry(fast_retry(0)),
        settings,
    )
}

fn link(h: &Harness, q: &str) -> String {
    format!("{}{DELTA}?{q}", h.uri())
}

fn page(
    h: &Harness,
    items: Vec<Value>,
    next: Option<&str>,
    delta: Option<&str>,
) -> ResponseTemplate {
    let mut body = json!({"value": items});
    if let Some(n) = next {
        body["@odata.nextLink"] = link(h, n).into();
    }
    if let Some(d) = delta {
        body["@odata.deltaLink"] = link(h, d).into();
    }
    ResponseTemplate::new(200).set_body_json(body)
}

fn event(id: &str, subject: &str) -> Value {
    json!({
        "id": id, "changeKey": format!("ck-{id}"), "subject": subject,
        "start": {"dateTime": "2026-11-03T18:00:00.0000000", "timeZone": "UTC"},
        "end": {"dateTime": "2026-11-03T19:30:00.0000000", "timeZone": "UTC"},
        "originalStartTimeZone": "Pacific Standard Time",
        "isAllDay": false, "isCancelled": false,
        "lastModifiedDateTime": "2026-10-20T08:00:00Z",
        "webLink": format!("https://outlook.office365.com/owa/?itemid={id}"),
    })
}

async fn mount_initial(h: &Harness, resp: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(DELTA))
        .and(query_param_is_missing("$skiptoken"))
        .and(query_param_is_missing("$deltatoken"))
        .respond_with(resp)
        .mount(&h.server)
        .await;
}

async fn mount_token(h: &Harness, name: &str, value: &str, resp: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(DELTA))
        .and(query_param(name, value))
        .respond_with(resp)
        .mount(&h.server)
        .await;
}

#[tokio::test]
async fn initial_listing_renders_events_and_stores_delta_link() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let mut rich = event("e1", "Quarterly planning");
    rich["body"] = json!({"contentType": "html", "content":
        "<html><head><style>p{}</style></head><body><p>Agenda:</p><ul><li>Budget &amp; hiring</li></ul></body></html>"});
    rich["location"] = json!({"displayName": "Room 4"});
    rich["locations"] = json!([{"displayName": "Room 4"}, {"displayName": "Online"}]);
    rich["organizer"] = json!({"emailAddress": {"name": "Ana", "address": "ana@contoso.com"}});
    rich["attendees"] = json!([
        {"type": "required", "status": {"response": "accepted"}, "emailAddress": {"name": "Bo", "address": "bo@contoso.com"}},
        {"type": "optional", "status": {"response": "tentativelyAccepted"}, "emailAddress": {"address": "cy@contoso.com"}},
        {"type": "required", "status": {"response": "none"}, "emailAddress": {"name": "Di", "address": "di@contoso.com"}},
        {"type": "resource", "status": {"response": "accepted"}, "emailAddress": {"name": "Room 4", "address": "room4@contoso.com"}}
    ]);
    rich["onlineMeeting"] = json!({"joinUrl": "https://teams.microsoft.com/l/meetup-join/abc"});
    rich["type"] = json!("occurrence");
    rich["seriesMasterId"] = json!("master1");
    mount_initial(
        &h,
        page(
            &h,
            vec![rich, event("e2", "Standup")],
            Some("$skiptoken=p2"),
            None,
        ),
    )
    .await;
    mount_token(
        &h,
        "$skiptoken",
        "p2",
        page(
            &h,
            vec![
                json!({"id": "gone", "@removed": {"reason": "deleted"}}),
                json!({"id": "cxl", "isCancelled": true, "subject": "Cancelled"}),
                json!({"id": "allday", "changeKey": "x", "subject": "Offsite", "isAllDay": true,
               "start": {"dateTime": "2026-11-10T00:00:00.0000000", "timeZone": "UTC"},
               "end": {"dateTime": "2026-11-12T00:00:00.0000000", "timeZone": "UTC"}}),
            ],
            None,
            Some("$deltatoken=d1"),
        ),
    )
    .await;

    let conn = calendar(&h, json!({"client_id": "cid"}));
    let sink = RecSink::default();
    let cursor = run(&conn, None, &sink).await.unwrap();
    assert_eq!(
        sink.summary(),
        vec!["+e1", "+e2", "-gone", "-cxl", "+allday"]
    );
    assert_eq!(cursor["delta_link"], link(&h, "$deltatoken=d1"));
    assert!(cursor.get("full").is_none() && cursor.get("next_link").is_none());
    let start = cursor["start"].as_i64().unwrap();
    let end = cursor["end"].as_i64().unwrap();
    assert!((now() - 365 * 86_400 - start).abs() < 60);
    assert!((now() + 365 * 86_400 - end).abs() < 60);
    let cps = sink.checkpoints.lock().unwrap().clone();
    assert_eq!(cps[0]["next_link"], link(&h, "$skiptoken=p2"));
    assert_eq!(cps[0]["full"], true);

    let first = h.requests_to(DELTA).await.remove(0);
    let q: HashMap<String, String> = first.url.query_pairs().into_owned().collect();
    assert_eq!(q["startDateTime"], rfc3339(start));
    assert_eq!(q["endDateTime"], rfc3339(end));
    let p = prefer(&first);
    assert!(p.contains(&"outlook.timezone=\"UTC\"".to_string()), "{p:?}");
    assert!(p.contains(&"IdType=\"ImmutableId\"".to_string()));

    let docs = sink.upserts();
    let d = &docs[0];
    assert_eq!(d.mime, "text/plain");
    assert_eq!(d.title.as_deref(), Some("Quarterly planning"));
    let Content::Text(text) = &d.content else {
        panic!("text content")
    };
    assert_eq!(
        text,
        "Quarterly planning\n\
         When: 2026-11-03 10:00 – 11:30 (Pacific Standard Time, UTC-08:00)\n\
         Where: Room 4; Online\n\
         Organizer: Ana <ana@contoso.com>\n\
         Attendees: Bo <bo@contoso.com> (accepted); cy@contoso.com (maybe, optional); Di <di@contoso.com> (no response)\n\
         Online meeting: https://teams.microsoft.com/l/meetup-join/abc\n\
         \nAgenda:\n\nBudget & hiring\n"
    );
    let span_start = 1_793_728_800; // 2026-11-03T18:00:00Z
    assert_eq!(
        d.version.as_deref(),
        Some(format!("ck-e1#{span_start}:{}", span_start + 5400).as_str())
    );
    assert_eq!(
        d.url.as_deref(),
        Some("https://outlook.office365.com/owa/?itemid=e1")
    );
    assert_eq!(d.metadata["series_master_id"], "master1");
    assert_eq!(d.metadata["type"], "occurrence");
    assert_eq!(d.metadata["attendees"][0]["response"], "accepted");
    let Content::Text(allday) = &docs[2].content else {
        panic!()
    };
    assert!(
        allday.contains("When: 2026-11-10 – 2026-11-11 (all day)"),
        "{allday}"
    );
    h.assert_egress("sync:outlook-calendar").await;
}

#[tokio::test]
async fn delta_round_follows_the_delta_link() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_token(
        &h,
        "$deltatoken",
        "d1",
        page(
            &h,
            vec![
                event("e3", "New"),
                json!({"id": "e1", "@removed": {"reason": "deleted"}}),
            ],
            None,
            Some("$deltatoken=d2"),
        ),
    )
    .await;
    let conn = calendar(&h, json!({"client_id": "cid"}));
    let sink = RecSink::default();
    let start = now() - 365 * 86_400;
    let prior =
        json!({"v": 1, "start": start, "end": now(), "delta_link": link(&h, "$deltatoken=d1")});
    let out = run(&conn, Some(prior), &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+e3", "-e1"]);
    assert_eq!(out["delta_link"], link(&h, "$deltatoken=d2"));
    assert_eq!(out["start"], start);
    assert_eq!(h.requests_to(DELTA).await.len(), 1);
    h.assert_egress("sync:outlook-calendar").await;
}

#[tokio::test]
async fn expired_delta_token_resyncs_and_verifies_missing() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let soon = now() + 86_400;
    let old = now() - 900 * 86_400;
    h.know("kept", &format!("ck#{soon}:{}", soon + 60));
    h.know("deleted", &format!("ck#{soon}:{}", soon + 60));
    h.know("cancelled", &format!("ck#{soon}:{}", soon + 60));
    h.know("ancient", &format!("ck#{old}:{}", old + 60)); // outside the window: stays
    mount_token(
        &h,
        "$deltatoken",
        "stale",
        ResponseTemplate::new(410).set_body_json(json!({
            "error": {"code": "resyncRequired", "message": "Resync required."}
        })),
    )
    .await;
    mount_initial(
        &h,
        page(&h, vec![event("e9", "Fresh")], None, Some("$deltatoken=d3")),
    )
    .await;
    for (id, resp) in [
        (
            "kept",
            ResponseTemplate::new(200).set_body_json(json!({"id": "kept", "isCancelled": false})),
        ),
        ("deleted", ResponseTemplate::new(404)),
        (
            "cancelled",
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "cancelled", "isCancelled": true})),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("{API}/me/events/{id}")))
            .respond_with(resp)
            .expect(1)
            .mount(&h.server)
            .await;
    }
    let conn = calendar(&h, json!({"client_id": "cid"}));
    let sink = RecSink::default();
    let prior = json!({"v": 1, "start": now() - 365 * 86_400, "end": now(), "delta_link": link(&h, "$deltatoken=stale")});
    let out = run(&conn, Some(prior), &sink).await.unwrap();
    let mut s = sink.summary();
    assert_eq!(s.remove(0), "+e9");
    s.sort();
    assert_eq!(s, vec!["-cancelled", "-deleted"]);
    assert_eq!(out["delta_link"], link(&h, "$deltatoken=d3"));
    h.assert_egress("sync:outlook-calendar").await;
}

#[tokio::test]
async fn reset_cursor_reconciles_against_the_new_window() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let past = now() - 100 * 86_400;
    h.know("e1", &format!("ck#{past}:{}", past + 60)); // before the new 30-day window
    mount_initial(&h, page(&h, vec![], None, Some("$deltatoken=d"))).await;
    let conn = calendar(
        &h,
        json!({"client_id": "cid", "past_days": 30, "future_days": 30}),
    );
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["-e1"]);
    // Deleted without a check.
    assert!(
        h.requests_to(&format!("{API}/me/events/e1"))
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn a_slid_window_is_relisted_without_dropping_old_events() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let past = now() - 380 * 86_400;
    h.know("old", &format!("ck#{past}:{}", past + 60));
    mount_initial(
        &h,
        page(
            &h,
            vec![event("e1", "In window")],
            None,
            Some("$deltatoken=new"),
        ),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(DELTA))
        .and(query_param("$deltatoken", "aged"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&h.server)
        .await;
    let conn = calendar(&h, json!({"client_id": "cid"}));
    let sink = RecSink::default();
    let prior = json!({"v": 1, "start": now() - 400 * 86_400, "end": now() - 30 * 86_400,
                       "delta_link": link(&h, "$deltatoken=aged")});
    let out = run(&conn, Some(prior), &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+e1"]);
    assert_eq!(out["delta_link"], link(&h, "$deltatoken=new"));
    assert!((out["start"].as_i64().unwrap() - (now() - 365 * 86_400)).abs() < 60);
}

#[tokio::test]
async fn hostile_events_are_contained() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let huge = "<p>x</p>".repeat(200_000);
    mount_initial(&h, page(&h, vec![
        json!([1, 2]),
        json!({"id": "a/b", "subject": "path"}),
        json!({"id": "nostart", "subject": "No times"}),
        json!({"id": "badtz", "subject": "Weird", "start": {"dateTime": "2026-01-01T10:00:00", "timeZone": "../../etc/passwd"},
               "originalStartTimeZone": "Mars Standard Time", "attendees": "nope"}),
        json!({"id": "big", "subject": "x".repeat(5000), "body": {"contentType": "html", "content": huge},
               "webLink": "http://insecure.example", "onlineMeetingUrl": "javascript:alert(1)",
               "start": {"dateTime": "garbage", "timeZone": "UTC"}}),
    ], None, Some("$deltatoken=d"))).await;
    let conn = calendar(&h, json!({"client_id": "cid"}));
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+nostart", "+big"]);
    let big = &sink.upserts()[1];
    assert_eq!(big.title.as_ref().unwrap().chars().count(), 500);
    assert!(big.url.is_none());
    let Content::Text(t) = &big.content else {
        panic!()
    };
    assert!(t.chars().count() < 25_000);
    assert!(!t.contains("javascript"));
}
