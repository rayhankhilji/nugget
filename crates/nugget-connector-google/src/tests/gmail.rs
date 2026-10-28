use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use nugget_connector::Content;
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::GmailFactory;

const API: &str = "/gmail/v1/users/me";

fn settings() -> Value {
    json!({"client_id": "cid.apps.googleusercontent.com", "client_secret": "s"})
}

fn gmail(h: &Harness, retry: RetryPolicy) -> Arc<dyn Connector> {
    h.build(
        &GmailFactory::new()
            .with_endpoints(h.endpoints())
            .with_retry(retry),
        settings(),
    )
}

fn raw_msg(id: &str, history: &str, labels: &[&str], subject: &str) -> Value {
    let rfc822 = format!(
        "From: Alice <alice@example.com>\r\nTo: me@example.com\r\nSubject: {subject}\r\n\r\nBody of {id}\r\n"
    );
    json!({
        "id": id, "threadId": format!("t-{id}"), "labelIds": labels,
        "snippet": format!("Body of {id}"), "historyId": history,
        "internalDate": "1730000000000", "sizeEstimate": 123,
        "raw": URL_SAFE_NO_PAD.encode(rfc822),
    })
}

async fn mount_profile(server: &MockServer, history_id: &str) {
    Mock::given(method("GET"))
        .and(path(format!("{API}/profile")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "emailAddress": "me@example.com", "messagesTotal": 3, "historyId": history_id
        })))
        .mount(server)
        .await;
}

async fn mount_raw(server: &MockServer, id: &str, history: &str, labels: &[&str]) {
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/{id}")))
        .and(query_param("format", "raw"))
        .respond_with(ResponseTemplate::new(200).set_body_json(raw_msg(
            id,
            history,
            labels,
            &format!("Subject {id}"),
        )))
        .mount(server)
        .await;
}

async fn mount_list_page(
    server: &MockServer,
    token: Option<&str>,
    ids: &[&str],
    next: Option<&str>,
) {
    let mut body = json!({
        "messages": ids.iter().map(|id| json!({"id": id, "threadId": format!("t-{id}")})).collect::<Vec<_>>(),
        "resultSizeEstimate": ids.len(),
    });
    if let Some(n) = next {
        body["nextPageToken"] = n.into();
    }
    let m = Mock::given(method("GET")).and(path(format!("{API}/messages")));
    let m = match token {
        Some(t) => m.and(query_param("pageToken", t)),
        None => m.and(query_param_is_missing("pageToken")),
    };
    m.respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn sync_without_tokens_is_unauthorized_and_sends_nothing() {
    let h = Harness::new().await;
    let conn = gmail(&h, fast_retry(0));
    let sink = RecSink::default();
    assert!(is_unauthorized(&run(&conn, None, &sink).await));
    assert!(h.server.received_requests().await.unwrap().is_empty());
    assert!(sink.summary().is_empty());
}

#[tokio::test]
async fn backfill_pages_newest_first_with_checkpoints() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_profile(&h.server, "1000").await;
    mount_list_page(&h.server, None, &["m3", "m2"], Some("p2")).await;
    mount_list_page(&h.server, Some("p2"), &["m1"], None).await;
    mount_raw(&h.server, "m3", "990", &["INBOX", "UNREAD"]).await;
    mount_raw(&h.server, "m2", "980", &["INBOX"]).await;
    mount_raw(&h.server, "m1", "970", &["INBOX", "Label_7"]).await;

    let conn = gmail(&h, fast_retry(0));
    let sink = RecSink::default();
    let cursor = run(&conn, None, &sink).await.unwrap();

    assert_eq!(sink.summary(), vec!["+m3", "+m2", "+m1"]);
    assert_eq!(
        cursor,
        json!({"phase": "incremental", "history_id": "1000"})
    );

    let cps = sink.checkpoints.lock().unwrap().clone();
    assert_eq!(cps.len(), 2, "{cps:?}");
    assert_eq!(cps[0]["phase"], "backfill");
    assert_eq!(cps[0]["history_id"], "1000");
    assert!(cps[0]["page_token"].is_null());
    assert_eq!(cps[1]["page_token"], "p2");
    assert_eq!(cps[1]["history_id"], "1000");

    // The window query is applied: after:<now - 365 days>.
    let after = cps[0]["after"].as_i64().unwrap();
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    assert!((now - 365 * 86_400 - after).abs() < 60);
    let reqs = h.server.received_requests().await.unwrap();
    let list = reqs
        .iter()
        .find(|r| r.url.path() == format!("{API}/messages"))
        .unwrap();
    assert!(
        list.url
            .query()
            .unwrap()
            .contains(&format!("q=after%3A{after}"))
    );
    assert!(!list.url.query().unwrap().contains("includeSpamTrash"));

    let docs = sink.upserts();
    let d = &docs[0];
    assert_eq!(d.mime, "message/rfc822");
    assert!(matches!(&d.content, Content::Bytes(b) if b.starts_with(b"From: Alice")));
    // `<historyId>@<internalDate ms>`: the date lets a resync tell old mail from deleted mail.
    assert_eq!(d.version.as_deref(), Some("990@1730000000000"));
    assert_eq!(d.title.as_deref(), Some("Subject m3"));
    assert_eq!(
        d.url.as_deref(),
        Some("https://mail.google.com/mail/u/0/#all/m3")
    );
    assert_eq!(d.metadata["thread_id"], "t-m3");
    assert_eq!(d.metadata["labels"], json!(["INBOX", "UNREAD"]));
    assert_eq!(d.metadata["from"], "Alice <alice@example.com>");
    assert_eq!(d.metadata["subject"], "Subject m3");
    assert_eq!(d.metadata["snippet"], "Body of m3");
    assert_eq!(d.modified_at.unwrap().unix_timestamp(), 1_730_000_000);
    h.assert_egress("sync:gmail").await;
}

