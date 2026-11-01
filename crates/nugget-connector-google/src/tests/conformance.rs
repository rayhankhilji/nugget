//! Conformance against a mock server: crash resumption, carried failures and fixed regressions
//! for `gmail`, `gcal` and `gdrive`.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use nugget_connector::testkit::{
    self, Replica, ReplicaSink, Sequence, assert_crash_resumable, assert_scrubbed, big_text,
    run_pass, scrub_json, server_error, too_many_requests,
};
use nugget_connector::{Change, Content, Document};
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use wiremock::matchers::{method, path, path_regex, query_param, query_param_is_missing};
use wiremock::{Mock, Request, ResponseTemplate};

use super::*;
use crate::{CalendarFactory, DriveFactory, GmailFactory};

/// A context whose known items are `known` (a [`Replica`], so reconciling passes see what was
/// really applied).
fn ctx_with(h: &Harness, known: Arc<dyn KnownItems>) -> ConnectorContext {
    testkit::context(
        SOURCE,
        Paths::rooted(std::env::temp_dir().join("nugget-google-conformance")),
        h.egress.clone(),
        h.secrets.clone(),
        known,
    )
}

fn qp(r: &Request, key: &str) -> Option<String> {
    r.url
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

/// Page number from a `p<n>` page token (0 without one).
fn page_no(r: &Request) -> usize {
    qp(r, "pageToken")
        .and_then(|t| t.strip_prefix('p').and_then(|n| n.parse().ok()))
        .unwrap_or(0)
}

fn last_segment(r: &Request) -> String {
    r.url
        .path()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// A 429 whose `Retry-After` is an HTTP-date, with the server's own `Date`.
fn throttled_until(date: &str, retry_at: &str) -> ResponseTemplate {
    ResponseTemplate::new(429)
        .insert_header("Date", date)
        .insert_header("Retry-After", retry_at)
}

fn upserted(sink: &ReplicaSink, key: &str) -> Document {
    sink.upserts()
        .into_iter()
        .rev()
        .find(|d| d.key.as_str() == key)
        .unwrap_or_else(|| panic!("no upsert for {key}"))
}

fn text_of(d: &Document) -> String {
    match &d.content {
        Content::Text(t) => t.clone(),
        Content::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        Content::File(p) => panic!("file content at {p:?}"),
    }
}

/// Recorded responses (from a test account, then scrubbed with [`scrub_json`]) used below.
const RECORDED_GMAIL_LIST: &str = r#"{"messages":[{"id":"18f3a1b2c3d4e5f6","threadId":"18f3a1b2c3d4e5f6"},{"id":"18f3a1b2c3d4e5f7","threadId":"18f3a1b2c3d4e5f6"}],"resultSizeEstimate":2}"#;
const RECORDED_GCAL_EVENT: &str = r#"{"kind":"calendar#event","etag":"\"3421987654321000\"","id":"7kq2abcdefghijklmnopqrstuv_20261102T150000Z","status":"confirmed","htmlLink":"https://www.google.com/calendar/event?eid=N2txMg","created":"2026-09-01T10:00:00.000Z","updated":"2026-10-01T10:00:00.000Z","summary":"Weekly sync","organizer":{"email":"user@example.com","self":true},"start":{"dateTime":"2026-11-02T10:00:00-05:00","timeZone":"America/New_York"},"end":{"dateTime":"2026-11-02T10:30:00-05:00","timeZone":"America/New_York"},"recurringEventId":"7kq2abcdefghijklmnopqrstuv","originalStartTime":{"dateTime":"2026-11-02T10:00:00-05:00","timeZone":"America/New_York"},"iCalUID":"7kq2abcdefghijklmnopqrstuv@example.com","sequence":0,"reminders":{"useDefault":true}}"#;
const RECORDED_DRIVE_FILE: &str = r#"{"id":"1AbCdEfGhIjKlMnOpQrStUvWxYz0123456789","name":"Q4 plan","mimeType":"application/vnd.google-apps.document","version":"57","modifiedTime":"2026-10-01T09:00:00.000Z","createdTime":"2026-09-01T09:00:00.000Z","trashed":false,"parents":["0AbCdEfRootFolder"],"ownedByMe":true,"shared":false,"webViewLink":"https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrStUvWxYz0123456789/edit","owners":[{"displayName":"Test User","emailAddress":"user@example.com"}]}"#;

#[test]
fn recorded_fixtures_are_scrubbed() {
    for f in [
        RECORDED_GMAIL_LIST,
        RECORDED_GCAL_EVENT,
        RECORDED_DRIVE_FILE,
    ] {
        assert_scrubbed(f);
    }
    // What scrubbing a fresh recording looks like.
    let mut raw =
        json!({"access_token": "ya29.live", "organizer": {"email": "real.person@corp.io"}});
    scrub_json(&mut raw);
    assert_scrubbed(&raw.to_string());
}

#[tokio::test]
async fn regression_retry_after_http_date_uses_server_clock() {
    // The server's clock is years behind ours; it asks for 30 s. Measured against the local
    // clock the date is in the past ("now"), and before the fix the HTTP-date form was not
    // parsed at all: either way the connector hammered the server and lost the hint.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .and(path("/drive/v3/changes"))
        .respond_with(throttled_until(
            "Wed, 21 Oct 2020 07:28:00 GMT",
            "Wed, 21 Oct 2020 07:28:30 GMT",
        ))
        .mount(&h.server)
        .await;
    let conn = h.build(
        &DriveFactory::new()
            .with_endpoints(h.endpoints())
            .with_retry(fast_retry(3)),
        json!({"client_id": "cid"}),
    );
    let r = run(
        &conn,
        Some(json!({"v": 1, "changes_token": "5"})),
        &RecSink::default(),
    )
    .await;
    assert!(
        matches!(
            r,
            Err(Error::RateLimited {
                retry_after_ms: Some(30_000)
            })
        ),
        "{r:?}"
    );
    // A 30 s wait is longer than the inline policy allows: no early retry.
    assert_eq!(h.server.received_requests().await.unwrap().len(), 1);
    h.assert_egress("sync:gdrive").await;
}

#[tokio::test]
async fn regression_retry_after_longer_than_policy_is_not_cut_short() {
    // Before the fix the hint was capped at the policy's max delay and the request retried
    // after 50 ms although the server asked for 30 s.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .and(path("/gmail/v1/users/me/profile"))
        .respond_with(too_many_requests(30))
        .mount(&h.server)
        .await;
    let conn = gmail_conn(&h, Replica::new(), fast_retry(3));
    let r = run_pass(&*conn, None, &ReplicaSink::new(Replica::new())).await;
    assert!(
        matches!(
            r,
            Err(Error::RateLimited {
                retry_after_ms: Some(30_000)
            })
        ),
        "{r:?}"
    );
    assert_eq!(h.server.received_requests().await.unwrap().len(), 1);
}

// ---------------------------------------------------------------------------------------------
// gmail
// ---------------------------------------------------------------------------------------------

const GM: &str = "/gmail/v1/users/me";

fn gmail_settings() -> Value {
    json!({"client_id": "cid.apps.googleusercontent.com", "client_secret": "s"})
}

fn gmail_conn(h: &Harness, replica: Arc<Replica>, retry: RetryPolicy) -> Arc<dyn Connector> {
    gmail_factory(h, retry)
        .build(&gmail_settings(), ctx_with(h, replica))
        .unwrap()
}

fn gmail_factory(h: &Harness, retry: RetryPolicy) -> GmailFactory {
    GmailFactory::new()
        .with_endpoints(h.endpoints())
        .with_retry(retry)
}

/// "Now" in ms, fixed for the test binary so stateless mocks answer identically every time.
fn now_ms() -> i64 {
    static NOW: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *NOW.get_or_init(|| OffsetDateTime::now_utc().unix_timestamp() * 1000)
}

fn rfc822(subject: &str, body: &str) -> String {
    format!(
        "From: Alice <alice@example.com>\r\nTo: me@example.com\r\nSubject: {subject}\r\n\
         MIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{body}\r\n"
    )
}

fn gm_message(id: &str, history: &str, labels: &[&str], subject: &str, body: &str) -> Value {
    json!({
        "id": id, "threadId": format!("t{id}"), "labelIds": labels,
        "snippet": "plain snippet", "historyId": history,
        "internalDate": now_ms().to_string(), "sizeEstimate": body.len(),
        "raw": URL_SAFE_NO_PAD.encode(rfc822(subject, body)),
    })
}

/// `messages.get?format=raw` for any id: "Subject <id>", history `7`.
fn gm_any_message(r: &Request) -> ResponseTemplate {
    let id = last_segment(r);
    ResponseTemplate::new(200).set_body_json(gm_message(
        &id,
        "7",
        &["INBOX"],
        &format!("Subject {id}"),
        &format!("Body of {id}"),
    ))
}

async fn gm_mount_any_message(h: &Harness) {
    Mock::given(method("GET"))
        .and(path_regex(format!("^{GM}/messages/[A-Za-z0-9]+$")))
        .and(query_param("format", "raw"))
        .respond_with(gm_any_message)
        .mount(&h.server)
        .await;
}

async fn gm_mount_profile(h: &Harness, history_id: &str) {
    Mock::given(method("GET"))
        .and(path(format!("{GM}/profile")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"historyId": history_id})))
        .mount(&h.server)
        .await;
}

