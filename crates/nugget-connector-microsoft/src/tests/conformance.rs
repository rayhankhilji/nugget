//! Conformance suite (FF2) for the three Microsoft 365 connectors, against wiremock with
//! synthetic and recorded-and-scrubbed fixtures: paging, throttling, token refresh, deletes,
//! moves and renames, deltas and resyncs, size caps, unicode and HTML, recurrence and time
//! zones, clock skew, partial failure, crash resumption and egress.

use std::collections::BTreeMap;

use nugget_connector::testkit::{
    self, Replica, ReplicaItem, ReplicaSink, assert_crash_resumable, server_error,
};
use nugget_connector::{Content, Document};
use nugget_core::egress::EgressSink;
use serde_json::json;
use wiremock::matchers::{header, method, path, path_regex, query_param, query_param_is_missing};
use wiremock::{Mock, ResponseTemplate};

use super::*;
use crate::{OneDriveFactory, OutlookCalendarFactory, OutlookFactory};

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

const TOKEN_PATH: &str = "/common/oauth2/v2.0/token";
/// A server clock far from the local one (clock-skew tests).
const SERVER_DATE: &str = "Tue, 01 Sep 2020 10:00:00 GMT";
const SERVER_DATE_PLUS_30: &str = "Tue, 01 Sep 2020 10:00:30 GMT";

/// A delta feed on the mock server: its path and the query parameters of its links.
struct Feed {
    path: &'static str,
    skip: &'static str,
    delta: &'static str,
}

const INBOX: Feed = Feed {
    path: "/v1.0/me/mailFolders/inbox/messages/delta",
    skip: "$skiptoken",
    delta: "$deltatoken",
};
const CAL: Feed = Feed {
    path: "/v1.0/me/calendarView/delta",
    skip: "$skiptoken",
    delta: "$deltatoken",
};
const DRIVE: Feed = Feed {
    path: "/v1.0/me/drive/root/delta",
    skip: "token",
    delta: "token",
};

/// Where a page is served: the first page of a full listing, or a link token.
#[derive(Clone, Copy)]
enum At<'a> {
    Initial,
    Skip(&'a str),
    Delta(&'a str),
}

impl Feed {
    fn link(&self, h: &Harness, name: &str, value: &str) -> String {
        format!("{}{}?{name}={value}", h.uri(), self.path)
    }

    /// A page; `next` is a skip token, `delta` a delta token.
    fn page(
        &self,
        h: &Harness,
        items: Vec<Value>,
        next: Option<&str>,
        delta: Option<&str>,
    ) -> ResponseTemplate {
        let mut body =
            json!({"@odata.context": "https://graph.microsoft.com/v1.0/$metadata", "value": items});
        if let Some(n) = next {
            body["@odata.nextLink"] = self.link(h, self.skip, n).into();
        }
        if let Some(d) = delta {
            body["@odata.deltaLink"] = self.link(h, self.delta, d).into();
        }
        ResponseTemplate::new(200).set_body_json(body)
    }

    fn mock(&self, at: At<'_>) -> wiremock::MockBuilder {
        let m = Mock::given(method("GET")).and(path(self.path));
        match at {
            At::Initial => m
                .and(query_param_is_missing(self.skip))
                .and(query_param_is_missing(self.delta)),
            At::Skip(t) => m.and(query_param(self.skip, t)),
            At::Delta(t) => m.and(query_param(self.delta, t)),
        }
    }

    async fn mount(&self, h: &Harness, at: At<'_>, resp: ResponseTemplate) {
        self.mock(at).respond_with(resp).mount(&h.server).await;
    }

    /// Serve `pages` as one chain starting at `first`: page `i > 0` is at skip token
    /// `<prefix><i>`, the last page ends with delta token `end`.
    async fn mount_chain(
        &self,
        h: &Harness,
        first: At<'_>,
        pages: Vec<Vec<Value>>,
        prefix: &str,
        end: &str,
    ) {
        let n = pages.len();
        for (i, items) in pages.into_iter().enumerate() {
            let tok = format!("{prefix}{i}");
            let at = if i == 0 { first } else { At::Skip(&tok) };
            let next = format!("{prefix}{}", i + 1);
            let resp = if i + 1 < n {
                self.page(h, items, Some(&next), None)
            } else {
                self.page(h, items, None, Some(end))
            };
            self.mount(h, at, resp).await;
        }
    }
}

fn paths() -> Paths {
    Paths::rooted(std::env::temp_dir().join("nugget-microsoft-conformance"))
}

/// A connector whose `KnownItems` is `replica`.
fn build(
    h: &Harness,
    factory: &dyn ConnectorFactory,
    settings: &Value,
    replica: Arc<Replica>,
) -> Arc<dyn Connector> {
    let egress: Arc<dyn EgressSink> = h.egress.clone();
    let secrets: Arc<dyn SecretStore> = h.secrets.clone();
    factory
        .build(
            settings,
            testkit::context(SOURCE, paths(), egress, secrets, replica),
        )
        .unwrap()
}

fn ol(h: &Harness, retries: u32, r: &Arc<Replica>) -> Arc<dyn Connector> {
    build(
        h,
        &ol_factory(h, fast_retry(retries)),
        &ol_settings(),
        r.clone(),
    )
}

fn cal(h: &Harness, retries: u32, r: &Arc<Replica>) -> Arc<dyn Connector> {
    build(
        h,
        &cal_factory(h, fast_retry(retries)),
        &cal_settings(),
        r.clone(),
    )
}

fn od(h: &Harness, retries: u32, r: &Arc<Replica>) -> Arc<dyn Connector> {
    build(
        h,
        &od_factory(h, fast_retry(retries)),
        &od_settings(),
        r.clone(),
    )
}

/// One successful pass of `c` from `cursor`, applied to `r`.
async fn pass(c: &Arc<dyn Connector>, cursor: Option<Value>, r: &Arc<Replica>) -> Value {
    testkit::run_pass(&**c, cursor, &ReplicaSink::new(r.clone()))
        .await
        .unwrap()
}

/// Crash a pass at every send and checkpoint and require the clean run's index after resuming.
async fn crash_matrix(
    h: &Harness,
    factory: Arc<dyn ConnectorFactory>,
    settings: Value,
    start: Option<Value>,
    seed: BTreeMap<ItemKey, ReplicaItem>,
) -> testkit::CrashReport {
    let egress: Arc<dyn EgressSink> = h.egress.clone();
    let secrets: Arc<dyn SecretStore> = h.secrets.clone();
    let report = assert_crash_resumable(
        |r: Arc<Replica>| {
            let ctx = testkit::context(SOURCE, paths(), egress.clone(), secrets.clone(), r);
            let conn = factory.build(&settings, ctx).unwrap();
            async move { conn }
        },
        start,
        seed,
    )
    .await;
    assert!(report.sends > 0 && report.runs > 0, "{report:?}");
    report
}

/// No key upserted twice in one pass.
fn assert_no_duplicate_upserts(sink: &ReplicaSink) {
    let ups: Vec<String> = sink.upserts().iter().map(|d| d.key.to_string()).collect();
    let unique: std::collections::BTreeSet<&String> = ups.iter().collect();
    assert_eq!(unique.len(), ups.len(), "duplicate upserts: {ups:?}");
}

async fn mount_refresh_invalid_grant(h: &Harness) {
    Mock::given(method("POST"))
        .and(path(TOKEN_PATH))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "AADSTS700082: The refresh token has expired due to inactivity."
        })))
        .mount(&h.server)
        .await;
}