#[tokio::test]
async fn backfill_resumes_from_page_token_and_skips_known() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    h.know("m1", "970");
    // Page 1 and the profile must not be requested again.
    Mock::given(method("GET"))
        .and(path(format!("{API}/profile")))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&h.server)
        .await;
    mount_list_page(&h.server, Some("p2"), &["m1", "m0"], None).await;
    mount_raw(&h.server, "m0", "960", &["INBOX"]).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/m1")))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&h.server)
        .await;

    let conn = gmail(&h, fast_retry(0));
    let sink = RecSink::default();
    let cursor = json!({"phase": "backfill", "history_id": "1000", "after": 1, "page_token": "p2", "resync": false});
    let out = run(&conn, Some(cursor), &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+m0"]);
    assert_eq!(out, json!({"phase": "incremental", "history_id": "1000"}));
    let q = h.server.received_requests().await.unwrap()[0]
        .url
        .query()
        .unwrap()
        .to_string();
    assert!(q.contains("q=after%3A1"), "{q}");
}

#[tokio::test]
async fn drafts_spam_and_trash_are_not_indexed() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_profile(&h.server, "5").await;
    mount_list_page(&h.server, None, &["d1", "s1", "ok"], None).await;
    mount_raw(&h.server, "d1", "1", &["DRAFT"]).await;
    mount_raw(&h.server, "s1", "1", &["SPAM"]).await;
    mount_raw(&h.server, "ok", "1", &["INBOX"]).await;
    let conn = gmail(&h, fast_retry(0));
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+ok"]);
}

fn history_page(records: Value, next: Option<&str>, history_id: &str) -> ResponseTemplate {
    let mut body = json!({"history": records, "historyId": history_id});
    if let Some(n) = next {
        body["nextPageToken"] = n.into();
    }
    ResponseTemplate::new(200).set_body_json(body)
}

