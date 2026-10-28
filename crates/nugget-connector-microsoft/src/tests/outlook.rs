use nugget_connector::Content;
use serde_json::json;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::OutlookFactory;

fn settings() -> Value {
    json!({"client_id": "cid"})
}

fn outlook(h: &Harness, retry: RetryPolicy) -> Arc<dyn Connector> {
    outlook_with(h, retry, settings())
}

fn outlook_with(h: &Harness, retry: RetryPolicy, settings: Value) -> Arc<dyn Connector> {
    h.build(
        &OutlookFactory::new()
            .with_endpoints(h.endpoints())
            .with_retry(retry),
        settings,
    )
}

const RECEIVED: &str = "2026-10-01T09:30:00Z";
const RECEIVED_SECS: i64 = 1_790_847_000;

fn msg(id: &str, subject: &str) -> Value {
    json!({
        "@odata.etag": "W/\"x\"",
        "id": id, "subject": subject, "receivedDateTime": RECEIVED,
        "lastModifiedDateTime": "2026-10-01T09:31:00Z",
        "from": {"emailAddress": {"name": "Alice", "address": "alice@example.com"}},
        "conversationId": format!("conv-{id}"), "internetMessageId": format!("<{id}@example.com>"),
        "isDraft": false, "webLink": format!("https://outlook.office365.com/owa/?ItemID={id}"),
    })
}

fn removed(id: &str) -> Value {
    json!({"id": id, "@removed": {"reason": "deleted"}})
}

fn delta_path(folder: &str) -> String {
    format!("{API}/me/mailFolders/{folder}/messages/delta")
}

fn link(h: &Harness, folder: &str, q: &str) -> String {
    format!("{}{}?{q}", h.uri(), delta_path(folder))
}

/// A delta page; `next` / `delta` are query strings for the links.
fn page(
    h: &Harness,
    folder: &str,
    items: Vec<Value>,
    next: Option<&str>,
    delta: Option<&str>,
) -> ResponseTemplate {
    let mut body = json!({"value": items});
    if let Some(n) = next {
        body["@odata.nextLink"] = link(h, folder, n).into();
    }
    if let Some(d) = delta {
        body["@odata.deltaLink"] = link(h, folder, d).into();
    }
    ResponseTemplate::new(200).set_body_json(body)
}

/// The first page of a full listing (has `$filter`, no tokens).
async fn mount_initial(h: &Harness, folder: &str, resp: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(delta_path(folder)))
        .and(query_param_is_missing("$skiptoken"))
        .and(query_param_is_missing("$deltatoken"))
        .respond_with(resp)
        .mount(&h.server)
        .await;
}

async fn mount_token(h: &Harness, folder: &str, name: &str, value: &str, resp: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(delta_path(folder)))
        .and(query_param(name, value))
        .respond_with(resp)
        .mount(&h.server)
        .await;
}

async fn mount_mime(server: &MockServer, id: &str) {
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/messages/{id}/$value")))
        .respond_with(
            ResponseTemplate::new(200).set_body_bytes(
                format!(
                    "From: Alice <alice@example.com>\r\nSubject: S {id}\r\n\r\nBody of {id}\r\n"
                )
                .into_bytes(),
            ),
        )
        .mount(server)
        .await;
}

