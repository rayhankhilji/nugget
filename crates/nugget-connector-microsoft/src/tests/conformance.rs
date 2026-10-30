//! Conformance suite (FF2) for the three Microsoft 365 connectors, against wiremock with
//! synthetic and recorded-and-scrubbed fixtures: paging, throttling, token refresh, deletes,
//! moves and renames, deltas and resyncs, size caps, unicode and HTML, recurrence and time
//! zones, clock skew, partial failure, crash resumption and egress.

use std::collections::BTreeMap;

use nugget_connector::testkit::{
    self, Replica, ReplicaItem, ReplicaSink, Sequence, UNICODE_SAMPLES, assert_crash_resumable,
    assert_scrubbed, big_text, html_mail, server_error, too_many_requests,
};
use nugget_connector::{Content, Document};
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
const SENT: Feed = Feed {
    path: "/v1.0/me/mailFolders/sentitems/messages/delta",
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

fn text_of(d: &ReplicaItem) -> &str {
    &d.content
}

fn doc_text(d: &Document) -> String {
    match &d.content {
        Content::Text(t) => t.clone(),
        Content::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        Content::File(_) => String::new(),
    }
}

/// What `clean_line` makes of a title: control characters become spaces, then trimmed.
fn cleaned(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

async fn mount_refresh_ok(h: &Harness, access: &str) {
    Mock::given(method("POST"))
        .and(path(TOKEN_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": access, "expires_in": 3600})),
        )
        .mount(&h.server)
        .await;
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

/// 509 (bandwidth limit) with an HTTP-date `Retry-After` equal to the server's `Date`.
fn bandwidth_exceeded_http_date() -> ResponseTemplate {
    ResponseTemplate::new(509)
        .insert_header("Date", SERVER_DATE)
        .insert_header("Retry-After", SERVER_DATE)
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

async fn token_requests(h: &Harness) -> usize {
    h.requests_to(TOKEN_PATH).await.len()
}

#[test]
fn backoff_is_exponential_capped_and_jittered() {
    let policy = RetryPolicy {
        max_retries: 8,
        base_delay: Duration::from_millis(100),
        max_delay: Duration::from_secs(2),
    };
    let net = Error::Network("x".into());
    for attempt in 0..8u32 {
        let exp = Duration::from_millis(100 * (1u64 << attempt)).min(policy.max_delay);
        let d = crate::graph::backoff(&policy, attempt, &net).unwrap();
        assert!(d >= exp && d <= exp + exp / 4, "attempt {attempt}: {d:?}");
    }
    // A server hint is a floor, never shortened by the cap or the jitter.
    let hinted = Error::RateLimited {
        retry_after_ms: Some(1_500),
    };
    let d = crate::graph::backoff(&policy, 0, &hinted).unwrap();
    assert!(d >= Duration::from_millis(1_500), "{d:?}");
}

/// A `Retry-After` longer than `max_delay` was cut down to `max_delay` and retried early,
/// inviting more throttling. It is now a floor: when it exceeds what a pass may wait, the error
/// is returned at once (carrying the full wait) for the supervisor to honour.
#[test]
fn regression_retry_after_is_a_floor_not_capped() {
    let policy = RetryPolicy {
        max_retries: 8,
        base_delay: Duration::from_millis(100),
        max_delay: Duration::from_secs(2),
    };
    let long = Error::RateLimited {
        retry_after_ms: Some(30_000),
    };
    assert_eq!(crate::graph::backoff(&policy, 0, &long), None);
    let at_cap = Error::RateLimited {
        retry_after_ms: Some(2_000),
    };
    assert!(crate::graph::backoff(&policy, 0, &at_cap).unwrap() >= Duration::from_secs(2));
}

// ---------------------------------------------------------------------------------------------
// Recorded fixtures (recorded from Graph, then scrubbed: tokens and real addresses removed).
// `{BASE}` is replaced by the mock server's URL.
// ---------------------------------------------------------------------------------------------

const RECORDED_MESSAGES: &str = r#"{
  "@odata.context": "https://graph.microsoft.com/v1.0/$metadata#Collection(message)",
  "value": [
    {
      "@odata.etag": "W/\"CQAAABYAAADeK2Y0BbVCQbGgD5O4rI2hAAAI0bfx\"",
      "id": "AAkALgAAAAAAHYQDEapmEc2byACqAC-EWg0A3itmNAW1QkGxoA-TuKyNoQAACNI4LwAA",
      "receivedDateTime": "{RECENT}",
      "lastModifiedDateTime": "{RECENT}",
      "subject": "Q3 planning — agenda",
      "isDraft": false,
      "conversationId": "AAQkAGI2TG93AAA=",
      "internetMessageId": "<BN6PR11MB0001@prod.example.com>",
      "webLink": "https://outlook.office365.com/owa/?ItemID=AAkALgAAAAAAHYQDEapmEc2byACqAC&exvsurl=1",
      "from": {"emailAddress": {"name": "Megan Bowen", "address": "user@example.com"}}
    }
  ],
  "@odata.deltaLink": "{BASE}/v1.0/me/mailFolders/inbox/messages/delta?$deltatoken=recorded"
}"#;

const RECORDED_EVENTS: &str = r#"{
  "@odata.context": "https://graph.microsoft.com/v1.0/$metadata#Collection(event)",
  "value": [
    {
      "@odata.etag": "W/\"3itmNAW1QkGxoA+TuKyNoQAACNP7cw==\"",
      "id": "AAMkAGI2TG93AAA-rec1",
      "changeKey": "3itmNAW1QkGxoA+TuKyNoQAACNP7cw==",
      "subject": "Weekly sync",
      "type": "occurrence",
      "seriesMasterId": "AAMkAGI2TG93AAA-master",
      "isAllDay": false,
      "isCancelled": false,
      "originalStartTimeZone": "Pacific Standard Time",
      "start": {"dateTime": "2026-07-07T16:00:00.0000000", "timeZone": "UTC"},
      "end": {"dateTime": "2026-07-07T16:30:00.0000000", "timeZone": "UTC"},
      "organizer": {"emailAddress": {"name": "Adele Vance", "address": "user@example.com"}},
      "attendees": [{"type": "required", "status": {"response": "accepted"}, "emailAddress": {"name": "Alex Wilber", "address": "user@example.com"}}],
      "onlineMeeting": {"joinUrl": "https://teams.microsoft.com/l/meetup-join/19%3ameeting_rec"},
      "webLink": "https://outlook.office365.com/owa/?itemid=AAMkAGI2TG93AAA-rec1",
      "lastModifiedDateTime": "2026-06-30T08:00:00Z"
    }
  ],
  "@odata.deltaLink": "{BASE}/v1.0/me/calendarView/delta?$deltatoken=recorded"
}"#;

const RECORDED_DRIVE: &str = r#"{
  "@odata.context": "https://graph.microsoft.com/v1.0/$metadata#Collection(driveItem)",
  "value": [
    {"id": "01ROOT", "name": "root", "root": {}, "folder": {"childCount": 1}},
    {
      "id": "01BYE5RZ6QN3ZWBTUFOFD3GSPGOHDJD36K",
      "name": "Contoso Q3 plan.txt",
      "size": 42,
      "eTag": "\"{6E7A3E86-B4C8-4F87-95FC-3D7CF1B6F2C8},3\"",
      "cTag": "\"c:{6E7A3E86-B4C8-4F87-95FC-3D7CF1B6F2C8},3\"",
      "lastModifiedDateTime": "2026-09-30T12:00:00Z",
      "webUrl": "https://contoso-my.sharepoint.com/personal/user_example_com/Documents/Contoso%20Q3%20plan.txt",
      "file": {"mimeType": "text/plain"},
      "parentReference": {"id": "01ROOT", "driveId": "b!x"},
      "createdBy": {"user": {"displayName": "Megan Bowen", "email": "user@example.com"}},
      "@microsoft.graph.downloadUrl": "{BASE}/dl/01BYE5RZ6QN3ZWBTUFOFD3GSPGOHDJD36K"
    }
  ],
  "@odata.deltaLink": "{BASE}/v1.0/me/drive/root/delta?token=recorded"
}"#;

fn recorded(raw: &str, h: &Harness) -> ResponseTemplate {
    let body = raw
        .replace("{BASE}", &h.uri())
        .replace("{RECENT}", &rfc3339(now() - 3 * 86_400));
    ResponseTemplate::new(200).set_body_raw(body, "application/json")
}

#[test]
fn recorded_fixtures_are_scrubbed() {
    for f in [RECORDED_MESSAGES, RECORDED_EVENTS, RECORDED_DRIVE] {
        assert_scrubbed(f);
    }
}

#[tokio::test]
async fn recorded_fixtures_sync_for_every_kind() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    INBOX
        .mount(&h, At::Initial, recorded(RECORDED_MESSAGES, &h))
        .await;
    CAL.mount(&h, At::Initial, recorded(RECORDED_EVENTS, &h))
        .await;
    DRIVE
        .mount(&h, At::Initial, recorded(RECORDED_DRIVE, &h))
        .await;
    mount_mime_any(&h).await;
    mount_downloads(&h).await;

    let r = Replica::new();
    let ol = build(
        &h,
        &ol_factory(&h, fast_retry(0)),
        &ol_settings(),
        r.clone(),
    );
    testkit::run_pass(&*ol, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
    let r2 = Replica::new();
    let cal = build(
        &h,
        &cal_factory(&h, fast_retry(0)),
        &cal_settings(),
        r2.clone(),
    );
    testkit::run_pass(&*cal, None, &ReplicaSink::new(r2.clone()))
        .await
        .unwrap();
    let r3 = Replica::new();
    let od = build(
        &h,
        &od_factory(&h, fast_retry(0)),
        &od_settings(),
        r3.clone(),
    );
    testkit::run_pass(&*od, None, &ReplicaSink::new(r3.clone()))
        .await
        .unwrap();

    assert_eq!(r.len(), 1);
    assert_eq!(
        r.snapshot().values().next().unwrap().title.as_deref(),
        Some("Q3 planning — agenda")
    );
    let ev = r2.get("AAMkAGI2TG93AAA-rec1").unwrap();
    assert!(
        text_of(&ev).contains("When: 2026-07-07 09:00 – 09:30 (Pacific Standard Time, UTC-07:00)"),
        "{}",
        ev.content
    );
    let f = r3.get("01BYE5RZ6QN3ZWBTUFOFD3GSPGOHDJD36K").unwrap();
    assert_eq!(f.title.as_deref(), Some("Contoso Q3 plan.txt"));
    assert_eq!(f.mime, "text/plain");
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

#[tokio::test]
async fn outlook_pages_through_five_pages_without_drops_or_duplicates() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let rec = recent();
    let mut pages = ol_pages("m", 5, 3, &rec);
    // Graph repeats an item on a later page now and then.
    pages[2].push(ol_msg("m5", "Subject m5", &rec));
    INBOX.mount_chain(&h, At::Initial, pages, "s", "d1").await;
    // Incremental: five pages more, with one removal.
    let mut inc = ol_pages("n", 5, 1, &rec);
    inc[3].push(ol_removed("m0"));
    INBOX.mount_chain(&h, At::Delta("d1"), inc, "i", "d2").await;
    mount_mime_any(&h).await;

    let r = Replica::new();
    let conn = build(
        &h,
        &ol_factory(&h, fast_retry(0)),
        &ol_settings(),
        r.clone(),
    );
    let sink = ReplicaSink::new(r.clone());
    let cur = testkit::run_pass(&*conn, None, &sink).await.unwrap();
    assert_no_duplicate_upserts(&sink);
    assert_eq!(r.len(), 15);
    assert_eq!(h.server.received_requests().await.unwrap().len(), 5 + 15);
    assert!(
        cur["folders"]["inbox"]["delta_link"]
            .as_str()
            .unwrap()
            .contains("deltatoken=d1")
    );
    // Each non-final page committed a resumable cursor.
    assert!(sink.checkpoint_calls() >= 5);

    let sink = ReplicaSink::new(r.clone());
    let cur = testkit::run_pass(&*conn, Some(cur), &sink).await.unwrap();
    assert_no_duplicate_upserts(&sink);
    assert_eq!(r.len(), 15 + 5 - 1);
    assert!(r.get("inbox/m0").is_none() && r.get("inbox/n4").is_some());
    assert!(
        cur["folders"]["inbox"]["delta_link"]
            .as_str()
            .unwrap()
            .contains("deltatoken=d2")
    );
    h.assert_egress("sync:outlook").await;
}

#[tokio::test]
async fn outlook_retries_429_503_509_with_retry_after_and_backoff() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let rec = recent();
    INBOX
        .mock(At::Initial)
        .respond_with(Sequence::new(vec![
            too_many_requests(0),
            server_error(503, Some(0)),
            bandwidth_exceeded_http_date(),
            server_error(500, None),
            INBOX.page(&h, vec![ol_msg("a", "A", &rec)], None, Some("d1")),
        ]))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(mime_path("a")))
        .respond_with(Sequence::new(vec![
            server_error(503, None),
            ResponseTemplate::new(200).set_body_string("Subject: a\r\n\r\nbody"),
        ]))
        .mount(&h.server)
        .await;
    let r = Replica::new();
    let conn = build(
        &h,
        &ol_factory(&h, fast_retry(4)),
        &ol_settings(),
        r.clone(),
    );
    testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
    assert_eq!(r.keys(), vec!["inbox/a"]);
    assert_eq!(h.requests_to(INBOX.path).await.len(), 5);
    assert_eq!(h.requests_to(&mime_path("a")).await.len(), 2);
    h.assert_egress("sync:outlook").await;
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
    let conn = build(
        &h,
        &ol_factory(&h, fast_retry(1)),
        &ol_settings(),
        r.clone(),
    );
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
async fn outlook_token_expiry_mid_sync_refreshes_and_continues() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    INBOX
        .mount_chain(&h, At::Initial, ol_pages("m", 4, 2, &recent()), "s", "d1")
        .await;
    // The token expires (server side) before page 3.
    mount_rejected_token(&h, &INBOX, At::Skip("s2"), "at").await;
    mount_refresh_ok(&h, "fresh").await;
    mount_mime_any(&h).await;

    let r = Replica::new();
    let conn = build(
        &h,
        &ol_factory(&h, fast_retry(0)),
        &ol_settings(),
        r.clone(),
    );
    testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
    assert_eq!(r.len(), 8);
    assert_eq!(token_requests(&h).await, 1);
    let page3 = h.requests_to(INBOX.path).await;
    let last = page3.last().unwrap();
    assert_eq!(last.headers.get("authorization").unwrap(), "Bearer fresh");
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
    let conn = build(
        &h,
        &ol_factory(&h, fast_retry(2)),
        &ol_settings(),
        r.clone(),
    );
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

/// Clock skew, local clock ahead: the stored token looks expired, so it is refreshed before use;
/// a revoked refresh token is `Unauthorized` before any Graph request.
#[tokio::test]
async fn outlook_locally_expired_token_is_refreshed_first_or_unauthorized() {
    let h = Harness::new().await;
    h.seed_tokens("old", true);
    INBOX
        .mount(&h, At::Initial, INBOX.page(&h, vec![], None, Some("d1")))
        .await;
    mount_refresh_invalid_grant(&h).await;
    let conn = build(
        &h,
        &ol_factory(&h, fast_retry(0)),
        &ol_settings(),
        Replica::new(),
    );
    let res = conn
        .sync(
            None,
            &ReplicaSink::new(Replica::new()),
            &CancellationToken::new(),
        )
        .await;
    assert!(is_unauthorized(&res), "{res:?}");
    assert!(h.requests_to(INBOX.path).await.is_empty());

    h.server.reset().await;
    h.seed_tokens("old", true);
    INBOX
        .mount(&h, At::Initial, INBOX.page(&h, vec![], None, Some("d1")))
        .await;
    mount_refresh_ok(&h, "new").await;
    let conn = build(
        &h,
        &ol_factory(&h, fast_retry(0)),
        &ol_settings(),
        Replica::new(),
    );
    testkit::run_pass(&*conn, None, &ReplicaSink::new(Replica::new()))
        .await
        .unwrap();
    let reqs = h.requests_to(INBOX.path).await;
    assert_eq!(reqs[0].headers.get("authorization").unwrap(), "Bearer new");
}

#[tokio::test]
async fn outlook_deletions_moves_and_edits_follow_the_delta() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let rec = recent();
    let settings = json!({"client_id": "cid", "folders": ["inbox", "sentitems"]});
    INBOX
        .mount(
            &h,
            At::Initial,
            INBOX.page(
                &h,
                vec![
                    ol_msg("a", "A", &rec),
                    ol_msg("b", "B", &rec),
                    ol_msg("c", "C", &rec),
                ],
                None,
                Some("d1"),
            ),
        )
        .await;
    SENT.mount(
        &h,
        At::Initial,
        SENT.page(&h, vec![ol_msg("s", "S", &rec)], None, Some("d1")),
    )
    .await;
    // Delta: `a` moved inbox → sentitems, `b` deleted, `c` flagged (same received time).
    let mut flagged = ol_msg("c", "C", &rec);
    flagged["isRead"] = true.into();
    INBOX
        .mount(
            &h,
            At::Delta("d1"),
            INBOX.page(
                &h,
                vec![ol_removed("a"), ol_removed("b"), flagged],
                None,
                Some("d2"),
            ),
        )
        .await;
    SENT.mount(
        &h,
        At::Delta("d1"),
        SENT.page(&h, vec![ol_msg("a", "A", &rec)], None, Some("d2")),
    )
    .await;
    mount_mime_any(&h).await;

    let r = Replica::new();
    let conn = build(&h, &ol_factory(&h, fast_retry(0)), &settings, r.clone());
    let cur = testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
    let sink = ReplicaSink::new(r.clone());
    testkit::run_pass(&*conn, Some(cur), &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["-inbox/a", "-inbox/b", "+sentitems/a"]);
    assert_eq!(r.keys(), vec!["inbox/c", "sentitems/a", "sentitems/s"]);
    // The flag change did not refetch the MIME.
    assert_eq!(h.requests_to(&mime_path("c")).await.len(), 1);
    h.assert_egress("sync:outlook").await;
}