/// `401 InvalidAuthenticationToken` for requests with `Bearer <stale>` at `at` (priority 1).
async fn mount_rejected_token(h: &Harness, feed: &Feed, at: At<'_>, stale: &str) {
    feed.mock(at)
        .and(header("authorization", format!("Bearer {stale}").as_str()))
        .respond_with(ResponseTemplate::new(401).set_body_json(
            json!({"error": {"code": "InvalidAuthenticationToken", "message": "Lifetime validation failed, the token is expired."}}),
        ))
        .with_priority(1)
        .mount(&h.server)
        .await;
}

/// 429 whose `Retry-After` is an HTTP-date 30 s after the server's (skewed) `Date`.
fn throttled_http_date() -> ResponseTemplate {
    ResponseTemplate::new(429)
        .insert_header("Date", SERVER_DATE)
        .insert_header("Retry-After", SERVER_DATE_PLUS_30)
        .set_body_json(json!({"error": {"code": "TooManyRequests", "message": "throttled"}}))
}

/// Forget the mocks, the requests seen and the egress rows.
async fn reset(h: &Harness) {
    h.server.reset().await;
    h.egress.0.lock().unwrap().clear();
}

// ---------------------------------------------------------------------------------------------
// Outlook mail
// ---------------------------------------------------------------------------------------------

fn ol_factory(h: &Harness, retry: RetryPolicy) -> OutlookFactory {
    OutlookFactory::new()
        .with_endpoints(h.endpoints())
        .with_retry(retry)
}

fn ol_settings() -> Value {
    json!({"client_id": "cid", "folders": ["inbox"]})
}

fn recent() -> String {
    rfc3339(now() - 7 * 86_400)
}

fn ol_msg(id: &str, subject: &str, received: &str) -> Value {
    json!({
        "@odata.etag": "W/\"x\"",
        "id": id, "subject": subject, "receivedDateTime": received,
        "lastModifiedDateTime": received,
        "from": {"emailAddress": {"name": "Alice", "address": "alice@example.com"}},
        "conversationId": format!("conv-{id}"), "internetMessageId": format!("<{id}@example.com>"),
        "isDraft": false, "webLink": format!("https://outlook.office365.com/owa/?ItemID={id}"),
    })
}

fn ol_removed(id: &str) -> Value {
    json!({"id": id, "@removed": {"reason": "deleted"}})
}

fn mime_path(id: &str) -> String {
    format!("{API}/me/messages/{id}/$value")
}

/// Every `$value` answers with a small MIME message naming the id.
async fn mount_mime_any(h: &Harness) {
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1\.0/me/messages/[^/]+/\$value$"))
        .respond_with(|r: &wiremock::Request| {
            let id = r
                .url
                .path()
                .split('/')
                .nth(4)
                .unwrap_or_default()
                .to_string();
            ResponseTemplate::new(200)
                .set_body_string(format!("Subject: {id}\r\n\r\nBody of {id}\r\n"))
        })
        .mount(&h.server)
        .await;
}

/// Pages of `per` messages each, ids `<prefix><n>`.
fn ol_pages(prefix: &str, pages: usize, per: usize, received: &str) -> Vec<Vec<Value>> {
    (0..pages)
        .map(|p| {
            (0..per)
                .map(|i| {
                    let id = format!("{prefix}{}", p * per + i);
                    ol_msg(&id, &format!("Subject {id}"), received)
                })
                .collect()
        })
        .collect()
}