#[tokio::test]
async fn history_deltas_add_delete_and_relabel() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let records_p1 = json!([
        {"id": "101", "messagesAdded": [{"message": {"id": "m3", "labelIds": ["INBOX", "UNREAD"]}}]},
        {"id": "102", "messagesDeleted": [{"message": {"id": "m1"}}]},
        {"id": "103", "labelsAdded": [{"message": {"id": "m2", "labelIds": ["TRASH"]}, "labelIds": ["TRASH"]}]},
        {"id": "104", "messagesAdded": [{"message": {"id": "dr", "labelIds": ["DRAFT"]}}]},
    ]);
    let records_p2 = json!([
        {"id": "105", "labelsRemoved": [{"message": {"id": "m4", "labelIds": ["INBOX"]}, "labelIds": ["Label_9"]}]},
        {"id": "106", "labelsRemoved": [{"message": {"id": "m5", "labelIds": ["INBOX"]}, "labelIds": ["UNREAD"]}]},
        {"id": "107", "messagesAdded": [{"message": {"id": "m6", "labelIds": ["INBOX"]}}]},
        {"id": "108", "messagesDeleted": [{"message": {"id": "m6"}}]},
    ]);
    Mock::given(method("GET"))
        .and(path(format!("{API}/history")))
        .and(query_param("startHistoryId", "100"))
        .and(query_param_is_missing("pageToken"))
        .respond_with(history_page(records_p1, Some("hp2"), "200"))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/history")))
        .and(query_param("startHistoryId", "100"))
        .and(query_param("pageToken", "hp2"))
        .respond_with(history_page(records_p2, None, "200"))
        .mount(&h.server)
        .await;
    mount_raw(&h.server, "m3", "101", &["INBOX", "UNREAD"]).await;
    mount_raw(&h.server, "m4", "105", &["INBOX"]).await;
    for id in ["m5", "m6", "dr"] {
        Mock::given(method("GET"))
            .and(path(format!("{API}/messages/{id}")))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&h.server)
            .await;
    }

    let conn = gmail(&h, fast_retry(0));
    let sink = RecSink::default();
    let out = run(
        &conn,
        Some(json!({"phase": "incremental", "history_id": "100"})),
        &sink,
    )
    .await
    .unwrap();
    assert_eq!(sink.summary(), vec!["+m3", "-m1", "-m2", "+m4", "-m6"]);
    assert_eq!(out, json!({"phase": "incremental", "history_id": "200"}));
    // Checkpoint between history pages at the last processed record.
    let cps = sink.checkpoints.lock().unwrap().clone();
    assert_eq!(
        cps,
        vec![json!({"phase": "incremental", "history_id": "104"})]
    );
    let reqs = h.server.received_requests().await.unwrap();
    let q = reqs[0].url.query().unwrap();
    for t in [
        "messageAdded",
        "messageDeleted",
        "labelAdded",
        "labelRemoved",
    ] {
        assert!(q.contains(&format!("historyTypes={t}")), "{q}");
    }
    h.assert_egress("sync:gmail").await;
}

#[tokio::test]
async fn added_message_gone_before_fetch_becomes_delete() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .and(path(format!("{API}/history")))
        .respond_with(history_page(
            json!([{"id": "11", "messagesAdded": [{"message": {"id": "gone", "labelIds": ["INBOX"]}}]}]),
            None,
            "12",
        ))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/gone")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.server)
        .await;
    let conn = gmail(&h, fast_retry(0));
    let sink = RecSink::default();
    let out = run(
        &conn,
        Some(json!({"phase": "incremental", "history_id": "10"})),
        &sink,
    )
    .await
    .unwrap();
    assert_eq!(sink.summary(), vec!["-gone"]);
    assert_eq!(out["history_id"], "12");
}

#[tokio::test]
async fn expired_history_id_triggers_bounded_resync() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    h.know("m1", "50"); // still exists (older than the window)
    h.know("gone", "40"); // deleted while history was unavailable
    h.know("trashed", "41"); // moved to trash meanwhile
    h.know("m4", "60"); // listed again: neither fetched nor checked
    Mock::given(method("GET"))
        .and(path(format!("{API}/history")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": {"code": 404, "message": "Requested entity was not found.", "status": "NOT_FOUND"}
        })))
        .expect(1)
        .mount(&h.server)
        .await;
    mount_profile(&h.server, "2000").await;
    mount_list_page(&h.server, None, &["m5", "m4"], None).await;
    mount_raw(&h.server, "m5", "1999", &["INBOX"]).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/m4")))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/m1")))
        .and(query_param("format", "minimal"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": "m1", "labelIds": ["INBOX"]})),
        )
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/trashed")))
        .and(query_param("format", "minimal"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "trashed", "labelIds": ["TRASH"]})),
        )
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/gone")))
        .and(query_param("format", "minimal"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.server)
        .await;

    let conn = gmail(&h, fast_retry(0));
    let sink = RecSink::default();
    let out = run(
        &conn,
        Some(json!({"phase": "incremental", "history_id": "7"})),
        &sink,
    )
    .await
    .unwrap();
    let mut summary = sink.summary();
    assert_eq!(summary.remove(0), "+m5");
    summary.sort();
    assert_eq!(summary, vec!["-gone", "-trashed"]);
    assert_eq!(out, json!({"phase": "incremental", "history_id": "2000"}));
    let cps = sink.checkpoints.lock().unwrap().clone();
    assert_eq!(cps[0]["phase"], "backfill");
    assert_eq!(cps[0]["resync"], true);
    assert_eq!(cps[0]["history_id"], "2000");
    h.assert_egress("sync:gmail").await;
}