#[tokio::test]
async fn outlook_resync_required_reconciles() {
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
    mount_mime_any(&h).await;
    let r = Replica::new();
    let conn = build(
        &h,
        &ol_factory(&h, fast_retry(0)),
        &ol_settings(),
        r.clone(),
    );
    let cur = testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();

    h.server.reset().await;
    INBOX
        .mount(
            &h,
            At::Delta("d1"),
            ResponseTemplate::new(400).set_body_json(json!({
                "error": {"code": "resyncRequired", "message": "Resync required. Replace any local items with the server's version."}
            })),
        )
        .await;
    INBOX
        .mount(
            &h,
            At::Initial,
            INBOX.page(
                &h,
                vec![ol_msg("a", "A", &rec), ol_msg("n", "N", &rec)],
                None,
                Some("d2"),
            ),
        )
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/mailFolders/inbox")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "INBOX-ID"})))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/messages/b")))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error": {"code": "ErrorItemNotFound"}})),
        )
        .mount(&h.server)
        .await;
    mount_mime_any(&h).await;
    let sink = ReplicaSink::new(r.clone());
    let cur = testkit::run_pass(&*conn, Some(cur), &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+inbox/n", "-inbox/b"]);
    assert_eq!(r.keys(), vec!["inbox/a", "inbox/n"]);
    assert!(cur.to_string().contains("deltatoken=d2"));
    // Unchanged `a` was not downloaded again.
    assert!(h.requests_to(&mime_path("a")).await.is_empty());
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
    let conn = build(
        &h,
        &ol_factory(&h, fast_retry(1)),
        &ol_settings(),
        r.clone(),
    );
    let cur = testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
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
async fn outlook_skips_malformed_and_oversized_messages() {
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
                    ol_msg("ok1", "OK", &rec),
                    json!({"id": 5, "subject": "number id"}),
                    json!("not an object"),
                    json!({"id": "nodate", "subject": "no received time"}),
                    json!({"id": "../x", "receivedDateTime": rec}),
                    ol_msg("huge", "Huge", &rec),
                    ol_msg("ok2", "OK", &rec),
                ],
                None,
                Some("d1"),
            ),
        )
        .await;
    // Bigger than the 35 MiB MIME cap: the download is cut off and the message skipped.
    Mock::given(method("GET"))
        .and(path(mime_path("huge")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 35 * 1024 * 1024 + 1]))
        .with_priority(1)
        .mount(&h.server)
        .await;
    mount_mime_any(&h).await;
    let r = Replica::new();
    let conn = build(
        &h,
        &ol_factory(&h, fast_retry(0)),
        &ol_settings(),
        r.clone(),
    );
    testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
    assert_eq!(r.keys(), vec!["inbox/ok1", "inbox/ok2"]);
    h.assert_egress("sync:outlook").await;
}