/// An HTTP-date `Retry-After` was ignored (only delta-seconds were parsed): the error carried no
/// wait, so the supervisor retried at once. It is now measured against the response's `Date`,
/// which also makes it immune to local clock skew. After the retry budget the error surfaces
/// with the progress committed, and a later pass resumes from there.
#[tokio::test]
async fn regression_retry_after_http_date() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let rec = recent();
    INBOX
        .mount(
            &h,
            At::Initial,
            INBOX.page(&h, vec![ol_msg("a", "A", &rec)], Some("s1"), None),
        )
        .await;
    INBOX
        .mock(At::Skip("s1"))
        .respond_with(throttled_http_date())
        .up_to_n_times(1)
        .mount(&h.server)
        .await;
    INBOX
        .mount(
            &h,
            At::Skip("s1"),
            INBOX.page(&h, vec![ol_msg("b", "B", &rec)], None, Some("d1")),
        )
        .await;
    mount_mime_any(&h).await;

    let r = Replica::new();
    let conn = ol(&h, 1, &r);
    let sink = ReplicaSink::new(r.clone());
    let err = conn
        .sync(None, &sink, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: Some(30_000)
            }
        ),
        "{err:?}"
    );
    assert_eq!(r.keys(), vec!["inbox/a"]);
    let committed = sink
        .committed()
        .expect("progress committed before the error");
    assert!(committed.to_string().contains("skiptoken=s1"));

    // A later pass resumes at page 2 without refetching page 1.
    let sink = ReplicaSink::new(r.clone());
    testkit::run_pass(&*conn, Some(committed), &sink)
        .await
        .unwrap();
    assert_eq!(sink.summary(), vec!["+inbox/b"]);
    assert_eq!(r.keys(), vec!["inbox/a", "inbox/b"]);
    assert_eq!(h.requests_to(&mime_path("a")).await.len(), 1);
    h.assert_egress("sync:outlook").await;
}

#[tokio::test]
async fn outlook_refresh_failure_is_unauthorized_never_partial_ok() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    INBOX
        .mount_chain(&h, At::Initial, ol_pages("m", 4, 2, &recent()), "s", "d1")
        .await;
    mount_rejected_token(&h, &INBOX, At::Skip("s2"), "at").await;
    mount_refresh_invalid_grant(&h).await;
    mount_mime_any(&h).await;

    let r = Replica::new();
    let conn = ol(&h, 2, &r);
    let sink = ReplicaSink::new(r.clone());
    let res = conn.sync(None, &sink, &CancellationToken::new()).await;
    assert!(is_unauthorized(&res), "{res:?}");
    // Progress up to the failure is committed; the rest waits for a reconnect.
    assert_eq!(r.len(), 4);
    let committed = sink.committed().unwrap();

    // After a reconnect the pass resumes where it stopped.
    h.seed_tokens("reconnected", false);
    let sink = ReplicaSink::new(r.clone());
    testkit::run_pass(&*conn, Some(committed), &sink)
        .await
        .unwrap();
    assert_eq!(r.len(), 8);
    assert_no_duplicate_upserts(&sink);
    h.assert_egress("sync:outlook").await;
}

/// One message whose MIME download keeps failing with a 5xx stopped the whole pass (and so
/// every later pass, since the delta never moved past it). It is now skipped, carried in the
/// cursor and fetched again on the next pass, so a partial outage loses nothing.
#[tokio::test]
async fn regression_outlook_one_failing_message_does_not_stop_the_pass() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let rec = recent();
    INBOX
        .mount(
            &h,
            At::Initial,
            INBOX.page(
                &h,
                vec![
                    ol_msg("good1", "G1", &rec),
                    ol_msg("bad", "B", &rec),
                    ol_msg("good2", "G2", &rec),
                ],
                None,
                Some("d1"),
            ),
        )
        .await;
    // A partial outage: `$value` fails for this message during the first pass only.
    Mock::given(method("GET"))
        .and(path(mime_path("bad")))
        .respond_with(server_error(500, None))
        .with_priority(1)
        .up_to_n_times(2)
        .mount(&h.server)
        .await;
    mount_mime_any(&h).await;
    let r = Replica::new();
    let conn = ol(&h, 1, &r);
    let cur = pass(&conn, None, &r).await;
    assert_eq!(r.keys(), vec!["inbox/good1", "inbox/good2"]);
    assert!(cur.to_string().contains("deltatoken=d1"));
    // Retried within budget, then carried in the cursor.
    assert_eq!(h.requests_to(&mime_path("bad")).await.len(), 2);
    assert_eq!(cur["retry"], json!([{"key": "inbox/bad", "attempts": 1}]));

    // The next pass (nothing new in the delta) fetches it again.
    INBOX
        .mount(
            &h,
            At::Delta("d1"),
            INBOX.page(&h, vec![], None, Some("d1")),
        )
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/messages/bad")))
        .respond_with(ResponseTemplate::new(200).set_body_json(ol_msg("bad", "B", &rec)))
        .mount(&h.server)
        .await;
    let sink = ReplicaSink::new(r.clone());
    let cur = testkit::run_pass(&*conn, Some(cur), &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+inbox/bad"]);
    assert!(cur.get("retry").is_none(), "{cur}");
    h.assert_egress("sync:outlook").await;
}

#[tokio::test]
async fn outlook_crash_resumable_backfill_and_delta() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let rec = recent();
    let factory: Arc<dyn ConnectorFactory> = Arc::new(ol_factory(&h, fast_retry(0)));
    // Backfill; a resume that lands on the delta link finds nothing new.
    INBOX
        .mount_chain(&h, At::Initial, ol_pages("m", 5, 2, &rec), "s", "d1")
        .await;
    INBOX
        .mount(
            &h,
            At::Delta("d1"),
            INBOX.page(&h, vec![], None, Some("d1")),
        )
        .await;
    mount_mime_any(&h).await;
    let report = crash_matrix(&h, factory.clone(), ol_settings(), None, BTreeMap::new()).await;
    assert!(report.sends >= 10 && report.checkpoints >= 5, "{report:?}");
    let r = Replica::new();
    let conn = build(&h, &*factory, &ol_settings(), r.clone());
    let cur = pass(&conn, None, &r).await;

    // An incremental pass over five pages, from the backfill's cursor.
    reset(&h).await;
    let mut inc = ol_pages("n", 5, 1, &rec);
    inc[1].push(ol_removed("m0"));
    inc[4].push(ol_removed("n0"));
    INBOX.mount_chain(&h, At::Delta("d1"), inc, "i", "d2").await;
    INBOX
        .mount(
            &h,
            At::Delta("d2"),
            INBOX.page(&h, vec![], None, Some("d2")),
        )
        .await;
    mount_mime_any(&h).await;
    let report = crash_matrix(&h, factory, ol_settings(), Some(cur), r.snapshot()).await;
    assert!(report.sends >= 5 && report.checkpoints >= 5, "{report:?}");
    h.assert_egress("sync:outlook").await;
}