fn now_ms() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp() * 1000
}

async fn mount_minimal(server: &MockServer, id: &str, status: u16, labels: &[&str]) {
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/{id}")))
        .and(query_param("format", "minimal"))
        .respond_with(
            ResponseTemplate::new(status).set_body_json(json!({"id": id, "labelIds": labels})),
        )
        .mount(server)
        .await;
}

async fn mount_history_empty(server: &MockServer, history_id: &str) {
    Mock::given(method("GET"))
        .and(path(format!("{API}/history")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"historyId": history_id})))
        .mount(server)
        .await;
}

#[tokio::test]
async fn a_reset_cursor_with_indexed_mail_reconciles_everything() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let old = now_ms() - 400 * 86_400_000;
    h.know("m1", &format!("5@{}", now_ms())); // listed again
    h.know("spam1", &format!("6@{}", now_ms())); // now excluded: in spam
    h.know("oldacct", &format!("7@{old}")); // another account's mail, older than the window
    mount_profile(&h.server, "3000").await;
    mount_list_page(&h.server, None, &["m1"], None).await;
    mount_minimal(&h.server, "spam1", 200, &["SPAM"]).await;
    // Older than the window: out of scope, deleted without a request.
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/oldacct")))
        .respond_with(ResponseTemplate::new(404))
        .expect(0)
        .mount(&h.server)
        .await;
    let conn = gmail(&h, fast_retry(0));
    let sink = RecSink::default();
    let out = run(&conn, None, &sink).await.unwrap();
    let mut summary = sink.summary();
    summary.sort();
    assert_eq!(summary, vec!["-oldacct", "-spam1"]);
    assert_eq!(out, json!({"phase": "incremental", "history_id": "3000"}));
    let cps = sink.checkpoints.lock().unwrap().clone();
    assert_eq!(cps[0]["resync"], true);
    assert_eq!(cps[0]["reconcile"], true);

    // A first run (nothing indexed) is a plain backfill.
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_profile(&h.server, "1").await;
    mount_list_page(&h.server, None, &[], None).await;
    let sink = RecSink::default();
    run(&gmail(&h, fast_retry(0)), None, &sink).await.unwrap();
    let cps = sink.checkpoints.lock().unwrap().clone();
    assert_eq!(cps[0]["resync"], false);
}