fn gm_list_body(ids: &[String], next: Option<String>) -> Value {
    let mut body = json!({
        "messages": ids.iter().map(|id| json!({"id": id, "threadId": format!("t{id}")})).collect::<Vec<_>>(),
        "resultSizeEstimate": ids.len(),
    });
    if let Some(n) = next {
        body["nextPageToken"] = n.into();
    }
    body
}

/// `messages.list`: `pages` pages of `per` ids (`m<page>x<i>`), tokens `p<n>`.
async fn gm_mount_pages(h: &Harness, pages: usize, per: usize) {
    Mock::given(method("GET"))
        .and(path(format!("{GM}/messages")))
        .respond_with(move |r: &Request| {
            let n = page_no(r);
            let ids: Vec<String> = (0..per).map(|i| format!("m{n}x{i}")).collect();
            let next = (n + 1 < pages).then(|| format!("p{}", n + 1));
            ResponseTemplate::new(200).set_body_json(gm_list_body(&ids, next))
        })
        .mount(&h.server)
        .await;
}

async fn gm_mount_list(h: &Harness, ids: &[&str]) {
    let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
    Mock::given(method("GET"))
        .and(path(format!("{GM}/messages")))
        .respond_with(ResponseTemplate::new(200).set_body_json(gm_list_body(&ids, None)))
        .mount(&h.server)
        .await;
}

fn gm_history(records: Value, next: Option<&str>, history_id: &str) -> ResponseTemplate {
    let mut body = json!({"history": records, "historyId": history_id});
    if let Some(n) = next {
        body["nextPageToken"] = n.into();
    }
    ResponseTemplate::new(200).set_body_json(body)
}

fn gm_seed(replica: &Replica, id: &str, version: &str) {
    let mut d = Document::new(id, "message/rfc822", Content::Text(id.into()));
    d.version = Some(version.into());
    replica.apply(&Change::Upsert(d));
}