// ---------------------------------------------------------------------------------------------
// Outlook calendar
// ---------------------------------------------------------------------------------------------

fn cal_factory(h: &Harness, retry: RetryPolicy) -> OutlookCalendarFactory {
    OutlookCalendarFactory::new()
        .with_endpoints(h.endpoints())
        .with_retry(retry)
}

fn cal_settings() -> Value {
    json!({"client_id": "cid"})
}

fn ev(id: &str, subject: &str) -> Value {
    ev_at(id, subject, "2026-11-03T18:00:00", "2026-11-03T19:30:00")
}

fn ev_at(id: &str, subject: &str, start: &str, end: &str) -> Value {
    json!({
        "id": id, "changeKey": format!("ck-{id}"), "subject": subject,
        "start": {"dateTime": format!("{start}.0000000"), "timeZone": "UTC"},
        "end": {"dateTime": format!("{end}.0000000"), "timeZone": "UTC"},
        "originalStartTimeZone": "UTC", "type": "singleInstance",
        "isAllDay": false, "isCancelled": false,
        "lastModifiedDateTime": "2026-10-20T08:00:00Z",
    })
}

fn ev_removed(id: &str) -> Value {
    json!({"id": id, "@removed": {"reason": "deleted"}})
}

fn when_line(r: &Replica, id: &str) -> String {
    let d = r.get(id).unwrap_or_else(|| panic!("{id} not indexed"));
    d.content
        .lines()
        .find(|l| l.starts_with("When: "))
        .unwrap_or_default()
        .to_string()
}

/// The calendar re-sent every listed event, unchanged or not: an event repeated on a later page
/// was upserted twice in one pass and a resync re-sent the whole calendar. Unchanged events
/// (same change key and span as indexed or already sent) are now skipped.
#[tokio::test]
async fn regression_calendar_unchanged_events_not_resent() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    CAL.mount(
        &h,
        At::Initial,
        CAL.page(&h, vec![ev("a", "A"), ev("b", "B")], Some("s1"), None),
    )
    .await;
    CAL.mount(
        &h,
        At::Skip("s1"),
        CAL.page(&h, vec![ev("a", "A")], None, Some("d1")),
    )
    .await;
    let r = Replica::new();
    let conn = cal(&h, 0, &r);
    let sink = ReplicaSink::new(r.clone());
    let cur = testkit::run_pass(&*conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+a", "+b"]);

    // An expired token: the relisting finds nothing new to send.
    reset(&h).await;
    CAL.mount(
        &h,
        At::Delta("d1"),
        ResponseTemplate::new(410).set_body_json(json!({"error": {"code": "syncStateNotFound"}})),
    )
    .await;
    let mut b2 = ev("b", "B, edited");
    b2["changeKey"] = "ck-b-2".into();
    CAL.mount(
        &h,
        At::Initial,
        CAL.page(&h, vec![ev("a", "A"), b2], None, Some("d2")),
    )
    .await;
    let sink = ReplicaSink::new(r.clone());
    testkit::run_pass(&*conn, Some(cur), &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+b"]);
    assert_eq!(r.get("b").unwrap().title.as_deref(), Some("B, edited"));
}

/// Recurrence: occurrences and exceptions are their own documents, cancelled instances are
/// deleted, and a series master that shows up next to its occurrences is not indexed as an
/// extra copy of the first occurrence.
#[tokio::test]
async fn regression_calendar_series_master_not_duplicated() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let mut master = ev_at("M", "Standup", "2026-07-06T07:00:00", "2026-07-06T07:15:00");
    master["type"] = "seriesMaster".into();
    master["recurrence"] = json!({"pattern": {"type": "weekly", "interval": 1, "daysOfWeek": ["monday"]},
        "range": {"type": "noEnd", "startDate": "2026-07-06", "recurrenceTimeZone": "W. Europe Standard Time"}});
    let occ = |id: &str, start: &str, end: &str| {
        let mut e = ev_at(id, "Standup", start, end);
        e["type"] = "occurrence".into();
        e["seriesMasterId"] = "M".into();
        e["originalStartTimeZone"] = "W. Europe Standard Time".into();
        e
    };
    // Summer (CEST, UTC+2) and winter (CET, UTC+1) instances of a 09:00 local meeting.
    let summer = occ("M-0706", "2026-07-06T07:00:00", "2026-07-06T07:15:00");
    let winter = occ("M-1207", "2026-12-07T08:00:00", "2026-12-07T08:15:00");
    let mut exception = occ("M-0713", "2026-07-13T12:00:00", "2026-07-13T12:30:00");
    exception["type"] = "exception".into();
    exception["subject"] = "Standup (moved)".into();
    let mut cancelled = occ("M-0720", "2026-07-20T07:00:00", "2026-07-20T07:15:00");
    cancelled["isCancelled"] = true.into();
    CAL.mount(
        &h,
        At::Initial,
        CAL.page(
            &h,
            vec![master, summer, winter, exception, cancelled.clone()],
            None,
            Some("d1"),
        ),
    )
    .await;
    // Later the whole series is cancelled instance by instance.
    CAL.mount(
        &h,
        At::Delta("d1"),
        CAL.page(
            &h,
            vec![ev_removed("M-0706"), ev_removed("M"), cancelled],
            None,
            Some("d2"),
        ),
    )
    .await;
    let r = Replica::new();
    let conn = cal(&h, 0, &r);
    let cur = pass(&conn, None, &r).await;
    assert_eq!(r.keys(), vec!["M-0706", "M-0713", "M-1207"]);
    assert_eq!(
        when_line(&r, "M-0706"),
        "When: 2026-07-06 09:00 – 09:15 (W. Europe Standard Time, UTC+02:00)"
    );
    assert_eq!(
        when_line(&r, "M-1207"),
        "When: 2026-12-07 09:00 – 09:15 (W. Europe Standard Time, UTC+01:00)"
    );
    assert_eq!(
        r.get("M-0713").unwrap().title.as_deref(),
        Some("Standup (moved)")
    );

    pass(&conn, Some(cur), &r).await;
    assert_eq!(r.keys(), vec!["M-0713", "M-1207"]);
}