async fn mount_no_mime(server: &MockServer) {
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(r"/\$value$"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(server)
        .await;
}

#[tokio::test]
async fn sync_without_tokens_is_unauthorized_and_sends_nothing() {
    let h = Harness::new().await;
    let conn = outlook(&h, fast_retry(0));
    let sink = RecSink::default();
    assert!(is_unauthorized(&run(&conn, None, &sink).await));
    assert!(h.server.received_requests().await.unwrap().is_empty());
    assert!(sink.summary().is_empty());
}

#[tokio::test]
async fn backfill_pages_each_folder_and_fetches_mime() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_initial(
        &h,
        "inbox",
        page(
            &h,
            "inbox",
            vec![msg("m1", "Hello"), msg("m2", "Second")],
            Some("$skiptoken=p2"),
            None,
        ),
    )
    .await;
    mount_token(
        &h,
        "inbox",
        "$skiptoken",
        "p2",
        page(
            &h,
            "inbox",
            vec![msg("m3", "Third")],
            None,
            Some("$deltatoken=d-in"),
        ),
    )
    .await;
    mount_initial(
        &h,
        "sentitems",
        page(
            &h,
            "sentitems",
            vec![msg("s1", "Sent")],
            None,
            Some("$deltatoken=d-sent"),
        ),
    )
    .await;
    for id in ["m1", "m2", "m3", "s1"] {
        mount_mime(&h.server, id).await;
    }

    let conn = outlook(&h, fast_retry(0));
    let sink = RecSink::default();
    let cursor = run(&conn, None, &sink).await.unwrap();
    assert_eq!(
        sink.summary(),
        vec!["+inbox/m1", "+inbox/m2", "+inbox/m3", "+sentitems/s1"]
    );
    assert_eq!(
        cursor,
        json!({"v": 1, "folders": {
            "inbox": {"delta_link": link(&h, "inbox", "$deltatoken=d-in")},
            "sentitems": {"delta_link": link(&h, "sentitems", "$deltatoken=d-sent")},
        }})
    );
    // A checkpoint after the first page holds the next link and the window.
    let cps = sink.checkpoints.lock().unwrap().clone();
    let first = &cps[0]["folders"]["inbox"];
    assert_eq!(first["next_link"], link(&h, "inbox", "$skiptoken=p2"));
    let after = first["after"].as_i64().unwrap();
    assert!((now() - 365 * 86_400 - after).abs() < 60);

    // The window filter, the select list and the Prefer headers.
    let initial = h
        .requests_to(&delta_path("inbox"))
        .await
        .into_iter()
        .find(|r| r.url.query().is_some_and(|q| q.contains("filter")))
        .unwrap();
    let q: HashMap<String, String> = initial.url.query_pairs().into_owned().collect();
    assert_eq!(
        q["$filter"],
        format!("receivedDateTime ge {}", rfc3339(after))
    );
    assert!(q["$select"].contains("receivedDateTime"));
    let p = prefer(&initial);
    assert!(p.contains(&"IdType=\"ImmutableId\"".to_string()), "{p:?}");
    assert!(p.iter().any(|v| v.starts_with("odata.maxpagesize=")));
    assert_eq!(initial.headers.get("authorization").unwrap(), "Bearer at");

    let docs = sink.upserts();
    let d = &docs[0];
    assert_eq!(d.mime, "message/rfc822");
    assert!(matches!(&d.content, Content::Bytes(b) if b.starts_with(b"From: Alice")));
    assert_eq!(
        d.version.as_deref(),
        Some(format!("r{RECEIVED_SECS}").as_str())
    );
    assert_eq!(d.title.as_deref(), Some("Hello"));
    assert_eq!(d.modified_at.unwrap().unix_timestamp(), RECEIVED_SECS);
    assert_eq!(
        d.url.as_deref(),
        Some("https://outlook.office365.com/owa/?ItemID=m1")
    );
    assert_eq!(d.metadata["folder"], "inbox");
    assert_eq!(d.metadata["conversation_id"], "conv-m1");
    assert_eq!(d.metadata["from"]["address"], "alice@example.com");
    h.assert_egress("sync:outlook").await;
}

#[tokio::test]
async fn delta_round_applies_removals_and_skips_unchanged() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let v = format!("r{RECEIVED_SECS}");
    h.know("inbox/m1", &v);
    h.know("inbox/gone", &v);
    h.know("inbox/draft", &v);
    mount_token(
        &h,
        "inbox",
        "$deltatoken",
        "d1",
        page(
            &h,
            "inbox",
            vec![
                // A read-flag change: same received time, so no new download.
                msg("m1", "Hello"),
                removed("gone"),
                json!({"id": "draft", "isDraft": true, "receivedDateTime": RECEIVED}),
                msg("new", "New mail"),
                removed("never-indexed"),
            ],
            None,
            Some("$deltatoken=d2"),
        ),
    )
    .await;
    mount_mime(&h.server, "new").await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/messages/m1/$value")))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&h.server)
        .await;

    let conn = outlook_with(
        &h,
        fast_retry(0),
        json!({"client_id": "cid", "folders": ["inbox"]}),
    );
    let sink = RecSink::default();
    let prior =
        json!({"v": 1, "folders": {"inbox": {"delta_link": link(&h, "inbox", "$deltatoken=d1")}}});
    let out = run(&conn, Some(prior), &sink).await.unwrap();
    assert_eq!(
        sink.summary(),
        vec!["-inbox/gone", "-inbox/draft", "+inbox/new"]
    );
    assert_eq!(
        out["folders"]["inbox"],
        json!({"delta_link": link(&h, "inbox", "$deltatoken=d2")})
    );
    h.assert_egress("sync:outlook").await;
}