#[tokio::test]
async fn regression_gmail_one_bad_message_does_not_stop_the_pass() {
    // Before the fix a malformed, mismatched or body-less messages.get answer failed the
    // whole pass (and every later pass, at the same message).
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    gm_mount_profile(&h, "5").await;
    gm_mount_list(&h, &["m1", "m2", "m3", "m4", "m5"]).await;
    let bad: [(&str, ResponseTemplate); 3] = [
        (
            "m1",
            ResponseTemplate::new(200).set_body_string(r#"{"id": "m1", "raw": "#),
        ),
        (
            "m2",
            ResponseTemplate::new(200).set_body_json(gm_message(
                "other",
                "3",
                &["INBOX"],
                "x",
                "y",
            )),
        ),
        (
            "m4",
            ResponseTemplate::new(200).set_body_json(json!({"id": "m4", "labelIds": ["INBOX"]})),
        ),
    ];
    for (id, resp) in bad {
        Mock::given(method("GET"))
            .and(path(format!("{GM}/messages/{id}")))
            .respond_with(resp)
            .with_priority(1)
            .mount(&h.server)
            .await;
    }
    gm_mount_any_message(&h).await;
    let replica = Replica::new();
    let conn = gmail_conn(&h, replica.clone(), fast_retry(0));
    let out = run_pass(&*conn, None, &ReplicaSink::new(replica.clone()))
        .await
        .unwrap();
    assert_eq!(replica.keys(), vec!["m3", "m5"]);
    // The bad answers are carried with their failure counts (never indexed under the wrong
    // key); they are dropped once they have failed in `MAX_ATTEMPTS` passes.
    assert_eq!(
        out["refetch"],
        json!([{"id": "m1", "n": 1}, {"id": "m2", "n": 1}, {"id": "m4", "n": 1}])
    );
    h.assert_egress("sync:gmail").await;
}

#[tokio::test]
async fn regression_gmail_message_5xx_is_carried_and_refetched() {
    // Before the fix a message that kept failing with 5xx failed the pass at the same spot
    // every time, so nothing after it was ever indexed.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    gm_mount_profile(&h, "5").await;
    gm_mount_list(&h, &["m1", "m2", "m3"]).await;
    Mock::given(method("GET"))
        .and(path(format!("{GM}/messages/m2")))
        .respond_with(Sequence::new(vec![
            server_error(500, None),
            server_error(503, None),
            gm_any_message_for("m2"),
        ]))
        .with_priority(1)
        .mount(&h.server)
        .await;
    gm_mount_any_message(&h).await;
    let replica = Replica::new();
    let conn = gmail_conn(&h, replica.clone(), fast_retry(1));
    let out = run_pass(&*conn, None, &ReplicaSink::new(replica.clone()))
        .await
        .unwrap();
    assert_eq!(replica.keys(), vec!["m1", "m3"]);
    assert_eq!(
        out,
        json!({"phase": "incremental", "history_id": "5", "refetch": [{"id": "m2", "n": 1}]})
    );

    // The next pass fetches it again.
    Mock::given(method("GET"))
        .and(path(format!("{GM}/history")))
        .respond_with(gm_history(json!([]), None, "6"))
        .mount(&h.server)
        .await;
    let out = run_pass(&*conn, Some(out), &ReplicaSink::new(replica.clone()))
        .await
        .unwrap();
    assert_eq!(out, json!({"phase": "incremental", "history_id": "6"}));
    assert_eq!(replica.keys(), vec!["m1", "m2", "m3"]);
    h.assert_egress("sync:gmail").await;
}

fn gm_any_message_for(id: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(gm_message(
        id,
        "7",
        &["INBOX"],
        &format!("Subject {id}"),
        "body",
    ))
}

#[tokio::test]
async fn gmail_backfill_and_incremental_survive_a_crash_anywhere() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    gm_mount_profile(&h, "900").await;
    gm_mount_pages(&h, 3, 2).await;
    gm_mount_any_message(&h).await;
    // Nothing new after the backfill: once its closing cursor is committed, a resumed pass
    // runs the history from it.
    Mock::given(method("GET"))
        .and(path(format!("{GM}/history")))
        .and(query_param("startHistoryId", "900"))
        .respond_with(gm_history(json!([]), None, "900"))
        .with_priority(1)
        .mount(&h.server)
        .await;
    // History for the incremental pass (and the follow-up passes the kit runs).
    Mock::given(method("GET"))
        .and(path(format!("{GM}/history")))
        .and(query_param_is_missing("pageToken"))
        .respond_with(gm_history(
            json!([
                {"id": "101", "messagesAdded": [{"message": {"id": "n1", "labelIds": ["INBOX"]}}]},
                {"id": "102", "messagesDeleted": [{"message": {"id": "s1"}}]},
            ]),
            Some("hp2"),
            "110",
        ))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{GM}/history")))
        .and(query_param("pageToken", "hp2"))
        .respond_with(gm_history(
            json!([
                {"id": "105", "messagesAdded": [{"message": {"id": "n2", "labelIds": ["INBOX"]}}]},
                {"id": "106", "messagesDeleted": [{"message": {"id": "s2"}}]},
            ]),
            None,
            "110",
        ))
        .mount(&h.server)
        .await;
    let report = assert_crash_resumable(
        |r| {
            let c = gmail_conn(&h, r, fast_retry(0));
            async move { c }
        },
        None,
        Default::default(),
    )
    .await;
    assert_eq!(report.sends, 6);

    let seed = Replica::new();
    for id in ["s1", "s2", "s3"] {
        gm_seed(&seed, id, "1@1");
    }
    let report = assert_crash_resumable(
        |r| {
            let c = gmail_conn(&h, r, fast_retry(0));
            async move { c }
        },
        Some(json!({"phase": "incremental", "history_id": "100"})),
        seed.snapshot(),
    )
    .await;
    assert_eq!(report.sends, 4);
    // A crash drops in-flight concurrent fetches: metered, though the server may not have
    // logged them.
    let events = h.egress.0.lock().unwrap().clone();
    assert!(events.len() >= h.server.received_requests().await.unwrap().len());
    assert!(events.iter().all(|e| e.feature == "sync:gmail"));
}

// ---------------------------------------------------------------------------------------------
// gcal
// ---------------------------------------------------------------------------------------------

const EVENTS: &str = "/calendar/v3/calendars/primary/events";