#[tokio::test]
async fn outlook_unicode_subjects_and_html_mime_pass_through() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let rec = recent();
    let items: Vec<Value> = UNICODE_SAMPLES
        .iter()
        .enumerate()
        .map(|(i, s)| ol_msg(&format!("u{i}"), s, &rec))
        .collect();
    INBOX
        .mount(&h, At::Initial, INBOX.page(&h, items, None, Some("d1")))
        .await;
    let html = html_mail(UNICODE_SAMPLES[1]);
    let mime = format!(
        "Subject: =?UTF-8?B?eA==?=\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{html}\r\n"
    );
    Mock::given(method("GET"))
        .and(path_regex(r"^/v1\.0/me/messages/u\d+/\$value$"))
        .respond_with(ResponseTemplate::new(200).set_body_string(mime.clone()))
        .mount(&h.server)
        .await;
    let r = Replica::new();
    let conn = build(
        &h,
        &ol_factory(&h, fast_retry(0)),
        &ol_settings(),
        r.clone(),
    );
    testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
    assert_eq!(r.len(), UNICODE_SAMPLES.len());
    for (i, s) in UNICODE_SAMPLES.iter().enumerate() {
        let d = r.get(&format!("inbox/u{i}")).unwrap();
        assert_eq!(d.title.as_deref(), Some(cleaned(s).as_str()), "sample {i}");
        assert_eq!(d.mime, "message/rfc822");
        // The MIME (HTML part included) reaches the extractor byte for byte; HTML is stripped
        // there, never half-parsed here.
        assert_eq!(d.content, mime);
    }
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
    let cur = testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();

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