#[tokio::test]
async fn expired_delta_token_resyncs_and_verifies_missing() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let inside = format!("r{}", now() - 10 * 86_400);
    let outside = format!("r{}", now() - 400 * 86_400);
    h.know("inbox/kept", &inside);
    h.know("inbox/deleted", &inside);
    h.know("inbox/moved", &inside);
    h.know("inbox/old", &outside); // older than the window: expected to be unlisted, stays
    mount_token(&h, "inbox", "$deltatoken", "stale", ResponseTemplate::new(410).set_body_json(json!({
        "error": {"code": "SyncStateNotFound", "message": "The sync state generation is not found."}
    }))).await;
    mount_initial(
        &h,
        "inbox",
        page(
            &h,
            "inbox",
            vec![msg("new", "New")],
            None,
            Some("$deltatoken=fresh"),
        ),
    )
    .await;
    mount_mime(&h.server, "new").await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/mailFolders/inbox")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "INBOX-ID"})))
        .mount(&h.server)
        .await;
    for (id, resp) in [
        (
            "kept",
            ResponseTemplate::new(200).set_body_json(
                json!({"id": "kept", "parentFolderId": "INBOX-ID", "isDraft": false}),
            ),
        ),
        (
            "deleted",
            ResponseTemplate::new(404)
                .set_body_json(json!({"error": {"code": "ErrorItemNotFound"}})),
        ),
        (
            "moved",
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "moved", "parentFolderId": "ARCHIVE-ID"})),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("{API}/me/messages/{id}")))
            .respond_with(resp)
            .expect(1)
            .mount(&h.server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/messages/old")))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&h.server)
        .await;

    let conn = outlook_with(
        &h,
        fast_retry(0),
        json!({"client_id": "cid", "folders": ["inbox"]}),
    );
    let sink = RecSink::default();
    let prior = json!({"v": 1, "folders": {"inbox": {"delta_link": link(&h, "inbox", "$deltatoken=stale")}}});
    let out = run(&conn, Some(prior), &sink).await.unwrap();
    let mut s = sink.summary();
    assert_eq!(s.remove(0), "+inbox/new");
    s.sort();
    assert_eq!(s, vec!["-inbox/deleted", "-inbox/moved"]);
    assert_eq!(
        out["folders"]["inbox"],
        json!({"delta_link": link(&h, "inbox", "$deltatoken=fresh")})
    );
    h.assert_egress("sync:outlook").await;
}

#[tokio::test]
async fn reset_cursor_reconciles_folders_and_window() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let inside = format!("r{}", now() - 5 * 86_400);
    let outside = format!("r{}", now() - 60 * 86_400);
    h.know("inbox/listed", &inside);
    h.know("inbox/old", &outside); // outside the new 30-day window: deleted without a request
    h.know("inbox/gone", &inside); // inside: checked, and gone
    h.know("archive/a1", &inside); // folder no longer synced
    mount_initial(&h, "inbox", page(&h, "inbox", vec![
        json!({"id": "listed", "subject": "Listed", "receivedDateTime": rfc3339(now() - 5 * 86_400)}),
    ], None, Some("$deltatoken=d"))).await;
    mount_initial(
        &h,
        "sentitems",
        page(&h, "sentitems", vec![], None, Some("$deltatoken=s")),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/mailFolders/inbox")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "INBOX-ID"})))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/messages/gone")))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&h.server)
        .await;
    mount_no_mime(&h.server).await;

    let conn = outlook_with(
        &h,
        fast_retry(0),
        json!({"client_id": "cid", "window_days": 30}),
    );
    let sink = RecSink::default();
    let out = run(&conn, None, &sink).await.unwrap();
    // The listed message is unchanged: no download, no upsert.
    assert_eq!(
        sink.summary(),
        vec!["-archive/a1", "-inbox/old", "-inbox/gone"]
    );
    assert_eq!(out["folders"].as_object().unwrap().len(), 2);
    let initial = h.requests_to(&delta_path("inbox")).await.remove(0);
    let q: HashMap<String, String> = initial.url.query_pairs().into_owned().collect();
    // The new, shorter window is listed.
    let since = q["$filter"].strip_prefix("receivedDateTime ge ").unwrap();
    let since = time::OffsetDateTime::parse(since, &time::format_description::well_known::Rfc3339)
        .unwrap()
        .unix_timestamp();
    assert!((now() - 30 * 86_400 - since).abs() < 60);
    h.assert_egress("sync:outlook").await;
}