fn gcal_conn(h: &Harness, replica: Arc<Replica>, retry: RetryPolicy) -> Arc<dyn Connector> {
    CalendarFactory::new()
        .with_endpoints(h.endpoints())
        .with_retry(retry)
        .build(
            &json!({"client_id": "cid", "calendars": ["primary"]}),
            ctx_with(h, replica),
        )
        .unwrap()
}

/// RFC 3339 (UTC) `days` from now, at `hour`:00.
fn at(days: i64, hour: u8) -> String {
    let d = (OffsetDateTime::now_utc() + time::Duration::days(days)).date();
    d.with_hms(hour, 0, 0)
        .unwrap()
        .assume_utc()
        .format(&Rfc3339)
        .unwrap()
}

fn ev(id: &str, summary: &str, etag: &str, start: Value, end: Value) -> Value {
    json!({
        "kind": "calendar#event", "id": id, "status": "confirmed", "etag": etag,
        "updated": "2026-10-01T12:00:00.000Z", "summary": summary,
        "htmlLink": format!("https://www.google.com/calendar/event?eid={id}"),
        "start": start, "end": end,
    })
}

fn ev_now(id: &str, summary: &str, etag: &str, day: i64) -> Value {
    ev(
        id,
        summary,
        etag,
        json!({"dateTime": at(day, 10)}),
        json!({"dateTime": at(day, 11)}),
    )
}

fn cal_page(items: Vec<Value>, next: Option<String>, sync: Option<&str>) -> ResponseTemplate {
    let mut body =
        json!({"kind": "calendar#events", "summary": "Work", "timeZone": "UTC", "items": items});
    if let Some(n) = next {
        body["nextPageToken"] = n.into();
    }
    if let Some(s) = sync {
        body["nextSyncToken"] = s.into();
    }
    ResponseTemplate::new(200).set_body_json(body)
}

/// A full listing of `pages` pages of `per` events (`e<page>x<i>`), then sync token `st1`.
async fn cal_mount_pages(h: &Harness, pages: usize, per: usize) {
    Mock::given(method("GET"))
        .and(path(EVENTS))
        .and(query_param_is_missing("syncToken"))
        .respond_with(move |r: &Request| {
            let n = page_no(r);
            let items = (0..per)
                .map(|i| ev_now(&format!("e{n}x{i}"), &format!("Event {n}.{i}"), "\"1\"", 1))
                .collect();
            let last = n + 1 == pages;
            cal_page(
                items,
                (!last).then(|| format!("p{}", n + 1)),
                last.then_some("st1"),
            )
        })
        .mount(&h.server)
        .await;
}

#[tokio::test]
async fn gcal_huge_descriptions_and_attendee_lists_are_capped() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let mut big = ev_now("e1", "All hands", "\"1\"", 1);
    big["description"] = big_text(1_000_000).into();
    big["attendees"] = (0..1000)
        .map(|i| json!({"email": format!("p{i}@example.com")}))
        .collect::<Vec<_>>()
        .into();
    Mock::given(method("GET"))
        .and(path(EVENTS))
        .respond_with(cal_page(
            vec![big, ev_now("e2", "Small", "\"1\"", 1)],
            None,
            Some("st1"),
        ))
        .mount(&h.server)
        .await;
    let replica = Replica::new();
    let conn = gcal_conn(&h, replica.clone(), fast_retry(1));
    let sink = ReplicaSink::new(replica.clone());
    run_pass(&*conn, None, &sink).await.unwrap();
    assert_eq!(replica.len(), 2);
    let d = upserted(&sink, "primary/e1");
    assert!(text_of(&d).len() < 40_000, "{}", text_of(&d).len());
    assert_eq!(d.metadata["attendees"].as_array().unwrap().len(), 200);
    h.assert_egress("sync:gcal").await;
}

#[tokio::test]
async fn regression_gcal_malformed_event_entry_is_skipped() {
    // Before the fix one event with a wrongly typed field failed the whole page, and every
    // pass after it.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .and(path(EVENTS))
        .respond_with(cal_page(
            vec![
                ev_now("e1", "Fine", "\"1\"", 1),
                json!({"id": 5, "summary": "numeric id"}),
                json!({"id": "e3", "start": "not an object"}),
                json!("not even an object"),
                ev_now("e4", "Also fine", "\"1\"", 2),
            ],
            None,
            Some("st1"),
        ))
        .mount(&h.server)
        .await;
    let replica = Replica::new();
    let conn = gcal_conn(&h, replica.clone(), fast_retry(1));
    let out = run_pass(&*conn, None, &ReplicaSink::new(replica.clone()))
        .await
        .unwrap();
    assert_eq!(out["calendars"]["primary"]["sync_token"], "st1");
    assert_eq!(replica.keys(), vec!["primary/e1", "primary/e4"]);
    h.assert_egress("sync:gcal").await;
}

#[tokio::test]
async fn gcal_full_and_incremental_survive_a_crash_anywhere() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    cal_mount_pages(&h, 3, 2).await;
    // Nothing new after the listing (the kit runs a follow-up pass from the final cursor).
    Mock::given(method("GET"))
        .and(path(EVENTS))
        .and(query_param("syncToken", "st1"))
        .respond_with(cal_page(vec![], None, Some("st1")))
        .mount(&h.server)
        .await;
    let report = assert_crash_resumable(
        |r| {
            let c = gcal_conn(&h, r, fast_retry(0));
            async move { c }
        },
        None,
        Default::default(),
    )
    .await;
    assert_eq!(report.sends, 6);
    h.assert_egress("sync:gcal").await;

    // Incremental (a fresh server): two pages of deltas from `st1`, then nothing new at `st2`.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    cal_mount_pages(&h, 3, 2).await;
    Mock::given(method("GET"))
        .and(path(EVENTS))
        .and(query_param("syncToken", "st1"))
        .and(query_param_is_missing("pageToken"))
        .respond_with(cal_page(
            vec![
                ev_now("e0x0", "Edited", "\"2\"", 1),
                json!({"id": "e0x1", "status": "cancelled"}),
            ],
            Some("p1".into()),
            None,
        ))
        .with_priority(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(EVENTS))
        .and(query_param("syncToken", "st1"))
        .and(query_param("pageToken", "p1"))
        .respond_with(cal_page(
            vec![
                ev_now("n1", "New", "\"1\"", 2),
                json!({"id": "e1x0", "status": "cancelled"}),
            ],
            None,
            Some("st2"),
        ))
        .with_priority(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(EVENTS))
        .and(query_param("syncToken", "st2"))
        .respond_with(cal_page(vec![], None, Some("st2")))
        .with_priority(1)
        .mount(&h.server)
        .await;
    let seed = Replica::new();
    let conn = gcal_conn(&h, seed.clone(), fast_retry(0));
    let start = run_pass(&*conn, None, &ReplicaSink::new(seed.clone()))
        .await
        .unwrap();
    let report = assert_crash_resumable(
        |r| {
            let c = gcal_conn(&h, r, fast_retry(0));
            async move { c }
        },
        Some(start),
        seed.snapshot(),
    )
    .await;
    assert_eq!(report.sends, 4);
    h.assert_egress("sync:gcal").await;
}