fn ev_pages(prefix: &str, pages: usize, per: usize) -> Vec<Vec<Value>> {
    (0..pages)
        .map(|p| {
            (0..per)
                .map(|i| {
                    let id = format!("{prefix}{}", p * per + i);
                    ev(&id, &format!("Event {id}"))
                })
                .collect()
        })
        .collect()
}

fn when_line(r: &Replica, id: &str) -> String {
    let d = r.get(id).unwrap_or_else(|| panic!("{id} not indexed"));
    d.content
        .lines()
        .find(|l| l.starts_with("When: "))
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn calendar_pages_through_five_pages_without_drops_or_duplicates() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let mut pages = ev_pages("e", 5, 3);
    pages[3].push(ev("e4", "Event e4"));
    CAL.mount_chain(&h, At::Initial, pages, "s", "d1").await;
    let mut inc = ev_pages("n", 5, 1);
    inc[2].push(ev_removed("e0"));
    let mut edited = ev("e1", "Event e1 (moved to Friday)");
    edited["changeKey"] = "ck-e1-v2".into();
    inc[4].push(edited);
    CAL.mount_chain(&h, At::Delta("d1"), inc, "i", "d2").await;

    let r = Replica::new();
    let conn = build(
        &h,
        &cal_factory(&h, fast_retry(0)),
        &cal_settings(),
        r.clone(),
    );
    let sink = ReplicaSink::new(r.clone());
    let cur = testkit::run_pass(&*conn, None, &sink).await.unwrap();
    assert_no_duplicate_upserts(&sink);
    assert_eq!(r.len(), 15);
    assert!(
        cur["delta_link"]
            .as_str()
            .unwrap()
            .contains("deltatoken=d1")
    );

    let sink = ReplicaSink::new(r.clone());
    let cur = testkit::run_pass(&*conn, Some(cur), &sink).await.unwrap();
    assert_no_duplicate_upserts(&sink);
    assert_eq!(r.len(), 15 + 5 - 1);
    assert!(r.get("e0").is_none());
    assert_eq!(
        r.get("e1").unwrap().title.as_deref(),
        Some("Event e1 (moved to Friday)")
    );
    assert!(
        r.get("e1")
            .unwrap()
            .version
            .unwrap()
            .starts_with("ck-e1-v2#")
    );
    assert!(
        cur["delta_link"]
            .as_str()
            .unwrap()
            .contains("deltatoken=d2")
    );
    h.assert_egress("sync:outlook-calendar").await;
}

