use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::SlackFactory;
use crate::text::{DAY_SECS, date, day_start, now_unix};

fn slack(h: &Harness, settings: Value) -> Arc<dyn Connector> {
    h.build(
        &SlackFactory::new()
            .with_endpoints(h.endpoints())
            .with_retry(fast_retry(2)),
        settings,
    )
}

fn ok(body: Value) -> ResponseTemplate {
    let mut b = body;
    b["ok"] = json!(true);
    ResponseTemplate::new(200).set_body_json(b)
}

async fn mount_auth(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/slack/auth.test"))
        .and(header("authorization", "Bearer xoxp-1"))
        .respond_with(ok(json!({
            "url": "https://acme.slack.com/", "team": "Acme", "user": "ada", "user_id": "U1"
        })))
        .mount(server)
        .await;
    for (id, name) in [("U1", "Ada"), ("U2", "Bob")] {
        Mock::given(method("GET"))
            .and(path("/slack/users.info"))
            .and(query_param("user", id))
            .respond_with(ok(
                json!({"user": {"name": name.to_lowercase(), "profile": {"display_name": name}}}),
            ))
            .mount(server)
            .await;
    }
}

async fn mount_convs(server: &MockServer, chans: Value) {
    Mock::given(method("GET"))
        .and(path("/slack/users.conversations"))
        .respond_with(ok(
            json!({"channels": chans, "response_metadata": {"next_cursor": ""}}),
        ))
        .mount(server)
        .await;
}

/// A range read of `chan`'s history (not the one-message probe).
async fn mount_history(
    server: &MockServer,
    chan: &str,
    cursor: Option<&str>,
    msgs: Value,
    next: Option<&str>,
) {
    let mut m = Mock::given(method("GET"))
        .and(path("/slack/conversations.history"))
        .and(query_param("channel", chan))
        .and(query_param("limit", "200"));
    if let Some(c) = cursor {
        m = m.and(query_param("cursor", c));
    }
    m.respond_with(ok(json!({
        "messages": msgs, "has_more": next.is_some(),
        "response_metadata": {"next_cursor": next.unwrap_or("")}
    })))
    .with_priority(if cursor.is_some() { 1 } else { 5 })
    .mount(server)
    .await;
}

async fn mount_probe(server: &MockServer, chan: &str, msgs: Value) {
    Mock::given(method("GET"))
        .and(path("/slack/conversations.history"))
        .and(query_param("channel", chan))
        .and(query_param("limit", "1"))
        .respond_with(ok(json!({"messages": msgs, "has_more": false})))
        .mount(server)
        .await;
}

fn ts(secs: i64, n: u32) -> String {
    format!("{secs}.{n:06}")
}

/// Two message times today (not after now) and two yesterday.
fn times() -> (i64, i64, i64, i64) {
    let now = now_unix();
    let today = day_start(now);
    let a = today + (now - today) / 2;
    (a, a + 1, today - DAY_SECS + 3600, today - DAY_SECS + 7200)
}

#[tokio::test]
async fn sync_without_token_is_unauthorized_and_sends_nothing() {
    let h = Harness::new().await;
    let conn = slack(&h, json!({}));
    let sink = h.sink();
    assert!(is_unauthorized(&run(&conn, None, &sink).await));
    assert!(sink.summary().is_empty());
    assert!(h.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn rejected_token_is_unauthorized() {
    let h = Harness::new().await;
    h.paste("xoxp-bad");
    Mock::given(method("GET"))
        .and(path("/slack/auth.test"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"ok": false, "error": "invalid_auth"})),
        )
        .mount(&h.server)
        .await;
    let conn = slack(&h, json!({}));
    let r = run(&conn, None, &h.sink()).await;
    assert!(is_unauthorized(&r), "{r:?}");
}