// ---------------------------------------------------------------------------------------------
// gdrive (incl. Docs, Sheets and Slides)
// ---------------------------------------------------------------------------------------------

const DRIVE: &str = "/drive/v3";
const GDOC: &str = "application/vnd.google-apps.document";
const GSHEET: &str = "application/vnd.google-apps.spreadsheet";
const GSLIDES: &str = "application/vnd.google-apps.presentation";

fn drive_conn(h: &Harness, replica: Arc<Replica>, retry: RetryPolicy) -> Arc<dyn Connector> {
    drive_conn_with(h, replica, retry, json!({"client_id": "cid"}))
}

fn drive_conn_with(
    h: &Harness,
    replica: Arc<Replica>,
    retry: RetryPolicy,
    settings: Value,
) -> Arc<dyn Connector> {
    DriveFactory::new()
        .with_endpoints(h.endpoints())
        .with_retry(retry)
        .build(&settings, ctx_with(h, replica))
        .unwrap()
}

fn dfile(id: &str, name: &str, mime: &str, version: &str) -> Value {
    json!({
        "id": id, "name": name, "mimeType": mime, "version": version,
        "modifiedTime": "2026-10-01T12:00:00.000Z", "trashed": false, "parents": ["root"],
        "ownedByMe": true, "webViewLink": format!("https://drive.google.com/file/d/{id}/view"),
    })
}

fn txt(id: &str) -> Value {
    dfile(id, &format!("{id}.txt"), "text/plain", "1")
}

fn files_page(files: Vec<Value>, next: Option<String>) -> ResponseTemplate {
    let mut body = json!({"files": files});
    if let Some(n) = next {
        body["nextPageToken"] = n.into();
    }
    ResponseTemplate::new(200).set_body_json(body)
}

fn changes_page(
    changes: Vec<Value>,
    next: Option<&str>,
    new_start: Option<&str>,
) -> ResponseTemplate {
    let mut body = json!({"changes": changes});
    if let Some(n) = next {
        body["nextPageToken"] = n.into();
    }
    if let Some(s) = new_start {
        body["newStartPageToken"] = s.into();
    }
    ResponseTemplate::new(200).set_body_json(body)
}

fn changed(f: Value) -> Value {
    json!({"changeType": "file", "removed": false, "fileId": f["id"].clone(), "file": f})
}

fn removed(id: &str) -> Value {
    json!({"changeType": "file", "removed": true, "fileId": id})
}

async fn drive_mount_start(h: &Harness, resp: impl wiremock::Respond + 'static) {
    Mock::given(method("GET"))
        .and(path(format!("{DRIVE}/changes/startPageToken")))
        .respond_with(resp)
        .mount(&h.server)
        .await;
}

fn start_token(t: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"startPageToken": t}))
}

async fn drive_mount_changes(h: &Harness, token: &str, resp: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(format!("{DRIVE}/changes")))
        .and(query_param("pageToken", token))
        .respond_with(resp)
        .mount(&h.server)
        .await;
}

/// Downloads (`alt=media`) and Docs exports for any id, plus a Sheets and Slides API.
async fn drive_mount_content(h: &Harness) {
    Mock::given(method("GET"))
        .and(path_regex(format!("^{DRIVE}/files/[A-Za-z0-9_-]+$")))
        .and(query_param("alt", "media"))
        .respond_with(|r: &Request| {
            ResponseTemplate::new(200).set_body_string(format!("content of {}", last_segment(r)))
        })
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(format!("^{DRIVE}/files/[A-Za-z0-9_-]+/export$")))
        .and(query_param("mimeType", "text/markdown"))
        .respond_with(|r: &Request| {
            let id = r
                .url
                .path()
                .split('/')
                .nth_back(1)
                .unwrap_or_default()
                .to_string();
            ResponseTemplate::new(200).set_body_string(format!("# Doc {id}\n\n日本語 body"))
        })
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("^/sheets/v4/spreadsheets/[A-Za-z0-9_-]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"sheets": [
            {"properties": {"sheetId": 0, "title": "Données", "sheetType": "GRID"}}
        ]})))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(
            "^/sheets/v4/spreadsheets/[A-Za-z0-9_-]+/values/",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "values": [["Item", "Coût"], ["Thé", "3 €"]]
        })))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("^/slides/v1/presentations/[A-Za-z0-9_-]+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"slides": [
            {"pageElements": [{"shape": {"text": {"textElements": [
                {"paragraphMarker": {}}, {"textRun": {"content": "Hello 👋\n"}}
            ]}}}]}
        ]})))
        .mount(&h.server)
        .await;
}