#[tokio::test]
async fn calendar_throttling_backoff_and_retry_budget() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    CAL.mock(At::Initial)
        .respond_with(Sequence::new(vec![
            too_many_requests(0),
            bandwidth_exceeded_http_date(),
            server_error(503, Some(0)),
            CAL.page(&h, vec![ev("e1", "One")], Some("s1"), None),
        ]))
        .mount(&h.server)
        .await;
    CAL.mount(&h, At::Skip("s1"), throttled_http_date()).await;
    let r = Replica::new();
    let conn = build(
        &h,
        &cal_factory(&h, fast_retry(3)),
        &cal_settings(),
        r.clone(),
    );
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
    assert_eq!(r.keys(), vec!["e1"]);
    // 4 for the first page; the second asks for 30 s, longer than a pass waits: no retry.
    assert_eq!(h.requests_to(CAL.path).await.len(), 5);
    let committed = sink.committed().unwrap();

    h.server.reset().await;
    CAL.mount(
        &h,
        At::Skip("s1"),
        CAL.page(&h, vec![ev("e2", "Two")], None, Some("d1")),
    )
    .await;
    let sink = ReplicaSink::new(r.clone());
    testkit::run_pass(&*conn, Some(committed), &sink)
        .await
        .unwrap();
    assert_eq!(sink.summary(), vec!["+e2"]);
    assert_eq!(r.keys(), vec!["e1", "e2"]);
}

#[tokio::test]
async fn calendar_token_expiry_mid_sync_and_refresh_failure() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    CAL.mount_chain(&h, At::Initial, ev_pages("e", 3, 2), "s", "d1")
        .await;
    mount_rejected_token(&h, &CAL, At::Skip("s1"), "at").await;
    Mock::given(method("POST"))
        .and(path(TOKEN_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "fresh", "expires_in": 3600})),
        )
        .up_to_n_times(1)
        .mount(&h.server)
        .await;
    let r = Replica::new();
    let conn = build(
        &h,
        &cal_factory(&h, fast_retry(0)),
        &cal_settings(),
        r.clone(),
    );
    testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
    assert_eq!(r.len(), 6);
    assert_eq!(token_requests(&h).await, 1);

    // Second source state: the fresh token is rejected too and the refresh token was revoked.
    mount_rejected_token(&h, &CAL, At::Skip("s1"), "fresh").await;
    mount_refresh_invalid_grant(&h).await;
    let r = Replica::new();
    let conn = build(
        &h,
        &cal_factory(&h, fast_retry(0)),
        &cal_settings(),
        r.clone(),
    );
    let sink = ReplicaSink::new(r.clone());
    let res = conn.sync(None, &sink, &CancellationToken::new()).await;
    assert!(is_unauthorized(&res), "{res:?}");
    assert_eq!(r.len(), 2, "page 1 kept, nothing after the failure");
    h.assert_egress("sync:outlook-calendar").await;
}

#[tokio::test]
async fn calendar_expired_delta_token_reconciles() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    CAL.mount(
        &h,
        At::Initial,
        CAL.page(&h, vec![ev("a", "A"), ev("b", "B")], None, Some("d1")),
    )
    .await;
    let r = Replica::new();
    let conn = build(
        &h,
        &cal_factory(&h, fast_retry(0)),
        &cal_settings(),
        r.clone(),
    );
    let cur = testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();

    h.server.reset().await;
    CAL.mount(
        &h,
        At::Delta("d1"),
        ResponseTemplate::new(410).set_body_json(json!({"error": {"code": "syncStateNotFound"}})),
    )
    .await;
    CAL.mount(
        &h,
        At::Initial,
        CAL.page(&h, vec![ev("a", "A"), ev("c", "C")], None, Some("d2")),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/events/b")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.server)
        .await;
    let sink = ReplicaSink::new(r.clone());
    let cur = testkit::run_pass(&*conn, Some(cur), &sink).await.unwrap();
    assert_eq!(r.keys(), vec!["a", "c"]);
    assert!(sink.summary().contains(&"-b".to_string()));
    assert!(
        cur["delta_link"]
            .as_str()
            .unwrap()
            .contains("deltatoken=d2")
    );
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
    let conn = build(
        &h,
        &cal_factory(&h, fast_retry(0)),
        &cal_settings(),
        r.clone(),
    );
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
    let conn = build(
        &h,
        &cal_factory(&h, fast_retry(0)),
        &cal_settings(),
        r.clone(),
    );
    let cur = testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
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

    testkit::run_pass(&*conn, Some(cur), &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
    assert_eq!(r.keys(), vec!["M-0713", "M-1207"]);
}

#[tokio::test]
async fn calendar_time_zones_all_day_and_dst() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let tz = |id: &str, start: &str, end: &str, zone: &str| {
        let mut e = ev_at(id, id, start, end);
        e["originalStartTimeZone"] = zone.into();
        e
    };
    let mut all_day = ev_at(
        "allday",
        "Holiday",
        "2026-11-03T00:00:00",
        "2026-11-04T00:00:00",
    );
    all_day["isAllDay"] = true.into();
    let mut multi_day = ev_at("trip", "Trip", "2026-11-03T00:00:00", "2026-11-06T00:00:00");
    multi_day["isAllDay"] = true.into();
    let items = vec![
        all_day,
        multi_day,
        // Crosses midnight into the next day in Tokyo.
        tz(
            "tokyo",
            "2026-11-03T15:30:00",
            "2026-11-03T16:00:00",
            "Tokyo Standard Time",
        ),
        // US DST ends 2026-11-01 02:00 local: 01:30 happens twice.
        tz(
            "edt",
            "2026-11-01T05:30:00",
            "2026-11-01T05:45:00",
            "Eastern Standard Time",
        ),
        tz(
            "est",
            "2026-11-01T06:30:00",
            "2026-11-01T06:45:00",
            "Eastern Standard Time",
        ),
        tz(
            "iana",
            "2026-07-01T22:00:00",
            "2026-07-01T23:00:00",
            "Europe/Paris",
        ),
        tz(
            "india",
            "2026-03-10T04:00:00",
            "2026-03-10T05:00:00",
            "India Standard Time",
        ),
        tz(
            "custom",
            "2026-03-10T04:00:00",
            "2026-03-10T05:00:00",
            "tzone://Microsoft/Custom",
        ),
    ];
    CAL.mount(&h, At::Initial, CAL.page(&h, items, None, Some("d1")))
        .await;
    let r = Replica::new();
    let conn = build(
        &h,
        &cal_factory(&h, fast_retry(0)),
        &cal_settings(),
        r.clone(),
    );
    testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
    let want = [
        ("allday", "When: 2026-11-03 (all day)"),
        ("trip", "When: 2026-11-03 – 2026-11-05 (all day)"),
        (
            "tokyo",
            "When: 2026-11-04 00:30 – 01:00 (Tokyo Standard Time, UTC+09:00)",
        ),
        (
            "edt",
            "When: 2026-11-01 01:30 – 01:45 (Eastern Standard Time, UTC-04:00)",
        ),
        (
            "est",
            "When: 2026-11-01 01:30 – 01:45 (Eastern Standard Time, UTC-05:00)",
        ),
        (
            "iana",
            "When: 2026-07-02 00:00 – 01:00 (Europe/Paris, UTC+02:00)",
        ),
        (
            "india",
            "When: 2026-03-10 09:30 – 10:30 (India Standard Time, UTC+05:30)",
        ),
        ("custom", "When: 2026-03-10 04:00 – 05:00 (UTC+00:00)"),
    ];
    for (id, line) in want {
        assert_eq!(when_line(&r, id), line, "{id}");
    }
}