#[tokio::test]
async fn backfill_pages_into_day_and_thread_windows() {
    let h = Harness::new().await;
    h.paste("xoxp-1");
    mount_auth(&h.server).await;
    mount_convs(
        &h.server,
        json!([
            {"id": "C1", "name": "general", "is_channel": true},
            {"id": "D1", "is_im": true, "user": "U2"}
        ]),
    )
    .await;
    let (t1, t2, y1, y2) = times();
    let parent = ts(t1, 100);
    mount_history(
        &h.server,
        "C1",
        None,
        json!([
            {"ts": ts(t2, 0), "user": "U2", "text": "hey <@U1> see <https://x.io|the *doc*>"},
            {"ts": parent, "user": "U1", "text": "*plan* for today", "thread_ts": parent,
             "reply_count": 2, "latest_reply": ts(t2, 5)},
            {"ts": ts(t1, 50), "user": "U2", "subtype": "channel_join", "text": "joined"},
        ]),
        Some("p2"),
    )
    .await;
    mount_history(
        &h.server,
        "C1",
        Some("p2"),
        json!([
            {"ts": ts(y2, 0), "user": "U1", "text": "yesterday 2", "edited": {"ts": ts(y2, 9)}},
            {"ts": ts(y1, 0), "bot_id": "B1", "bot_profile": {"name": "CI"}, "text": "build ok",
             "files": [{"name": "log.txt"}]},
        ]),
        None,
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/slack/conversations.replies"))
        .and(query_param("ts", parent.as_str()))
        .respond_with(ok(json!({"messages": [
            {"ts": parent, "user": "U1", "text": "*plan* for today", "thread_ts": parent, "reply_count": 2},
            {"ts": ts(t1, 200), "user": "U2", "text": "sounds good", "thread_ts": parent},
            {"ts": ts(t2, 5), "user": "U1", "text": "great", "thread_ts": parent},
        ], "has_more": false})))
        .expect(1)
        .mount(&h.server)
        .await;
    mount_history(&h.server, "D1", None, json!([]), None).await;

    let conn = slack(&h, json!({"window_days": 7}));
    let sink = h.sink();
    let cursor = run(&conn, None, &sink).await.unwrap();

    let today = date(t1);
    let yday = date(y1);
    let thread_key = format!("C1/thread/{parent}");
    let mut keys = sink.summary();
    keys.sort();
    let mut want = vec![
        format!("+C1/day/{today}"),
        format!("+C1/day/{yday}"),
        format!("+{thread_key}"),
    ];
    want.sort();
    assert_eq!(keys, want);

    let day = sink.text(&format!("C1/day/{today}"));
    assert!(
        day.starts_with(&format!("#general — {today} (UTC)")),
        "{day}"
    );
    assert!(
        day.contains("Bob: hey @Ada see the doc (https://x.io)"),
        "{day}"
    );
    assert!(
        day.contains("Ada: plan for today [thread: 2 replies]"),
        "{day}"
    );
    assert!(!day.contains("joined"));
    let yd = sink.text(&format!("C1/day/{yday}"));
    assert!(yd.contains("CI: build ok [file: log.txt]"), "{yd}");
    assert!(yd.contains("yesterday 2 (edited)"));
    // Oldest first within the day.
    assert!(yd.find("build ok").unwrap() < yd.find("yesterday 2").unwrap());

    let thread = sink.doc(&thread_key);
    let text = sink.text(&thread_key);
    assert!(
        text.starts_with("Thread in #general: plan for today"),
        "{text}"
    );
    assert!(text.contains("  [") && text.contains("Bob: sounds good"));
    assert!(
        thread
            .version
            .as_deref()
            .unwrap()
            .starts_with(&format!("2:{}:#", ts(t2, 5)))
    );
    assert_eq!(thread.metadata["from_me"], json!(true));
    assert_eq!(thread.metadata["my_message_count"], json!(2));
    assert_eq!(thread.metadata["me"], json!("Ada"));
    assert_eq!(thread.metadata["channel"], json!("#general"));
    assert_eq!(thread.metadata["thread_ts"], json!(parent));
    assert_eq!(
        thread.url.as_deref(),
        Some(
            format!(
                "https://acme.slack.com/archives/C1/p{}",
                parent.replace('.', "")
            )
            .as_str()
        )
    );
    let d = sink.doc(&format!("C1/day/{yday}"));
    assert_eq!(d.metadata["team"], json!("Acme"));
    assert_eq!(d.metadata["authors"], json!(["CI", "Ada"]));

    assert_eq!(cursor["user_id"], json!("U1"));
    assert_eq!(cursor["channels"]["C1"]["latest"], json!(ts(t2, 0)));
    assert!(cursor["channels"]["C1"].get("backfill").is_none());
    assert!(cursor["channels"]["D1"]["latest"].is_string());
    // The backfill checkpointed its progress after the first page finished a day.
    let cps = sink.checkpoints.lock().unwrap().clone();
    assert!(
        cps.iter()
            .any(|c| c["channels"]["C1"]["backfill"]["before"].as_i64() == Some(day_start(t1))),
        "{cps:?}"
    );
    h.assert_egress("sync:slack").await;
}

#[tokio::test]
async fn incremental_probe_rebuilds_only_when_something_is_new() {
    let h = Harness::new().await;
    h.paste("xoxp-1");
    mount_auth(&h.server).await;
    mount_convs(&h.server, json!([{"id": "C1", "name": "general"}])).await;
    let (t1, t2, _, _) = times();
    let now = now_unix();
    let cursor = json!({"v": 1, "user_id": "U1", "channels": {
        "C1": {"latest": ts(t1, 0), "checked_at": now}
    }});
    // Nothing newer: only the probe.
    mount_probe(&h.server, "C1", json!([])).await;
    Mock::given(method("GET"))
        .and(path("/slack/conversations.history"))
        .and(query_param("limit", "200"))
        .respond_with(ok(json!({"messages": []})))
        .expect(0)
        .mount(&h.server)
        .await;
    let conn = slack(&h, json!({}));
    let sink = h.sink();
    let next = run(&conn, Some(cursor.clone()), &sink).await.unwrap();
    assert!(sink.summary().is_empty());
    assert_eq!(next["channels"]["C1"]["latest"], json!(ts(t1, 0)));
    h.server.verify().await;

    // Something new: the day of `latest` is rebuilt with every message.
    h.server.reset().await;
    mount_auth(&h.server).await;
    mount_convs(&h.server, json!([{"id": "C1", "name": "general"}])).await;
    mount_probe(
        &h.server,
        "C1",
        json!([{"ts": ts(t2, 0), "user": "U2", "text": "new"}]),
    )
    .await;
    mount_history(
        &h.server,
        "C1",
        None,
        json!([
            {"ts": ts(t2, 0), "user": "U2", "text": "new"},
            {"ts": ts(t1, 0), "user": "U1", "text": "old"}
        ]),
        None,
    )
    .await;
    let sink = h.sink();
    let next = run(&conn, Some(cursor), &sink).await.unwrap();
    let key = format!("C1/day/{}", date(t1));
    assert_eq!(sink.summary(), vec![format!("+{key}")]);
    let text = sink.text(&key);
    assert!(text.contains("Ada: old") && text.contains("Bob: new"));
    assert_eq!(next["channels"]["C1"]["latest"], json!(ts(t2, 0)));
    let reqs = h.server.received_requests().await.unwrap();
    let range = reqs
        .iter()
        .find(|r| {
            r.url.path().ends_with("conversations.history")
                && r.url.query().unwrap_or_default().contains("limit=200")
        })
        .unwrap();
    let q: HashMap<String, String> = range.url.query_pairs().into_owned().collect();
    assert_eq!(q["oldest"], format!("{}.000000", day_start(t1)));
}

#[tokio::test]
async fn recheck_picks_up_edits_deletions_and_new_replies() {
    let h = Harness::new().await;
    h.paste("xoxp-1");
    mount_auth(&h.server).await;
    mount_convs(&h.server, json!([{"id": "C1", "name": "general"}])).await;
    let (t1, _, y1, _) = times();
    let two_days = day_start(t1) - 2 * DAY_SECS + 60;
    let parent = ts(t1, 1);
    let gone_parent = ts(y1, 1);
    // Indexed: today, yesterday (all deleted since), two days ago (outside the recheck: kept),
    // a thread whose reply count changed, and a thread whose parent was deleted.
    h.know(&format!("C1/day/{}", date(t1)), "old");
    h.know(&format!("C1/day/{}", date(y1)), "old");
    h.know(&format!("C1/day/{}", date(two_days)), "old");
    h.know(
        &format!("C1/thread/{parent}"),
        &format!("1:{}:#x", ts(t1, 2)),
    );
    h.know(&format!("C1/thread/{gone_parent}"), "1:x:#x");
    mount_history(
        &h.server,
        "C1",
        None,
        json!([{"ts": parent, "user": "U1", "text": "edited text", "edited": {"ts": ts(t1, 9)},
                "thread_ts": parent, "reply_count": 2, "latest_reply": ts(t1, 3)}]),
        None,
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/slack/conversations.replies"))
        .respond_with(ok(json!({"messages": [
            {"ts": parent, "user": "U1", "text": "edited text", "thread_ts": parent},
            {"ts": ts(t1, 2), "user": "U2", "text": "r1", "thread_ts": parent},
            {"ts": ts(t1, 3), "user": "U2", "text": "r2", "thread_ts": parent}
        ]})))
        .expect(1)
        .mount(&h.server)
        .await;
    let now = now_unix();
    let cursor = json!({"v": 1, "user_id": "U1", "channels": {
        "C1": {"latest": ts(t1, 1), "checked_at": now - crate::slack::RECHECK_INTERVAL_SECS - 1}
    }});
    let conn = slack(&h, json!({"recheck_days": 1}));
    let sink = h.sink();
    let next = run(&conn, Some(cursor), &sink).await.unwrap();
    let mut s = sink.summary();
    s.sort();
    let mut want = vec![
        format!("+C1/day/{}", date(t1)),
        format!("+C1/thread/{parent}"),
        format!("-C1/day/{}", date(y1)),
        format!("-C1/thread/{gone_parent}"),
    ];
    want.sort();
    assert_eq!(s, want);
    assert!(
        sink.text(&format!("C1/day/{}", date(t1)))
            .contains("edited text (edited)")
    );
    assert!(sink.text(&format!("C1/thread/{parent}")).contains("r2"));
    assert!(
        h.known_keys()
            .contains(&format!("C1/day/{}", date(two_days)))
    );
    assert!(next["channels"]["C1"]["checked_at"].as_i64().unwrap() >= now);
}

#[tokio::test]
async fn unchanged_threads_are_not_refetched() {
    let h = Harness::new().await;
    h.paste("xoxp-1");
    mount_auth(&h.server).await;
    mount_convs(&h.server, json!([{"id": "C1", "name": "general"}])).await;
    let (t1, _, _, _) = times();
    let parent = ts(t1, 1);
    h.know(
        &format!("C1/thread/{parent}"),
        &format!("1:{}:#abc", ts(t1, 2)),
    );
    mount_history(
        &h.server,
        "C1",
        None,
        json!([{"ts": parent, "user": "U1", "text": "p", "thread_ts": parent, "reply_count": 1, "latest_reply": ts(t1, 2)}]),
        None,
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/slack/conversations.replies"))
        .respond_with(ok(json!({"messages": []})))
        .expect(0)
        .mount(&h.server)
        .await;
    let conn = slack(&h, json!({}));
    let sink = h.sink();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec![format!("+C1/day/{}", date(t1))]);
    assert!(h.known_keys().contains(&format!("C1/thread/{parent}")));
}

#[tokio::test]
async fn a_reset_cursor_reconciles_conversations_and_the_window() {
    let h = Harness::new().await;
    h.paste("xoxp-1");
    mount_auth(&h.server).await;
    mount_convs(&h.server, json!([{"id": "C1", "name": "general"}])).await;
    let (t1, _, y1, _) = times();
    let old = day_start(t1) - 30 * DAY_SECS;
    h.know(&format!("C1/day/{}", date(y1)), "v"); // in the window, gone now
    h.know(&format!("C1/day/{}", date(old)), "v"); // outside the new 7-day window
    h.know(&format!("C1/thread/{}", ts(old, 1)), "v");
    h.know("C9/day/2026-01-01", "v"); // a conversation no longer synced
    h.know("C9/thread/1767225600.000100", "v");
    mount_history(
        &h.server,
        "C1",
        None,
        json!([{"ts": ts(t1, 0), "user": "U1", "text": "hi"}]),
        None,
    )
    .await;
    let conn = slack(&h, json!({"window_days": 7}));
    let sink = h.sink();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(h.known_keys(), vec![format!("C1/day/{}", date(t1))]);
}

#[tokio::test]
async fn configured_conversations_resolve_ids_and_names() {
    let h = Harness::new().await;
    h.paste("xoxp-1");
    mount_auth(&h.server).await;
    Mock::given(method("GET"))
        .and(path("/slack/conversations.info"))
        .and(query_param("channel", "C0123456789"))
        .respond_with(ok(
            json!({"channel": {"id": "C0123456789", "name": "eng", "is_private": true}}),
        ))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slack/users.conversations"))
        .respond_with(ok(json!({"channels": [{"id": "C2", "name": "random"}]})))
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slack/conversations.list"))
        .respond_with(ok(json!({"channels": []})))
        .mount(&h.server)
        .await;
    for c in ["C0123456789", "C2"] {
        mount_history(
            &h.server,
            c,
            None,
            json!([{"ts": ts(times().0, 0), "user": "U1", "text": "x"}]),
            None,
        )
        .await;
    }
    let conn = slack(
        &h,
        json!({"conversations": ["C0123456789", "#random", "missing"]}),
    );
    let sink = h.sink();
    let cursor = run(&conn, None, &sink).await.unwrap();
    let chans: Vec<&String> = cursor["channels"].as_object().unwrap().keys().collect();
    assert_eq!(chans, vec!["C0123456789", "C2"]);
    let docs = sink.upserts();
    assert!(docs.iter().any(|d| d.metadata["channel"] == json!("#eng")
        && d.metadata["channel_type"] == json!("private_channel")));
}

#[tokio::test]
async fn rate_limits_honour_retry_after() {
    let h = Harness::new().await;
    h.paste("xoxp-1");
    mount_auth(&h.server).await;
    mount_convs(&h.server, json!([{"id": "C1", "name": "general"}])).await;
    Mock::given(method("GET"))
        .and(path("/slack/conversations.history"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&h.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slack/conversations.history"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ok": false, "error": "ratelimited"}))
                .insert_header("retry-after", "0"),
        )
        .up_to_n_times(1)
        .with_priority(2)
        .mount(&h.server)
        .await;
    mount_history(
        &h.server,
        "C1",
        None,
        json!([{"ts": ts(times().0, 0), "user": "U1", "text": "x"}]),
        None,
    )
    .await;
    let conn = slack(&h, json!({}));
    let sink = h.sink();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.upserts().len(), 1);

    // A long Retry-After is not slept on: the supervisor waits instead.
    h.server.reset().await;
    mount_auth(&h.server).await;
    mount_convs(&h.server, json!([{"id": "C1", "name": "general"}])).await;
    Mock::given(method("GET"))
        .and(path("/slack/conversations.history"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "120"))
        .expect(1)
        .mount(&h.server)
        .await;
    let err = run(&conn, None, &h.sink()).await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: Some(120_000)
            }
        ),
        "{err}"
    );
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let h = Harness::new().await;
    h.paste("xoxp-1");
    let elsewhere = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ok(json!({})))
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
    let conn = slack(&h, json!({}));
    let err = run(&conn, None, &h.sink()).await.unwrap_err();
    assert!(
        matches!(err, Error::Network(ref m) if m.contains("redirect")),
        "{err}"
    );
    assert_eq!(h.egress.0.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn hostile_payloads_are_capped_not_panics() {
    let h = Harness::new().await;
    h.paste("xoxp-1");
    mount_auth(&h.server).await;
    mount_convs(
        &h.server,
        json!([{"id": "C1", "name": "general"}, {"id": "../x"}, {"id": "C2"}]),
    )
    .await;
    let t = times().0;
    let huge = "A".repeat(1_000_000);
    let many: Vec<Value> = [json!({"ts": ts(t, 5_000), "user": "U1", "text": huge})]
        .into_iter()
        .chain((0..2_100).map(
            |i| json!({"ts": ts(t, i), "user": "U1", "text": format!("m{i} {}", "<".repeat(3))}),
        ))
        .chain([
            json!({"ts": "not-a-ts", "user": "U1", "text": "bad"}),
            json!({"ts": ts(t, 5_001), "user": "U1", "text": "<@U1|<<<<"}),
            json!({"ts": 17, "text": null}),
        ])
        .collect();
    mount_history(&h.server, "C1", None, Value::Array(many), None).await;
    Mock::given(method("GET"))
        .and(path("/slack/conversations.history"))
        .and(query_param("channel", "C2"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{not json"))
        .mount(&h.server)
        .await;
    let conn = slack(&h, json!({}));
    let sink = h.sink();
    let err = run(&conn, None, &sink).await.unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err}");
    let text = sink.text(&format!("C1/day/{}", date(t)));
    assert!(text.len() < 1_000_000, "{}", text.len());
    assert!(text.contains("more messages not shown"));
    assert!(text.contains(&format!("{}…", "A".repeat(100))));
    assert!(!text.contains("bad"));
}

/// An `ok:false` error code is server text that lands in error messages (logs, the UI): it is
/// capped and stripped of control characters.
#[tokio::test]
async fn hostile_error_codes_are_capped_and_printable() {
    let h = Harness::new().await;
    h.paste("xoxp-1");
    let code = format!("\u{1b}[2J\nfake_log_line {}", "x".repeat(100_000));
    Mock::given(method("GET"))
        .and(path("/slack/auth.test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": false, "error": code})))
        .mount(&h.server)
        .await;
    let conn = slack(&h, json!({}));
    let msg = run(&conn, None, &h.sink()).await.unwrap_err().to_string();
    assert!(msg.len() < 400, "{} bytes", msg.len());
    assert!(!msg.chars().any(char::is_control), "{msg:?}");
}