#[tokio::test]
async fn resync_checks_in_window_mail_newest_first_and_carries_the_rest() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let now = now_ms();
    let old = now - 400 * 86_400_000;
    // 600 indexed messages inside the window, all deleted while history was unavailable, and
    // 50 old ones that still exist (outside the window, so never listed).
    let gone: Vec<String> = (0..600).map(|i| format!("19{i:014x}")).collect();
    for id in &gone {
        h.know(id, &format!("1@{now}"));
    }
    for i in 0..50 {
        h.know(&format!("10{i:014x}"), &format!("1@{old}"));
    }
    Mock::given(method("GET"))
        .and(path(format!("{API}/history")))
        .respond_with(ResponseTemplate::new(404))
        .up_to_n_times(1)
        .mount(&h.server)
        .await;
    mount_profile(&h.server, "5000").await;
    mount_list_page(&h.server, None, &[], None).await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(format!(
            "^{API}/messages/19[0-9a-f]+$"
        )))
        .and(query_param("format", "minimal"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex(format!(
            "^{API}/messages/10[0-9a-f]+$"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"labelIds": ["INBOX"]})))
        .expect(0)
        .mount(&h.server)
        .await;

    let conn = gmail(&h, fast_retry(0));
    let sink = RecSink::default();
    let out = run(
        &conn,
        Some(json!({"phase": "incremental", "history_id": "7"})),
        &sink,
    )
    .await
    .unwrap();
    let deleted: Vec<String> = sink.summary().iter().map(|s| s[1..].to_string()).collect();
    assert_eq!(deleted.len(), 500);
    // Newest first: the 500 highest ids.
    let mut want: Vec<String> = gone.clone();
    want.sort();
    want.reverse();
    let mut got = deleted.clone();
    got.sort();
    got.reverse();
    assert_eq!(got, want[..500].to_vec());
    let carried: Vec<String> = serde_json::from_value(out["verify"].clone()).unwrap();
    assert_eq!(carried.len(), 100);
    assert_eq!(out["history_id"], "5000");

    // The next (incremental) pass checks the rest; nothing is left after it.
    mount_history_empty(&h.server, "5001").await;
    let sink = RecSink::default();
    let out = run(&conn, Some(out), &sink).await.unwrap();
    assert_eq!(sink.summary().len(), 100);
    assert_eq!(out, json!({"phase": "incremental", "history_id": "5001"}));
}

#[tokio::test]
async fn an_interrupted_resync_lists_again_instead_of_taking_listed_mail_for_missing() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    h.know("m3", &format!("1@{}", now_ms())); // listed on page 1, before the interruption
    mount_list_page(&h.server, None, &["m3"], Some("p2")).await;
    mount_list_page(&h.server, Some("p2"), &["m1"], None).await;
    mount_raw(&h.server, "m1", "9", &["INBOX"]).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/m3")))
        .respond_with(ResponseTemplate::new(404))
        .expect(0)
        .mount(&h.server)
        .await;
    let conn = gmail(&h, fast_retry(0));
    let sink = RecSink::default();
    let cursor = json!({"phase": "backfill", "history_id": "100", "after": 1, "page_token": "p2", "resync": true});
    let out = run(&conn, Some(cursor), &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+m1"]);
    assert_eq!(out, json!({"phase": "incremental", "history_id": "100"}));
}

#[tokio::test]
async fn rate_limit_429_is_retried_with_backoff() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_profile(&h.server, "5").await;
    mount_list_page(&h.server, None, &["m1"], None).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/m1")))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/m1")))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error": {
            "code": 403, "message": "User-rate limit exceeded",
            "errors": [{"reason": "userRateLimitExceeded", "domain": "usageLimits"}]
        }})))
        .up_to_n_times(1)
        .with_priority(2)
        .mount(&h.server)
        .await;
    mount_raw(&h.server, "m1", "4", &["INBOX"]).await;
    let conn = gmail(&h, fast_retry(3));
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+m1"]);
    let gets = h
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().ends_with("/messages/m1"))
        .count();
    assert_eq!(gets, 3);
    // Every attempt is in the ledger.
    h.assert_egress("sync:gmail").await;
}

#[tokio::test]
async fn persistent_rate_limit_returns_rate_limited_after_retries() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_profile(&h.server, "5").await;
    mount_list_page(&h.server, None, &["m1"], None).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/m1")))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .expect(3)
        .mount(&h.server)
        .await;
    let conn = gmail(&h, fast_retry(2));
    let sink = RecSink::default();
    let err = run(&conn, None, &sink).await.unwrap_err();
    assert!(matches!(err, Error::RateLimited { .. }), "{err}");
    // The starting checkpoint (with the mailbox history id) was kept for the next attempt.
    assert_eq!(sink.checkpoints.lock().unwrap()[0]["history_id"], "5");
}

#[tokio::test]
async fn forbidden_without_rate_reason_is_unauthorized() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"error": {
            "code": 403, "message": "Gmail API has not been used in project 123",
            "errors": [{"reason": "accessNotConfigured"}]
        }})))
        .expect(1)
        .mount(&h.server)
        .await;
    let conn = gmail(&h, fast_retry(3));
    let err = run(&conn, None, &RecSink::default()).await.unwrap_err();
    assert!(
        matches!(err, Error::Unauthorized(ref m) if m.contains("accessNotConfigured")),
        "{err}"
    );
}