#[tokio::test]
async fn calendar_unicode_html_and_huge_bodies() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let mut items: Vec<Value> = UNICODE_SAMPLES
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut e = ev(&format!("u{i}"), s);
            e["body"] = json!({"contentType": "html", "content": html_mail(s)});
            e
        })
        .collect();
    let mut huge = ev("huge", "Huge");
    huge["body"] = json!({"contentType": "text", "content": big_text(3 * 1024 * 1024)});
    items.push(huge);
    items.push(json!({"id": "bad", "start": "not an object"}));
    items.push(json!(["garbage"]));
    CAL.mount(&h, At::Initial, CAL.page(&h, items, None, Some("d1")))
        .await;
    let r = Replica::new();
    let conn = build(
        &h,
        &cal_factory(&h, fast_retry(0)),
        &cal_settings(),
        r.clone(),
    );
    let sink = ReplicaSink::new(r.clone());
    testkit::run_pass(&*conn, None, &sink).await.unwrap();
    assert_eq!(r.len(), UNICODE_SAMPLES.len() + 1, "{:?}", r.keys());
    for (i, s) in UNICODE_SAMPLES.iter().enumerate() {
        let d = r.get(&format!("u{i}")).unwrap();
        assert_eq!(d.title.as_deref(), Some(cleaned(s).as_str()), "sample {i}");
        for leak in [
            "alert(",
            "color:red",
            "hidden comment",
            "<p>",
            "&amp;",
            "t.example.com",
        ] {
            assert!(
                !d.content.contains(leak),
                "{leak} leaked in {:?}",
                d.content
            );
        }
        assert!(d.content.contains("& more <ok>"), "{:?}", d.content);
    }
    let huge = sink
        .upserts()
        .into_iter()
        .find(|d| d.key.as_str() == "huge")
        .unwrap();
    assert!(doc_text(&huge).chars().count() < 25_000);
}

#[tokio::test]
async fn calendar_crash_resumable_backfill_and_delta() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let factory: Arc<dyn ConnectorFactory> = Arc::new(cal_factory(&h, fast_retry(0)));
    CAL.mount_chain(&h, At::Initial, ev_pages("e", 5, 2), "s", "d1")
        .await;
    CAL.mount(&h, At::Delta("d1"), CAL.page(&h, vec![], None, Some("d1")))
        .await;
    let report = crash_matrix(&h, factory.clone(), cal_settings(), None, BTreeMap::new()).await;
    assert!(report.sends >= 10 && report.checkpoints >= 5, "{report:?}");
    let r = Replica::new();
    let conn = build(&h, &*factory, &cal_settings(), r.clone());
    let cur = testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();

    reset(&h).await;
    let mut inc = ev_pages("n", 5, 1);
    inc[1].push(ev_removed("e0"));
    let mut cancelled = ev("e3", "Cancelled");
    cancelled["isCancelled"] = true.into();
    inc[3].push(cancelled);
    inc[4].push(ev_removed("n0"));
    CAL.mount_chain(&h, At::Delta("d1"), inc, "i", "d2").await;
    CAL.mount(&h, At::Delta("d2"), CAL.page(&h, vec![], None, Some("d2")))
        .await;
    let report = crash_matrix(&h, factory, cal_settings(), Some(cur), r.snapshot()).await;
    assert!(report.sends >= 5 && report.checkpoints >= 5, "{report:?}");
    h.assert_egress("sync:outlook-calendar").await;
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