/// `files.list`: `pages` pages of `per` text files (`f<page>x<i>`).
async fn drive_mount_pages(h: &Harness, pages: usize, per: usize) {
    Mock::given(method("GET"))
        .and(path(format!("{DRIVE}/files")))
        .respond_with(move |r: &Request| {
            let n = page_no(r);
            let files = (0..per).map(|i| txt(&format!("f{n}x{i}"))).collect();
            files_page(files, (n + 1 < pages).then(|| format!("p{}", n + 1)))
        })
        .mount(&h.server)
        .await;
}

#[tokio::test]
async fn regression_gdrive_export_5xx_is_carried_and_retried() {
    // Before the fix a Docs/Sheets/Slides export or download that kept failing with 5xx failed
    // the whole pass at the same file every time.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    drive_mount_start(&h, start_token("100")).await;
    let failing = [
        dfile("d1", "Doc", GDOC, "1"),
        dfile("s1", "Sheet", GSHEET, "1"),
        dfile("k1", "Deck", GSLIDES, "1"),
        dfile("b1", "b1.txt", "text/plain", "1"),
    ];
    Mock::given(method("GET"))
        .and(path(format!("{DRIVE}/files")))
        .respond_with(files_page(
            failing
                .iter()
                .cloned()
                .chain([json!({"id": 5}), json!("junk"), txt("ok1")])
                .collect(),
            None,
        ))
        .mount(&h.server)
        .await;
    for p in [
        format!("{DRIVE}/files/d1/export"),
        "/sheets/v4/spreadsheets/s1".to_string(),
        "/slides/v1/presentations/k1".to_string(),
    ] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(server_error(500, None))
            .up_to_n_times(2)
            .with_priority(1)
            .mount(&h.server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(format!("{DRIVE}/files/b1")))
        .and(query_param("alt", "media"))
        .respond_with(server_error(502, None))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&h.server)
        .await;
    for f in &failing {
        let id = f["id"].as_str().unwrap();
        Mock::given(method("GET"))
            .and(path(format!("{DRIVE}/files/{id}")))
            .and(query_param_is_missing("alt"))
            .respond_with(ResponseTemplate::new(200).set_body_json(f.clone()))
            .mount(&h.server)
            .await;
    }
    drive_mount_changes(&h, "100", changes_page(vec![], None, Some("101"))).await;
    drive_mount_changes(&h, "101", changes_page(vec![], None, Some("102"))).await;
    drive_mount_content(&h).await;
    let replica = Replica::new();
    let conn = drive_conn(&h, replica.clone(), fast_retry(1));
    let out = run_pass(&*conn, None, &ReplicaSink::new(replica.clone()))
        .await
        .unwrap();
    assert_eq!(replica.keys(), vec!["ok1"]);
    assert_eq!(
        out,
        json!({"v": 1, "changes_token": "101", "retry": [
            {"id": "d1", "n": 1}, {"id": "s1", "n": 1}, {"id": "k1", "n": 1}, {"id": "b1", "n": 1}
        ]})
    );
    let out = run_pass(&*conn, Some(out), &ReplicaSink::new(replica.clone()))
        .await
        .unwrap();
    assert_eq!(out, json!({"v": 1, "changes_token": "102"}));
    assert_eq!(replica.keys(), vec!["b1", "d1", "k1", "ok1", "s1"]);
    h.assert_egress("sync:gdrive").await;
}

#[tokio::test]
async fn gdrive_listing_and_changes_survive_a_crash_anywhere() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    drive_mount_start(&h, start_token("100")).await;
    drive_mount_pages(&h, 3, 2).await;
    drive_mount_changes(
        &h,
        "100",
        changes_page(vec![changed(txt("n1")), removed("f0x0")], Some("101"), None),
    )
    .await;
    drive_mount_changes(
        &h,
        "101",
        changes_page(
            vec![
                changed(dfile("f1x0", "renamed.txt", "text/plain", "2")),
                removed("f2x1"),
            ],
            None,
            Some("102"),
        ),
    )
    .await;
    drive_mount_changes(&h, "102", changes_page(vec![], None, Some("102"))).await;
    drive_mount_content(&h).await;
    let report = assert_crash_resumable(
        |r| {
            let c = drive_conn(&h, r, fast_retry(0));
            async move { c }
        },
        None,
        Default::default(),
    )
    .await;
    assert!(report.sends >= 6 + 4, "{report:?}");

    // Incremental from an index holding the listing.
    let seed = Replica::new();
    for n in 0..3 {
        for i in 0..2 {
            let mut d = Document::new(format!("f{n}x{i}"), "text/plain", Content::Text("x".into()));
            d.version = Some("1|".into());
            seed.apply(&Change::Upsert(d));
        }
    }
    let report = assert_crash_resumable(
        |r| {
            let c = drive_conn(&h, r, fast_retry(0));
            async move { c }
        },
        Some(json!({"v": 1, "changes_token": "100"})),
        seed.snapshot(),
    )
    .await;
    assert_eq!((report.sends, report.checkpoints), (4, 3), "{report:?}");
    h.assert_egress("sync:gdrive").await;
}