#[tokio::test]
async fn interrupted_resync_restarts_its_listing() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    h.know("inbox/a", &format!("r{}", now()));
    mount_initial(
        &h,
        "inbox",
        page(
            &h,
            "inbox",
            vec![json!({"id": "a", "receivedDateTime": rfc3339(now())})],
            None,
            Some("$deltatoken=d"),
        ),
    )
    .await;
    Mock::given(method("GET"))
        .and(path(delta_path("inbox")))
        .and(query_param("$skiptoken", "half"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&h.server)
        .await;
    let conn = outlook_with(
        &h,
        fast_retry(0),
        json!({"client_id": "cid", "folders": ["inbox"]}),
    );
    let sink = RecSink::default();
    let prior = json!({"v": 1, "folders": {"inbox": {
        "after": now() - 86_400, "next_link": link(&h, "inbox", "$skiptoken=half"), "resync": true
    }}});
    run(&conn, Some(prior), &sink).await.unwrap();
    // `a` was listed again, so it is not taken for missing.
    assert!(sink.summary().is_empty());
}

#[tokio::test]
async fn throttling_is_retried_with_retry_after() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .and(path(delta_path("inbox")))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0").set_body_json(json!({
            "error": {"code": "TooManyRequests", "message": "Application is over its MailboxConcurrency limit."}
        })))
        .up_to_n_times(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(delta_path("inbox")))
        .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .mount(&h.server)
        .await;
    mount_initial(
        &h,
        "inbox",
        page(&h, "inbox", vec![], None, Some("$deltatoken=d")),
    )
    .await;
    let conn = outlook_with(
        &h,
        fast_retry(3),
        json!({"client_id": "cid", "folders": ["inbox"]}),
    );
    let out = run(&conn, None, &RecSink::default()).await.unwrap();
    assert!(out["folders"]["inbox"]["delta_link"].is_string());
    assert_eq!(h.requests_to(&delta_path("inbox")).await.len(), 3);
    h.assert_egress("sync:outlook").await;
}

#[tokio::test]
async fn persistent_throttling_surfaces_rate_limited() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .and(path(delta_path("inbox")))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "7"))
        .mount(&h.server)
        .await;
    let conn = outlook_with(
        &h,
        fast_retry(1),
        json!({"client_id": "cid", "folders": ["inbox"]}),
    );
    let err = run(&conn, None, &RecSink::default()).await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: Some(7000)
            }
        ),
        "{err:?}"
    );
    assert_eq!(h.requests_to(&delta_path("inbox")).await.len(), 2);
}

#[tokio::test]
async fn rejected_token_is_refreshed_once() {
    let h = Harness::new().await;
    h.seed_tokens("stale", false);
    Mock::given(method("POST"))
        .and(path("/common/oauth2/v2.0/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "fresh", "expires_in": 3600
        })))
        .expect(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(delta_path("inbox")))
        .and(wiremock::matchers::header("authorization", "Bearer stale"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error": {"code": "InvalidAuthenticationToken"}})),
        )
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(delta_path("inbox")))
        .and(wiremock::matchers::header("authorization", "Bearer fresh"))
        .respond_with(page(&h, "inbox", vec![], None, Some("$deltatoken=d")))
        .mount(&h.server)
        .await;
    let conn = outlook_with(
        &h,
        fast_retry(0),
        json!({"client_id": "cid", "folders": ["inbox"]}),
    );
    run(&conn, None, &RecSink::default()).await.unwrap();
    h.assert_egress("sync:outlook").await;
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let other = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .expect(0)
        .mount(&other)
        .await;
    Mock::given(method("GET"))
        .and(path(delta_path("inbox")))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", format!("{}/steal", other.uri())),
        )
        .mount(&h.server)
        .await;
    let conn = outlook_with(
        &h,
        fast_retry(2),
        json!({"client_id": "cid", "folders": ["inbox"]}),
    );
    let err = run(&conn, None, &RecSink::default()).await.unwrap_err();
    assert!(
        matches!(err, Error::Network(ref m) if m.contains("redirect")),
        "{err:?}"
    );
    assert_eq!(
        h.requests_to(&delta_path("inbox")).await.len(),
        1,
        "not retried"
    );
    h.assert_egress("sync:outlook").await;
}