// ---------------------------------------------------------------------------------------------
// OneDrive
// ---------------------------------------------------------------------------------------------

fn od_factory(h: &Harness, retry: RetryPolicy) -> OneDriveFactory {
    OneDriveFactory::new()
        .with_endpoints(h.endpoints())
        .with_retry(retry)
}

fn od_settings() -> Value {
    json!({"client_id": "cid"})
}

fn od_root() -> Value {
    json!({"id": "ROOT", "name": "root", "root": {}, "folder": {"childCount": 1}})
}

fn od_folder(id: &str, parent: &str, name: &str) -> Value {
    json!({"id": id, "name": name, "folder": {"childCount": 1}, "parentReference": {"id": parent}})
}

fn od_file(h: &Harness, id: &str, parent: &str, name: &str, etag: u32) -> Value {
    json!({
        "id": id, "name": name, "size": 20, "eTag": format!("\"{{{id}}},{etag}\""),
        "lastModifiedDateTime": "2026-10-01T12:00:00Z",
        "webUrl": format!("https://contoso-my.sharepoint.com/{id}"),
        "file": {"mimeType": "text/plain"}, "parentReference": {"id": parent},
        "@microsoft.graph.downloadUrl": format!("{}/dl/{id}", h.uri()),
    })
}

fn od_deleted(id: &str) -> Value {
    json!({"id": id, "deleted": {"state": "deleted"}, "file": {}, "parentReference": {"id": "ROOT"}})
}

fn dl_path(id: &str) -> String {
    format!("/dl/{id}")
}

/// Every `/dl/<id>` serves `content of <id>`.
async fn mount_downloads(h: &Harness) {
    Mock::given(method("GET"))
        .and(path_regex(r"^/dl/[^/]+$"))
        .respond_with(|r: &wiremock::Request| {
            let id = r.url.path().trim_start_matches("/dl/").to_string();
            ResponseTemplate::new(200).set_body_string(format!("content of {id}"))
        })
        .mount(&h.server)
        .await;
}

fn od_pages(h: &Harness, prefix: &str, pages: usize, per: usize) -> Vec<Vec<Value>> {
    (0..pages)
        .map(|p| {
            (0..per)
                .map(|i| {
                    let id = format!("{prefix}{}", p * per + i);
                    od_file(h, &id, "DOCS", &format!("{id}.txt"), 1)
                })
                .collect()
        })
        .collect()
}

/// The download host's HTTP-date `Retry-After` (with a skewed `Date`) surfaces once the budget
/// is spent: throttling is never mistaken for a bad file.
#[tokio::test]
async fn regression_onedrive_download_retry_after_http_date() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let p = vec![od_root(), od_file(&h, "t", "ROOT", "t.txt", 1)];
    DRIVE
        .mount(&h, At::Initial, DRIVE.page(&h, p, None, Some("d1")))
        .await;
    Mock::given(method("GET"))
        .and(path(dl_path("t")))
        .respond_with(throttled_http_date())
        .mount(&h.server)
        .await;
    let r = Replica::new();
    let conn = od(&h, 1, &r);
    let err = conn
        .sync(
            None,
            &ReplicaSink::new(r.clone()),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: Some(30_000)
            }
        ),
        "{err:?}"
    );
    assert!(r.is_empty());
}

#[tokio::test]
async fn onedrive_size_caps_skip_without_aborting() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let mut big = od_file(&h, "big", "ROOT", "big.txt", 1);
    big["size"] = (2 * 1024 * 1024).into();
    // Claims to be small but the download is over the cap: cut off, skipped.
    let liar = od_file(&h, "liar", "ROOT", "liar.txt", 1);
    let p = vec![
        od_root(),
        od_file(&h, "ok1", "ROOT", "ok1.txt", 1),
        big,
        liar,
        od_file(&h, "ok2", "ROOT", "ok2.txt", 1),
    ];
    DRIVE
        .mount(&h, At::Initial, DRIVE.page(&h, p, None, Some("d1")))
        .await;
    Mock::given(method("GET"))
        .and(path(dl_path("liar")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 1024 * 1024 + 1]))
        .with_priority(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/drive/items/liar/content")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 1024 * 1024 + 1]))
        .mount(&h.server)
        .await;
    mount_downloads(&h).await;
    let r = Replica::new();
    let conn = build(
        &h,
        &od_factory(&h, fast_retry(0)),
        &json!({"client_id": "cid", "max_file_mb": 1}),
        r.clone(),
    );
    pass(&conn, None, &r).await;
    assert_eq!(r.keys(), vec!["ok1", "ok2"]);
    assert!(
        h.requests_to(&dl_path("big")).await.is_empty(),
        "oversized file not downloaded"
    );
    h.assert_egress("sync:onedrive").await;
}