// ---------------------------------------------------------------------------------------------
// Review follow-ups: carried failures never block a source
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn regression_gmail_poison_messages_cannot_block_a_source() {
    // Before the fix eight messages that always fail ended every pass at the same history page
    // (the streak counted them every time, and the failure counts were never saved).
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let replica = Replica::new();
    gm_seed(&replica, "p0", "1@1");
    let added: Vec<Value> = (0..9)
        .map(|i| json!({"id": format!("{}", 101 + i), "messagesAdded": [{"message": {"id": format!("p{i}"), "labelIds": ["INBOX"]}}]}))
        .collect();
    Mock::given(method("GET"))
        .and(path(format!("{GM}/history")))
        .and(query_param("startHistoryId", "100"))
        .respond_with(gm_history(json!(added), None, "200"))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{GM}/history")))
        .and(query_param("startHistoryId", "200"))
        .respond_with(gm_history(json!([]), None, "200"))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(format!("^{GM}/messages/p[0-9]$")))
        .respond_with(server_error(500, None))
        .mount(&h.server)
        .await;
    let conn = gmail_conn(&h, replica.clone(), fast_retry(0));
    let sink = ReplicaSink::new(replica.clone());
    let r = run_pass(
        &*conn,
        Some(json!({"phase": "incremental", "history_id": "100"})),
        &sink,
    )
    .await;
    assert!(
        matches!(r, Err(Error::Network(_))),
        "an outage-like run: {r:?}"
    );
    // The counts were saved at the last position.
    let mut cursor = sink.committed().unwrap();
    assert_eq!(cursor["history_id"], "100");
    assert_eq!(cursor["refetch"].as_array().unwrap().len(), 8);
    assert!(
        cursor["refetch"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["n"] == 1)
    );
    // Every later pass completes, and the poison is dropped after the limit.
    let mut passes = 0;
    while cursor.get("refetch").is_some() {
        passes += 1;
        assert!(passes <= 8, "poison never dropped: {cursor}");
        cursor = run_pass(&*conn, Some(cursor), &ReplicaSink::new(replica.clone()))
            .await
            .unwrap();
        assert_eq!(cursor["history_id"], "200");
    }
    // A history-phase failure keeps the indexed version.
    assert_eq!(replica.get("p0").unwrap().version.as_deref(), Some("1@1"));
    h.assert_egress("sync:gmail").await;
}

#[tokio::test]
async fn regression_gmail_deletes_do_not_reset_the_failure_streak() {
    // Before the fix every delete in a history page reset the streak, so an outage between
    // deletes never ended the pass.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let records: Vec<Value> = (0..8)
        .flat_map(|i| {
            [
                json!({"id": format!("{}", 101 + 2 * i), "messagesAdded": [{"message": {"id": format!("f{i}"), "labelIds": ["INBOX"]}}]}),
                json!({"id": format!("{}", 102 + 2 * i), "messagesDeleted": [{"message": {"id": format!("d{i}")}}]}),
            ]
        })
        .collect();
    Mock::given(method("GET"))
        .and(path(format!("{GM}/history")))
        .respond_with(gm_history(json!(records), None, "200"))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(format!("^{GM}/messages/f[0-9]$")))
        .respond_with(server_error(503, None))
        .mount(&h.server)
        .await;
    let replica = Replica::new();
    let conn = gmail_conn(&h, replica.clone(), fast_retry(0));
    let r = run_pass(
        &*conn,
        Some(json!({"phase": "incremental", "history_id": "100"})),
        &ReplicaSink::new(replica),
    )
    .await;
    assert!(matches!(r, Err(Error::Network(_))), "{r:?}");
}

#[tokio::test]
async fn regression_gmail_refetch_survives_history_expiry() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .and(path(format!("{GM}/history")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.server)
        .await;
    gm_mount_profile(&h, "300").await;
    gm_mount_list(&h, &["m1"]).await;
    gm_mount_any_message(&h).await;
    let replica = Replica::new();
    let conn = gmail_conn(&h, replica.clone(), fast_retry(0));
    let sink = ReplicaSink::new(replica.clone());
    let out = run_pass(
        &*conn,
        Some(
            json!({"phase": "incremental", "history_id": "100", "refetch": [{"id": "m7", "n": 2}]}),
        ),
        &sink,
    )
    .await
    .unwrap();
    assert_eq!(out, json!({"phase": "incremental", "history_id": "300"}));
    assert_eq!(replica.keys(), vec!["m1", "m7"]);
    // The resync's start carried the list.
    let cps = sink.checkpoints.lock().unwrap().clone();
    assert_eq!(cps[0]["phase"], "backfill");
    assert_eq!(cps[0]["refetch"], json!([{"id": "m7", "n": 2}]));
    h.assert_egress("sync:gmail").await;
}

#[tokio::test]
async fn regression_gmail_refetch_of_a_deleted_message_deletes_it() {
    // An older cursor (bare ids) whose carried message was deleted upstream.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .and(path(format!("{GM}/history")))
        .respond_with(gm_history(json!([]), None, "101"))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{GM}/messages/m3")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.server)
        .await;
    let replica = Replica::new();
    gm_seed(&replica, "m3", "1@1");
    let conn = gmail_conn(&h, replica.clone(), fast_retry(0));
    let out = run_pass(
        &*conn,
        Some(json!({"phase": "incremental", "history_id": "100", "refetch": ["m3"]})),
        &ReplicaSink::new(replica.clone()),
    )
    .await
    .unwrap();
    assert_eq!(out, json!({"phase": "incremental", "history_id": "101"}));
    assert!(replica.is_empty());
    h.assert_egress("sync:gmail").await;
}