#[tokio::test]
async fn foreign_next_link_is_refused_before_anything_is_applied() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let evil = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&evil)
        .await;
    let body = json!({
        "value": [msg("m1", "Hi")],
        "@odata.nextLink": format!("{}{}?$skiptoken=x", evil.uri(), delta_path("inbox")),
    });
    mount_initial(&h, "inbox", ResponseTemplate::new(200).set_body_json(body)).await;
    mount_no_mime(&h.server).await;
    let conn = outlook_with(
        &h,
        fast_retry(0),
        json!({"client_id": "cid", "folders": ["inbox"]}),
    );
    let sink = RecSink::default();
    let err = run(&conn, None, &sink).await.unwrap_err();
    assert!(
        matches!(err, Error::Invalid(ref m) if m.contains("outside the API")),
        "{err:?}"
    );
    assert!(sink.summary().is_empty());

    // A tampered cursor link is refused too.
    let sink = RecSink::default();
    let prior = json!({"v": 1, "folders": {"inbox": {"delta_link": format!("{}/v1.0/me?$deltatoken=x", evil.uri())}}});
    assert!(matches!(
        run(&conn, Some(prior), &sink).await,
        Err(Error::Invalid(_))
    ));
    let prior =
        json!({"v": 1, "folders": {"inbox": {"delta_link": format!("{}/other/api?x", h.uri())}}});
    assert!(matches!(
        run(&conn, Some(prior), &sink).await,
        Err(Error::Invalid(_))
    ));
}

#[tokio::test]
async fn hostile_payloads_are_contained() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let long_subject = "x".repeat(10_000);
    mount_initial(&h, "inbox", page(&h, "inbox", vec![
        json!("not an object"),
        json!({"id": 42}),
        json!({"id": "../../me/drive", "receivedDateTime": RECEIVED}),
        json!({"id": "nodate"}),
        json!({"id": "baddate", "receivedDateTime": "yesterday"}),
        json!({"id": "big", "receivedDateTime": RECEIVED}),
        json!({"id": "ok", "receivedDateTime": RECEIVED, "subject": format!("{long_subject}\u{0}\n"), "webLink": "javascript:alert(1)"}),
    ], None, Some("$deltatoken=d"))).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/me/messages/big/$value")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 36 * 1024 * 1024]))
        .mount(&h.server)
        .await;
    mount_mime(&h.server, "ok").await;
    let conn = outlook_with(
        &h,
        fast_retry(0),
        json!({"client_id": "cid", "folders": ["inbox"]}),
    );
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+inbox/ok"]);
    let d = &sink.upserts()[0];
    assert_eq!(d.title.as_ref().unwrap().chars().count(), 500);
    assert!(d.url.is_none());

    // Garbage instead of JSON is an error, not a panic.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_initial(
        &h,
        "inbox",
        ResponseTemplate::new(200).set_body_string("{\"value\": ["),
    )
    .await;
    let conn = outlook_with(
        &h,
        fast_retry(0),
        json!({"client_id": "cid", "folders": ["inbox"]}),
    );
    assert!(matches!(
        run(&conn, None, &RecSink::default()).await,
        Err(Error::Invalid(_))
    ));
    // A page with no links at all is malformed.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_initial(
        &h,
        "inbox",
        ResponseTemplate::new(200).set_body_json(json!({"value": []})),
    )
    .await;
    let conn = outlook_with(
        &h,
        fast_retry(0),
        json!({"client_id": "cid", "folders": ["inbox"]}),
    );
    assert!(matches!(
        run(&conn, None, &RecSink::default()).await,
        Err(Error::Invalid(_))
    ));
    // An unreadable cursor starts over instead of failing.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_initial(
        &h,
        "inbox",
        page(&h, "inbox", vec![], None, Some("$deltatoken=d")),
    )
    .await;
    let conn = outlook_with(
        &h,
        fast_retry(0),
        json!({"client_id": "cid", "folders": ["inbox"]}),
    );
    run(
        &conn,
        Some(json!({"v": "nope", "folders": 3})),
        &RecSink::default(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn missing_folder_is_not_found() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .and(path(delta_path("nosuch")))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error": {"code": "ErrorInvalidIdMalformed"}})),
        )
        .mount(&h.server)
        .await;
    let conn = outlook_with(
        &h,
        fast_retry(0),
        json!({"client_id": "cid", "folders": ["nosuch"]}),
    );
    assert!(matches!(
        run(&conn, None, &RecSink::default()).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn removal_later_in_the_same_pass_deletes_a_fresh_upsert() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_initial(
        &h,
        "inbox",
        page(
            &h,
            "inbox",
            vec![msg("m1", "Hi")],
            Some("$skiptoken=p2"),
            None,
        ),
    )
    .await;
    mount_token(
        &h,
        "inbox",
        "$skiptoken",
        "p2",
        page(
            &h,
            "inbox",
            vec![removed("m1")],
            None,
            Some("$deltatoken=d"),
        ),
    )
    .await;
    mount_mime(&h.server, "m1").await;
    let conn = outlook_with(
        &h,
        fast_retry(0),
        json!({"client_id": "cid", "folders": ["inbox"]}),
    );
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+inbox/m1", "-inbox/m1"]);
}