#[tokio::test]
async fn expired_access_token_is_refreshed_through_metered_http() {
    let h = Harness::new().await;
    h.seed_tokens("old", true);
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "new", "expires_in": 3600
        })))
        .expect(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/history")))
        .and(header("authorization", "Bearer new"))
        .respond_with(history_page(json!([]), None, "9"))
        .expect(1)
        .mount(&h.server)
        .await;
    let conn = gmail(&h, fast_retry(0));
    let out = run(
        &conn,
        Some(json!({"phase": "incremental", "history_id": "9"})),
        &RecSink::default(),
    )
    .await
    .unwrap();
    assert_eq!(out["history_id"], "9");
    h.assert_egress("sync:gmail").await;
}

#[tokio::test]
async fn api_401_forces_one_refresh_then_retries() {
    let h = Harness::new().await;
    h.seed_tokens("revoked-early", false);
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "second", "expires_in": 3600
        })))
        .expect(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(header("authorization", "Bearer revoked-early"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/history")))
        .and(header("authorization", "Bearer second"))
        .respond_with(history_page(json!([]), None, "9"))
        .mount(&h.server)
        .await;
    let conn = gmail(&h, fast_retry(0));
    run(
        &conn,
        Some(json!({"phase": "incremental", "history_id": "9"})),
        &RecSink::default(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn refresh_invalid_grant_makes_sync_unauthorized() {
    let h = Harness::new().await;
    h.seed_tokens("old", true);
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})))
        .mount(&h.server)
        .await;
    let conn = gmail(&h, fast_retry(0));
    let r = run(&conn, None, &RecSink::default()).await;
    assert!(is_unauthorized(&r), "{r:?}");
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    let elsewhere = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"historyId": "1"})))
        .expect(0)
        .mount(&elsewhere)
        .await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/steal", elsewhere.uri())),
        )
        .mount(&h.server)
        .await;
    let conn = gmail(&h, fast_retry(3));
    let err = run(&conn, None, &RecSink::default()).await.unwrap_err();
    assert!(
        matches!(err, Error::Network(ref m) if m.contains("redirect")),
        "{err}"
    );
    assert_eq!(
        h.egress.0.lock().unwrap().len(),
        1,
        "no retries on a redirect"
    );
}

#[tokio::test]
async fn malformed_responses_are_errors_not_panics() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_profile(&h.server, "5").await;
    mount_list_page(&h.server, None, &["bad", "../evil", "ok"], None).await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages/bad")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "bad", "labelIds": ["INBOX"], "raw": "!!!not base64!!!"
        })))
        .mount(&h.server)
        .await;
    mount_raw(&h.server, "ok", "3", &["INBOX"]).await;
    let conn = gmail(&h, fast_retry(0));
    let sink = RecSink::default();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+ok"]);

    // A garbage list body is a typed error.
    let h2 = Harness::new().await;
    h2.seed_tokens("at", false);
    mount_profile(&h2.server, "5").await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages")))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
        .mount(&h2.server)
        .await;
    let conn = gmail(&h2, fast_retry(0));
    assert!(matches!(
        run(&conn, None, &RecSink::default()).await,
        Err(Error::Invalid(_))
    ));
}

#[tokio::test]
async fn unreadable_cursor_starts_over_and_cancel_is_honoured() {
    let h = Harness::new().await;
    h.seed_tokens("at", false);
    mount_profile(&h.server, "5").await;
    mount_list_page(&h.server, None, &[], None).await;
    let conn = gmail(&h, fast_retry(0));
    let out = run(&conn, Some(json!({"phase": "???"})), &RecSink::default())
        .await
        .unwrap();
    assert_eq!(out["history_id"], "5");

    let cancel = CancellationToken::new();
    cancel.cancel();
    let r = conn
        .sync(
            Some(json!({"phase": "incremental", "history_id": "5"})),
            &RecSink::default(),
            &cancel,
        )
        .await;
    assert!(matches!(r, Err(Error::Cancelled)));
}