/// One file whose download keeps failing with a 5xx (on the download URL and on `/content`)
/// stopped the whole pass. It is now skipped and the rest of the drive syncs.
#[tokio::test]
async fn regression_onedrive_one_failing_download_does_not_stop_the_pass() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let p = vec![
        od_root(),
        od_file(&h, "ok1", "ROOT", "ok1.txt", 1),
        od_file(&h, "bad", "ROOT", "bad.txt", 1),
        json!({"id": "malformed", "size": "huge", "file": {}}),
        json!({"id": "a/b", "name": "x.txt", "file": {}}),
        json!(42),
        od_file(&h, "ok2", "ROOT", "ok2.txt", 1),
    ];
    DRIVE
        .mount(&h, At::Initial, DRIVE.page(&h, p, None, Some("d1")))
        .await;
    // A partial outage during the first pass only.
    Mock::given(method("GET"))
        .and(path(dl_path("bad")))
        .respond_with(server_error(500, None))
        .with_priority(1)
        .up_to_n_times(2)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/drive/items/bad/content")))
        .respond_with(server_error(502, None))
        .up_to_n_times(2)
        .mount(&h.server)
        .await;
    mount_downloads(&h).await;
    let r = Replica::new();
    let conn = od(&h, 1, &r);
    let cur = pass(&conn, None, &r).await;
    assert_eq!(r.keys(), vec!["ok1", "ok2"]);
    assert!(cur["delta_link"].as_str().unwrap().contains("token=d1"));
    assert_eq!(h.requests_to(&dl_path("bad")).await.len(), 2);
    assert_eq!(cur["retry"], json!([{"key": "bad", "attempts": 1}]));

    // The next pass fetches its metadata and content again.
    DRIVE
        .mount(
            &h,
            At::Delta("d1"),
            DRIVE.page(&h, vec![], None, Some("d1")),
        )
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/drive/items/bad")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(od_file(&h, "bad", "ROOT", "bad.txt", 1)),
        )
        .mount(&h.server)
        .await;
    let sink = ReplicaSink::new(r.clone());
    let cur = testkit::run_pass(&*conn, Some(cur), &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+bad"]);
    assert_eq!(r.get("bad").unwrap().content, "content of bad");
    assert!(cur.get("retry").is_none(), "{cur}");
    h.assert_egress("sync:onedrive").await;
}

#[tokio::test]
async fn onedrive_crash_resumable_backfill_and_delta() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let factory: Arc<dyn ConnectorFactory> = Arc::new(od_factory(&h, fast_retry(0)));
    let mut pages = od_pages(&h, "f", 5, 2);
    pages[0].insert(0, od_root());
    pages[0].insert(1, od_folder("DOCS", "ROOT", "Docs"));
    pages[2].push(od_folder("OLD", "ROOT", "Old"));
    pages[3].push(od_file(&h, "o1", "OLD", "o1.txt", 1));
    DRIVE.mount_chain(&h, At::Initial, pages, "p", "d1").await;
    DRIVE
        .mount(
            &h,
            At::Delta("d1"),
            DRIVE.page(&h, vec![], None, Some("d1")),
        )
        .await;
    mount_downloads(&h).await;
    let report = crash_matrix(&h, factory.clone(), od_settings(), None, BTreeMap::new()).await;
    assert!(report.sends >= 10 && report.checkpoints >= 5, "{report:?}");
    let r = Replica::new();
    let conn = build(&h, &*factory, &od_settings(), r.clone());
    let cur = pass(&conn, None, &r).await;
    assert!(r.get("o1").is_some());

    reset(&h).await;
    let mut inc = od_pages(&h, "n", 5, 1);
    inc[1].push(od_deleted("f0"));
    inc[2].push(od_file(&h, "f1", "DOCS", "f1 renamed.txt", 2));
    inc[3].push(json!({"id": "OLD", "deleted": {"state": "deleted"}, "folder": {}}));
    inc[4].push(od_deleted("n0"));
    DRIVE.mount_chain(&h, At::Delta("d1"), inc, "i", "d2").await;
    DRIVE
        .mount(
            &h,
            At::Delta("d2"),
            DRIVE.page(&h, vec![], None, Some("d2")),
        )
        .await;
    mount_downloads(&h).await;
    let report = crash_matrix(
        &h,
        factory.clone(),
        od_settings(),
        Some(cur.clone()),
        r.snapshot(),
    )
    .await;
    assert!(report.sends >= 5 && report.checkpoints >= 5, "{report:?}");
    pass(&conn, Some(cur), &r).await;
    assert!(r.get("o1").is_none() && r.get("f0").is_none() && r.get("n0").is_none());
    assert_eq!(
        r.get("f1").unwrap().title.as_deref(),
        Some("f1 renamed.txt")
    );
    h.assert_egress("sync:onedrive").await;
}

/// A delta entry that is not a JSON object (`["<id>"]`) deserialized from its array form: in
/// OneDrive it read as an item without a file facet and deleted the indexed file with that id;
/// in the calendar it became an empty "(no title)" event. Such entries are now skipped.
#[tokio::test]
async fn regression_non_object_delta_entries_are_skipped() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    DRIVE
        .mount(
            &h,
            At::Initial,
            DRIVE.page(
                &h,
                vec![od_root(), od_file(&h, "a", "ROOT", "a.txt", 1)],
                None,
                Some("d1"),
            ),
        )
        .await;
    DRIVE
        .mount(
            &h,
            At::Delta("d1"),
            DRIVE.page(&h, vec![json!(["a"])], None, Some("d2")),
        )
        .await;
    CAL.mount(
        &h,
        At::Initial,
        CAL.page(
            &h,
            vec![json!(["garbage"]), ev("e1", "Real")],
            None,
            Some("d1"),
        ),
    )
    .await;
    INBOX
        .mount(
            &h,
            At::Initial,
            INBOX.page(&h, vec![json!(["m", recent()])], None, Some("d1")),
        )
        .await;
    mount_downloads(&h).await;
    mount_mime_any(&h).await;
    let r = Replica::new();
    let conn = od(&h, 0, &r);
    let cur = pass(&conn, None, &r).await;
    let sink = ReplicaSink::new(r.clone());
    testkit::run_pass(&*conn, Some(cur), &sink).await.unwrap();
    assert!(sink.summary().is_empty(), "{:?}", sink.summary());
    assert_eq!(r.keys(), vec!["a"]);

    let r = Replica::new();
    let conn = cal(&h, 0, &r);
    pass(&conn, None, &r).await;
    assert_eq!(r.keys(), vec!["e1"]);

    let r = Replica::new();
    let conn = ol(&h, 0, &r);
    pass(&conn, None, &r).await;
    assert!(r.is_empty());
    assert!(h.requests_to(&mime_path("m")).await.is_empty());
    let requests = h.server.received_requests().await.unwrap().len();
    let rows = h.egress.0.lock().unwrap().clone();
    assert_eq!(rows.len(), requests, "one egress row per request");
    let features: std::collections::BTreeSet<&str> =
        rows.iter().map(|e| e.feature.as_str()).collect();
    assert_eq!(
        features,
        ["sync:onedrive", "sync:outlook", "sync:outlook-calendar"].into()
    );
}