#[tokio::test]
async fn regression_gdrive_poison_files_keep_their_version_and_are_dropped() {
    // Before the fix a carried file that kept failing was retried forever, and enough of
    // them ended every pass.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    drive_mount_start(&h, start_token("100")).await;
    let x1 = dfile("x1", "x1.txt", "text/plain", "2");
    Mock::given(method("GET"))
        .and(path(format!("{DRIVE}/files")))
        .respond_with(files_page(vec![x1.clone(), txt("ok1")], None))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{DRIVE}/files/x1")))
        .and(query_param("alt", "media"))
        .respond_with(server_error(500, None))
        .with_priority(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{DRIVE}/files/x1")))
        .and(query_param_is_missing("alt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(x1))
        .mount(&h.server)
        .await;
    drive_mount_changes(&h, "100", changes_page(vec![], None, Some("101"))).await;
    drive_mount_changes(&h, "101", changes_page(vec![], None, Some("101"))).await;
    drive_mount_content(&h).await;
    // Indexed before; the cursor was reset, so this is a reconciling listing.
    let replica = Replica::new();
    let mut d = Document::new("x1", "text/plain", Content::Text("old".into()));
    d.version = Some("1|".into());
    replica.apply(&Change::Upsert(d));
    let conn = drive_conn(&h, replica.clone(), fast_retry(0));
    let mut cursor = run_pass(&*conn, None, &ReplicaSink::new(replica.clone()))
        .await
        .unwrap();
    assert_eq!(cursor["retry"], json!([{"id": "x1", "n": 1}]));
    let mut passes = 0;
    while cursor.get("retry").is_some() {
        passes += 1;
        assert!(passes <= 6, "poison never dropped: {cursor}");
        cursor = run_pass(&*conn, Some(cursor), &ReplicaSink::new(replica.clone()))
            .await
            .unwrap();
    }
    assert_eq!(passes, 5);
    // Through all of it the indexed version stayed.
    let x = replica.get("x1").unwrap();
    assert_eq!(
        (x.version.as_deref(), x.content.as_str()),
        (Some("1|"), "old")
    );
    assert!(replica.get("ok1").is_some());
    h.assert_egress("sync:gdrive").await;
}

#[tokio::test]
async fn regression_gdrive_poison_files_cannot_block_a_source() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    drive_mount_start(&h, start_token("100")).await;
    drive_mount_pages(&h, 1, 9).await;
    Mock::given(method("GET"))
        .and(path_regex(format!("^{DRIVE}/files/f0x[0-9]$")))
        .and(query_param("alt", "media"))
        .respond_with(server_error(503, None))
        .mount(&h.server)
        .await;
    drive_mount_changes(&h, "100", changes_page(vec![], None, Some("101"))).await;
    let replica = Replica::new();
    let conn = drive_conn(&h, replica.clone(), fast_retry(0));
    let sink = ReplicaSink::new(replica.clone());
    let r = run_pass(&*conn, None, &sink).await;
    assert!(matches!(r, Err(Error::Network(_))), "{r:?}");
    let committed = sink.committed().unwrap();
    assert_eq!(
        committed["retry"].as_array().unwrap().len(),
        8,
        "{committed}"
    );
    // The next pass gets past them.
    let out = run_pass(&*conn, Some(committed), &ReplicaSink::new(replica.clone()))
        .await
        .unwrap();
    assert_eq!(out["changes_token"], "101");
    assert_eq!(out["retry"].as_array().unwrap().len(), 9);
}

#[tokio::test]
async fn regression_gdrive_refused_carried_file_is_removed_not_fatal() {
    // Before the fix a 403 on one carried file (permissions changed) was `Unauthorized` for
    // the whole source.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    drive_mount_changes(&h, "101", changes_page(vec![], None, Some("102"))).await;
    Mock::given(method("GET"))
        .and(path(format!("{DRIVE}/files/x1")))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error": {
            "code": 403, "message": "The user does not have sufficient permissions for this file.",
            "errors": [{"reason": "insufficientFilePermissions"}]
        }})))
        .mount(&h.server)
        .await;
    let replica = Replica::new();
    let mut d = Document::new("x1", "text/plain", Content::Text("old".into()));
    d.version = Some("1|".into());
    replica.apply(&Change::Upsert(d));
    let conn = drive_conn(&h, replica.clone(), fast_retry(0));
    let out = run_pass(
        &*conn,
        Some(json!({"v": 1, "changes_token": "101", "retry": [{"id": "x1", "n": 1}]})),
        &ReplicaSink::new(replica.clone()),
    )
    .await
    .unwrap();
    assert_eq!(out, json!({"v": 1, "changes_token": "102"}));
    assert!(replica.is_empty());
    h.assert_egress("sync:gdrive").await;
}

#[tokio::test]
async fn regression_gcal_resync_with_malformed_events() {
    // Before the fix a malformed indexed event failed every resync (its existence check
    // parsed the whole event), a malformed cancellation was lost, and `"attendees": null`
    // made an event malformed.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .and(path(EVENTS))
        .and(query_param("syncToken", "st1"))
        .respond_with(ResponseTemplate::new(410))
        .mount(&h.server)
        .await;
    let mut e4 = ev_now("e4", "Null lists", "\"1\"", 1);
    e4["attendees"] = Value::Null;
    let mut e5 = ev_now("e5", "Null flags", "\"1\"", 1);
    e5["attendees"] = json!([{"email": "a@example.com", "optional": null, "resource": null}]);
    Mock::given(method("GET"))
        .and(path(EVENTS))
        .and(query_param_is_missing("syncToken"))
        .respond_with(cal_page(
            vec![
                json!({"id": "e1", "status": "confirmed", "start": "bad"}),
                json!({"id": "e2", "status": "cancelled", "start": 5}),
                e4,
                e5,
            ],
            None,
            Some("st2"),
        ))
        .mount(&h.server)
        .await;
    // e3 was not listed; its check answers with a malformed (but not cancelled) event.
    Mock::given(method("GET"))
        .and(path(format!("{EVENTS}/e3")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "e3", "status": "confirmed", "start": "bad"})),
        )
        .expect(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{EVENTS}/e1")))
        .respond_with(ResponseTemplate::new(404))
        .expect(0)
        .mount(&h.server)
        .await;
    let replica = Replica::new();
    let s = OffsetDateTime::now_utc().unix_timestamp() + 86_400;
    for id in ["e1", "e2", "e3"] {
        let mut d = Document::new(
            format!("primary/{id}"),
            "text/plain",
            Content::Text(id.into()),
        );
        d.version = Some(format!("\"1\"#{s}:{}", s + 3600));
        replica.apply(&Change::Upsert(d));
    }
    let conn = gcal_conn(&h, replica.clone(), fast_retry(0));
    let out = run_pass(
        &*conn,
        Some(json!({"v": 1, "calendars": {"primary": {"sync_token": "st1"}}})),
        &ReplicaSink::new(replica.clone()),
    )
    .await
    .unwrap();
    assert_eq!(out["calendars"]["primary"]["sync_token"], "st2");
    assert_eq!(
        replica.keys(),
        vec!["primary/e1", "primary/e3", "primary/e4", "primary/e5"]
    );
    assert_eq!(replica.get("primary/e1").unwrap().content, "e1");
    h.assert_egress("sync:gcal").await;
}