#[tokio::test]
async fn onedrive_pages_deletes_moves_and_renames() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let mut pages = od_pages(&h, "f", 5, 3);
    pages[0].insert(0, od_root());
    pages[0].insert(1, od_folder("DOCS", "ROOT", "Docs"));
    pages[0].insert(2, od_folder("ARCH", "ROOT", "Archive"));
    pages[3].push(od_file(&h, "f2", "DOCS", "f2.txt", 1));
    DRIVE.mount_chain(&h, At::Initial, pages, "p", "d1").await;
    // Incremental, five pages: a delete, a rename, a move, new files.
    let mut inc = od_pages(&h, "n", 5, 1);
    inc[0].push(od_deleted("f0"));
    inc[1].push(od_file(&h, "f1", "DOCS", "f1 renamed.txt", 2));
    inc[2].push(od_file(&h, "f2", "ARCH", "f2.txt", 1));
    DRIVE.mount_chain(&h, At::Delta("d1"), inc, "i", "d2").await;
    mount_downloads(&h).await;

    let r = Replica::new();
    let conn = build(
        &h,
        &od_factory(&h, fast_retry(0)),
        &od_settings(),
        r.clone(),
    );
    let sink = ReplicaSink::new(r.clone());
    let cur = testkit::run_pass(&*conn, None, &sink).await.unwrap();
    assert_no_duplicate_upserts(&sink);
    assert_eq!(r.len(), 15);
    assert_eq!(h.requests_to(&dl_path("f2")).await.len(), 1);

    let sink = ReplicaSink::new(r.clone());
    let cur = testkit::run_pass(&*conn, Some(cur), &sink).await.unwrap();
    assert_no_duplicate_upserts(&sink);
    assert_eq!(r.len(), 15 - 1 + 5);
    assert!(r.get("f0").is_none());
    assert_eq!(
        r.get("f1").unwrap().title.as_deref(),
        Some("f1 renamed.txt")
    );
    assert!(r.get("f2").unwrap().version.unwrap().ends_with("|ARCH"));
    let moved = sink
        .upserts()
        .into_iter()
        .find(|d| d.key.as_str() == "f2")
        .unwrap();
    assert_eq!(moved.metadata["path"], "/Archive/f2.txt");
    assert!(cur["delta_link"].as_str().unwrap().contains("token=d2"));
    h.assert_egress("sync:onedrive").await;
}

#[tokio::test]
async fn onedrive_expired_delta_token_reconciles_the_drive() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let p = vec![
        od_root(),
        od_file(&h, "a", "ROOT", "a.txt", 1),
        od_file(&h, "b", "ROOT", "b.txt", 1),
    ];
    DRIVE
        .mount(&h, At::Initial, DRIVE.page(&h, p, None, Some("d1")))
        .await;
    mount_downloads(&h).await;
    let r = Replica::new();
    let conn = build(
        &h,
        &od_factory(&h, fast_retry(0)),
        &od_settings(),
        r.clone(),
    );
    let cur = testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();

    h.server.reset().await;
    DRIVE
        .mount(
            &h,
            At::Delta("d1"),
            ResponseTemplate::new(410).set_body_json(json!({"error": {"code": "resyncRequired"}})),
        )
        .await;
    let p = vec![
        od_root(),
        od_file(&h, "a", "ROOT", "a.txt", 1),
        od_file(&h, "c", "ROOT", "c.txt", 1),
    ];
    DRIVE
        .mount(&h, At::Initial, DRIVE.page(&h, p, None, Some("d2")))
        .await;
    mount_downloads(&h).await;
    let sink = ReplicaSink::new(r.clone());
    testkit::run_pass(&*conn, Some(cur), &sink).await.unwrap();
    assert_eq!(r.keys(), vec!["a", "c"]);
    assert_eq!(sink.summary(), vec!["+c", "-b"]);
    assert!(
        h.requests_to(&dl_path("a")).await.is_empty(),
        "unchanged a not refetched"
    );
}