// ---------------------------------------------------------------------------------------------
// Item failures: retried on later passes, outages and token failures end the pass
// ---------------------------------------------------------------------------------------------

/// `$value` for `id` always answers `resp` (priority 1, so it wins over `mount_mime_any`).
async fn mount_mime_answer(h: &Harness, id: &str, resp: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(mime_path(id)))
        .respond_with(resp)
        .with_priority(1)
        .mount(&h.server)
        .await;
}

/// A skipped message was lost for good: the delta moved on and nothing recorded it. Skipped
/// messages are now fetched again on later passes, deleted when gone upstream, and given up
/// after `MAX_ITEM_ATTEMPTS` tries.
#[tokio::test]
async fn regression_skipped_items_retried_deleted_or_given_up() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let rec = recent();
    INBOX
        .mount(
            &h,
            At::Initial,
            INBOX.page(
                &h,
                vec![
                    ol_msg("ok", "OK", &rec),
                    ol_msg("gone", "G", &rec),
                    ol_msg("stuck", "S", &rec),
                ],
                None,
                Some("d1"),
            ),
        )
        .await;
    INBOX
        .mount(
            &h,
            At::Delta("d1"),
            INBOX.page(&h, vec![], None, Some("d1")),
        )
        .await;
    mount_mime_answer(&h, "gone", server_error(500, None)).await;
    mount_mime_answer(&h, "stuck", server_error(500, None)).await;
    mount_mime_any(&h).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/messages/gone")))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error": {"code": "ErrorItemNotFound"}})),
        )
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/messages/stuck")))
        .respond_with(ResponseTemplate::new(200).set_body_json(ol_msg("stuck", "S", &rec)))
        .mount(&h.server)
        .await;
    let r = Replica::new();
    let conn = ol(&h, 0, &r);
    let mut cur = pass(&conn, None, &r).await;
    assert_eq!(
        cur["retry"],
        json!([{"key": "inbox/gone", "attempts": 1}, {"key": "inbox/stuck", "attempts": 1}])
    );
    let sink = ReplicaSink::new(r.clone());
    cur = testkit::run_pass(&*conn, Some(cur), &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["-inbox/gone"]);
    assert_eq!(cur["retry"], json!([{"key": "inbox/stuck", "attempts": 2}]));
    for attempts in 3..crate::graph::MAX_ITEM_ATTEMPTS {
        cur = pass(&conn, Some(cur), &r).await;
        assert_eq!(
            cur["retry"],
            json!([{"key": "inbox/stuck", "attempts": attempts}])
        );
    }
    cur = pass(&conn, Some(cur), &r).await;
    assert!(cur.get("retry").is_none(), "given up: {cur}");
    assert_eq!(r.keys(), vec!["inbox/ok"]);
    h.assert_egress("sync:outlook").await;
}

/// Every item fetch failing is an outage, not a run of bad items: the pass ends (`Network`)
/// instead of succeeding with nothing.
#[tokio::test]
async fn regression_item_outage_ends_the_pass() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let rec = recent();
    let msgs: Vec<Value> = (0..8)
        .map(|i| ol_msg(&format!("m{i}"), "x", &rec))
        .collect();
    INBOX
        .mount(&h, At::Initial, INBOX.page(&h, msgs, None, Some("d1")))
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"/\$value$"))
        .respond_with(server_error(503, None))
        .mount(&h.server)
        .await;
    let mut files = vec![od_root()];
    files.extend((0..8).map(|i| od_file(&h, &format!("f{i}"), "ROOT", "f.txt", 1)));
    DRIVE
        .mount(&h, At::Initial, DRIVE.page(&h, files, None, Some("d1")))
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/dl/"))
        .respond_with(server_error(500, None))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"/content$"))
        .respond_with(server_error(500, None))
        .mount(&h.server)
        .await;

    let r = Replica::new();
    let conn = ol(&h, 0, &r);
    let sink = ReplicaSink::new(r.clone());
    let res = conn.sync(None, &sink, &CancellationToken::new()).await;
    assert!(matches!(res, Err(Error::Network(_))), "{res:?}");
    assert!(sink.committed().is_none() && r.is_empty());

    let conn = od(&h, 0, &r);
    let sink = ReplicaSink::new(r.clone());
    let res = conn.sync(None, &sink, &CancellationToken::new()).await;
    assert!(matches!(res, Err(Error::Network(_))), "{res:?}");
    assert!(sink.committed().is_none() && r.is_empty());
}

/// A token refresh failing with anything but `invalid_grant` (AAD `interaction_required`,
/// keychain trouble, a malformed token answer) came back as `Other`/`Invalid` and was taken for
/// an item failure: every fetch was "skipped" and the pass succeeded. Token failures now always
/// end the pass, and nothing is recorded as skipped.
#[tokio::test]
async fn regression_token_failure_on_message_fetch_ends_the_pass() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let rec = recent();
    INBOX
        .mount(
            &h,
            At::Initial,
            INBOX.page(
                &h,
                vec![ol_msg("a", "A", &rec), ol_msg("b", "B", &rec)],
                None,
                Some("d1"),
            ),
        )
        .await;
    Mock::given(method("GET"))
        .and(path_regex(r"/\$value$"))
        .and(header("authorization", "Bearer at"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error": {"code": "InvalidAuthenticationToken"}})),
        )
        .mount(&h.server)
        .await;
    Mock::given(method("POST"))
        .and(path(TOKEN_PATH))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "interaction_required",
            "error_description": "AADSTS50076: multi-factor authentication required."
        })))
        .mount(&h.server)
        .await;
    let r = Replica::new();
    let conn = ol(&h, 2, &r);
    let sink = ReplicaSink::new(r.clone());
    let res = conn.sync(None, &sink, &CancellationToken::new()).await;
    assert!(res.is_err(), "{res:?}");
    assert!(r.is_empty());
    assert!(sink.committed().is_none(), "nothing recorded as skipped");
    h.assert_egress("sync:outlook").await;
}