#[tokio::test]
async fn onedrive_throttled_downloads_and_retry_budget() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let p = vec![od_root(), od_file(&h, "t", "ROOT", "t.txt", 1)];
    DRIVE
        .mock(At::Initial)
        .respond_with(Sequence::new(vec![
            server_error(503, Some(0)),
            DRIVE.page(&h, p, None, Some("d1")),
        ]))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(dl_path("t")))
        .respond_with(Sequence::new(vec![
            too_many_requests(0),
            server_error(503, Some(0)),
            bandwidth_exceeded_http_date(),
            ResponseTemplate::new(200).set_body_string("finally"),
        ]))
        .mount(&h.server)
        .await;
    let r = Replica::new();
    let conn = build(
        &h,
        &od_factory(&h, fast_retry(3)),
        &od_settings(),
        r.clone(),
    );
    testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
    assert_eq!(r.get("t").unwrap().content, "finally");
    assert_eq!(h.requests_to(&dl_path("t")).await.len(), 4);
    h.assert_egress("sync:onedrive").await;
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
    let conn = build(
        &h,
        &od_factory(&h, fast_retry(1)),
        &od_settings(),
        r.clone(),
    );
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
async fn onedrive_token_expiry_mid_sync_and_refresh_failure() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let mut pages = od_pages(&h, "f", 3, 2);
    pages[0].insert(0, od_root());
    pages[0].insert(1, od_folder("DOCS", "ROOT", "Docs"));
    DRIVE.mount_chain(&h, At::Initial, pages, "p", "d1").await;
    mount_rejected_token(&h, &DRIVE, At::Skip("p2"), "at").await;
    mount_refresh_ok(&h, "fresh").await;
    mount_downloads(&h).await;
    let r = Replica::new();
    let conn = build(
        &h,
        &od_factory(&h, fast_retry(0)),
        &od_settings(),
        r.clone(),
    );
    testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
    assert_eq!(r.len(), 6);
    assert_eq!(token_requests(&h).await, 1);
    // Downloads never carry the bearer token, refreshed or not.
    for req in h.server.received_requests().await.unwrap() {
        if req.url.path().starts_with("/dl/") {
            assert!(req.headers.get("authorization").is_none());
        }
    }
    h.assert_egress("sync:onedrive").await;

    h.server.reset().await;
    h.seed_tokens("at", false);
    let mut pages = od_pages(&h, "f", 3, 2);
    pages[0].insert(0, od_root());
    pages[0].insert(1, od_folder("DOCS", "ROOT", "Docs"));
    DRIVE.mount_chain(&h, At::Initial, pages, "p", "d1").await;
    mount_rejected_token(&h, &DRIVE, At::Skip("p2"), "at").await;
    mount_refresh_invalid_grant(&h).await;
    mount_downloads(&h).await;
    let r = Replica::new();
    let conn = build(
        &h,
        &od_factory(&h, fast_retry(0)),
        &od_settings(),
        r.clone(),
    );
    let sink = ReplicaSink::new(r.clone());
    let res = conn.sync(None, &sink, &CancellationToken::new()).await;
    assert!(is_unauthorized(&res), "{res:?}");
    assert_eq!(r.len(), 4);
    assert!(sink.committed().unwrap().to_string().contains("token=p2"));
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
    testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
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
    let conn = build(
        &h,
        &od_factory(&h, fast_retry(1)),
        &od_settings(),
        r.clone(),
    );
    let cur = testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
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
async fn onedrive_unicode_file_and_folder_names() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let folder_name = UNICODE_SAMPLES[4];
    let mut items = vec![od_root(), od_folder("UNI", "ROOT", folder_name)];
    for (i, s) in UNICODE_SAMPLES.iter().enumerate() {
        items.push(od_file(&h, &format!("u{i}"), "UNI", &format!("{s}.txt"), 1));
    }
    DRIVE
        .mount(&h, At::Initial, DRIVE.page(&h, items, None, Some("d1")))
        .await;
    mount_downloads(&h).await;
    let r = Replica::new();
    let conn = build(
        &h,
        &od_factory(&h, fast_retry(0)),
        &od_settings(),
        r.clone(),
    );
    let sink = ReplicaSink::new(r.clone());
    testkit::run_pass(&*conn, None, &sink).await.unwrap();
    assert_eq!(r.len(), UNICODE_SAMPLES.len());
    for (i, s) in UNICODE_SAMPLES.iter().enumerate() {
        let name = cleaned(&format!("{s}.txt"));
        let d = sink
            .upserts()
            .into_iter()
            .find(|d| d.key.to_string() == format!("u{i}"))
            .unwrap();
        assert_eq!(d.title.as_deref(), Some(name.as_str()), "sample {i}");
        assert_eq!(d.metadata["path"], format!("/{folder_name}/{name}"));
    }
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
    let cur = testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
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
    testkit::run_pass(&*conn, Some(cur), &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
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
    let conn = build(
        &h,
        &od_factory(&h, fast_retry(0)),
        &od_settings(),
        r.clone(),
    );
    let cur = testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
    let sink = ReplicaSink::new(r.clone());
    testkit::run_pass(&*conn, Some(cur), &sink).await.unwrap();
    assert!(sink.summary().is_empty(), "{:?}", sink.summary());
    assert_eq!(r.keys(), vec!["a"]);

    let r = Replica::new();
    let conn = build(
        &h,
        &cal_factory(&h, fast_retry(0)),
        &cal_settings(),
        r.clone(),
    );
    testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
    assert_eq!(r.keys(), vec!["e1"]);

    let r = Replica::new();
    let conn = build(
        &h,
        &ol_factory(&h, fast_retry(0)),
        &ol_settings(),
        r.clone(),
    );
    testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
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
    let conn = build(
        &h,
        &ol_factory(&h, fast_retry(0)),
        &ol_settings(),
        r.clone(),
    );
    let mut cur = testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
    assert_eq!(
        cur["retry"],
        json!([{"key": "inbox/gone", "attempts": 1}, {"key": "inbox/stuck", "attempts": 1}])
    );
    let sink = ReplicaSink::new(r.clone());
    cur = testkit::run_pass(&*conn, Some(cur), &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["-inbox/gone"]);
    assert_eq!(cur["retry"], json!([{"key": "inbox/stuck", "attempts": 2}]));
    for attempts in 3..crate::graph::MAX_ITEM_ATTEMPTS {
        cur = testkit::run_pass(&*conn, Some(cur), &ReplicaSink::new(r.clone()))
            .await
            .unwrap();
        assert_eq!(
            cur["retry"],
            json!([{"key": "inbox/stuck", "attempts": attempts}])
        );
    }
    cur = testkit::run_pass(&*conn, Some(cur), &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
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
    let conn = build(
        &h,
        &ol_factory(&h, fast_retry(0)),
        &ol_settings(),
        r.clone(),
    );
    let sink = ReplicaSink::new(r.clone());
    let res = conn.sync(None, &sink, &CancellationToken::new()).await;
    assert!(matches!(res, Err(Error::Network(_))), "{res:?}");
    assert!(sink.committed().is_none() && r.is_empty());

    let conn = build(
        &h,
        &od_factory(&h, fast_retry(0)),
        &od_settings(),
        r.clone(),
    );
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
    let conn = build(
        &h,
        &ol_factory(&h, fast_retry(2)),
        &ol_settings(),
        r.clone(),
    );
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
    let conn = build(
        &h,
        &od_factory(&h, fast_retry(1)),
        &od_settings(),
        r.clone(),
    );
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
    let conn = build(
        &h,
        &ol_factory(&h, fast_retry(0)),
        &ol_settings(),
        r.clone(),
    );
    let cur = testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
    assert_eq!(r.keys(), vec!["inbox/ok"]);
    assert_eq!(cur["retry"], json!([{"key": "inbox/irm", "attempts": 1}]));

    let r = Replica::new();
    let conn = build(
        &h,
        &od_factory(&h, fast_retry(0)),
        &od_settings(),
        r.clone(),
    );
    let cur = testkit::run_pass(&*conn, None, &ReplicaSink::new(r.clone()))
        .await
        .unwrap();
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
    let conn = build(
        &h,
        &ol_factory(&h, fast_retry(0)),
        &ol_settings(),
        Replica::new(),
    );
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
    let conn = build(
        &h,
        &cal_factory(&h, fast_retry(0)),
        &cal_settings(),
        seed.clone(),
    );
    let sink = ReplicaSink::new(seed.clone());
    testkit::run_pass(&*conn, None, &sink).await.unwrap();
    assert_eq!(seed.keys(), vec!["e1"]);
    assert_eq!(sink.summary(), vec!["-M1", "+e1", "-M2"]);
}