/// A rejected token on `/content` refreshes; a revoked refresh token is `Unauthorized`
/// (reconnect), never a skipped file.
#[tokio::test]
async fn regression_rejected_token_on_file_content_is_unauthorized() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let mut f = od_file(&h, "a", "ROOT", "a.txt", 1);
    f.as_object_mut()
        .unwrap()
        .remove("@microsoft.graph.downloadUrl");
    DRIVE
        .mount(
            &h,
            At::Initial,
            DRIVE.page(&h, vec![od_root(), f], None, Some("d1")),
        )
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/drive/items/a/content")))
        .and(header("authorization", "Bearer at"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&h.server)
        .await;
    mount_refresh_invalid_grant(&h).await;
    let r = Replica::new();
    let conn = od(&h, 1, &r);
    let sink = ReplicaSink::new(r.clone());
    let res = conn.sync(None, &sink, &CancellationToken::new()).await;
    assert!(is_unauthorized(&res), "{res:?}");
    assert!(r.is_empty() && sink.committed().is_none());
    h.assert_egress("sync:onedrive").await;
}

/// A 403 for one item (a policy- or malware-blocked file, a protected message) failed every
/// pass with "reconnect the account". It is now an item failure: skipped and retried later.
#[tokio::test]
async fn regression_item_access_denied_is_skipped_not_reconnect() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let rec = recent();
    INBOX
        .mount(
            &h,
            At::Initial,
            INBOX.page(
                &h,
                vec![ol_msg("ok", "OK", &rec), ol_msg("irm", "Protected", &rec)],
                None,
                Some("d1"),
            ),
        )
        .await;
    mount_mime_answer(
        &h,
        "irm",
        ResponseTemplate::new(403).set_body_json(
            json!({"error": {"code": "ErrorAccessDenied", "message": "Access is denied."}}),
        ),
    )
    .await;
    mount_mime_any(&h).await;
    let mut blocked = od_file(&h, "mal", "ROOT", "mal.txt", 1);
    blocked
        .as_object_mut()
        .unwrap()
        .remove("@microsoft.graph.downloadUrl");
    DRIVE
        .mount(
            &h,
            At::Initial,
            DRIVE.page(
                &h,
                vec![od_root(), od_file(&h, "ok", "ROOT", "ok.txt", 1), blocked],
                None,
                Some("d1"),
            ),
        )
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/drive/items/mal/content")))
        .respond_with(ResponseTemplate::new(403).set_body_json(
            json!({"error": {"code": "accessDenied", "message": "The file is infected with a virus."}}),
        ))
        .mount(&h.server)
        .await;
    mount_downloads(&h).await;

    let r = Replica::new();
    let conn = ol(&h, 0, &r);
    let cur = pass(&conn, None, &r).await;
    assert_eq!(r.keys(), vec!["inbox/ok"]);
    assert_eq!(cur["retry"], json!([{"key": "inbox/irm", "attempts": 1}]));

    let r = Replica::new();
    let conn = od(&h, 0, &r);
    let cur = pass(&conn, None, &r).await;
    assert_eq!(r.keys(), vec!["ok"]);
    assert_eq!(cur["retry"], json!([{"key": "mal", "attempts": 1}]));

    // A 403 about the account (not one item) still ends the pass as "reconnect".
    reset(&h).await;
    INBOX
        .mount(
            &h,
            At::Initial,
            INBOX.page(&h, vec![ol_msg("x", "X", &rec)], None, Some("d1")),
        )
        .await;
    mount_mime_answer(
        &h,
        "x",
        ResponseTemplate::new(403)
            .set_body_json(json!({"error": {"code": "Authorization_RequestDenied"}})),
    )
    .await;
    let conn = ol(&h, 0, &Replica::new());
    let res = conn
        .sync(
            None,
            &ReplicaSink::new(Replica::new()),
            &CancellationToken::new(),
        )
        .await;
    assert!(is_unauthorized(&res), "{res:?}");
}

/// Series masters indexed before they were skipped stayed as duplicates forever (a resync
/// skipped them, and the missing-event check found them still there). A listed master that is
/// indexed is now deleted, and the check treats a master as gone.
#[tokio::test]
async fn regression_calendar_indexed_series_master_removed_on_resync() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let t = now();
    let seed = Replica::new();
    for id in ["M1", "M2"] {
        let mut d = Document::new(id, "text/plain", Content::Text(format!("master {id}")));
        d.version = Some(format!("ck-{id}#{t}:{}", t + 900));
        seed.apply(&nugget_connector::Change::Upsert(d));
    }
    let mut m1 = ev("M1", "Standup");
    m1["type"] = "seriesMaster".into();
    // No cursor while events are indexed: a reconciling resync.
    CAL.mount(
        &h,
        At::Initial,
        CAL.page(&h, vec![m1, ev("e1", "One")], None, Some("d1")),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/events/M2")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "M2", "type": "seriesMaster", "isCancelled": false})),
        )
        .mount(&h.server)
        .await;
    let conn = cal(&h, 0, &seed);
    let sink = ReplicaSink::new(seed.clone());
    testkit::run_pass(&*conn, None, &sink).await.unwrap();
    assert_eq!(seed.keys(), vec!["e1"]);
    assert_eq!(sink.summary(), vec!["-M1", "+e1", "-M2"]);
}
