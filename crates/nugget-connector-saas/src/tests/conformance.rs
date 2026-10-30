//! Conformance suite (FF2): the robustness properties every connector must have, proven for
//! Slack, Notion, GitHub and Linear against a mock server.
//!
//! For each connector: pagination across many pages (nothing dropped or duplicated), 429 / 5xx
//! with `Retry-After` (delta-seconds and HTTP-date) and backoff, the retry budget surfacing the
//! error and a later pass resuming, token expiry / revocation mid-sync (refresh where OAuth
//! exists, else `Unauthorized`), deletions / moves / renames / edits after sync, huge items,
//! unicode and markup, clock skew, partial failure, crash resumption at every send and
//! checkpoint, and egress recorded for every request. Fixes found by the suite carry a
//! `regression_*` test.

use std::collections::BTreeMap;

use nugget_connector::testkit::{
    self, Replica, ReplicaSink, Sequence, UNICODE_SAMPLES, assert_crash_resumable, assert_scrubbed,
    big_text, run_pass, server_error, too_many_requests,
};
use serde_json::json;
use time::OffsetDateTime;
use wiremock::matchers::{
    body_partial_json, body_string_contains, header, method, path, query_param,
    query_param_is_missing,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::text::{DAY_SECS, date, day_start, now_unix, rfc3339};
use crate::{GitHubFactory, LinearFactory, NotionFactory, SlackFactory};

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

/// A connector over `replica` (as its known items), sharing the harness' egress and secrets.
fn build_over(
    h: &Harness,
    factory: &dyn ConnectorFactory,
    settings: &Value,
    replica: Arc<Replica>,
) -> Arc<dyn Connector> {
    let ctx = testkit::context(
        SOURCE,
        Paths::rooted(std::env::temp_dir().join("nugget-saas-conformance")),
        h.egress.clone(),
        h.secrets.clone(),
        replica,
    );
    factory.build(settings, ctx).unwrap()
}

/// Crash a pass at every send and checkpoint and require the same index as a clean run.
async fn crash_matrix(
    h: &Harness,
    factory: &dyn ConnectorFactory,
    settings: &Value,
    start: Option<Value>,
    seed: BTreeMap<ItemKey, testkit::ReplicaItem>,
) -> testkit::CrashReport {
    assert_crash_resumable(
        |r| {
            let c = build_over(h, factory, settings, r);
            async move { c }
        },
        start,
        seed,
    )
    .await
}

/// An IMF-fixdate (`Wed, 21 Oct 2026 07:28:00 GMT`).
fn http_date(unix: i64) -> String {
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let t = OffsetDateTime::from_unix_timestamp(unix).unwrap();
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        DAYS[t.weekday().number_days_from_monday() as usize],
        t.day(),
        MONTHS[usize::from(u8::from(t.month())) - 1],
        t.year(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// A `Date` far from the local clock (2020), so only a server-relative reading is right.
const SKEWED: i64 = 1_600_000_000;

/// `429` whose `Retry-After` is an HTTP-date 30 s after the response's own (skewed) `Date`.
fn throttled_until_date() -> ResponseTemplate {
    ResponseTemplate::new(429)
        .insert_header("Date", http_date(SKEWED).as_str())
        .insert_header("Retry-After", http_date(SKEWED + 30).as_str())
}

/// `429` with an HTTP-date `Retry-After` equal to its `Date`: retry now.
fn throttled_now_by_date() -> ResponseTemplate {
    ResponseTemplate::new(429)
        .insert_header("Date", http_date(SKEWED).as_str())
        .insert_header("Retry-After", http_date(SKEWED).as_str())
}

fn ago(secs: i64) -> String {
    rfc3339(now_unix() - secs).unwrap()
}

/// `secs` before the start of today (UTC): the same value when fixtures are mounted again a
/// moment later, so re-mounted mocks describe unchanged items.
fn stable_ago(secs: i64) -> String {
    rfc3339(day_start(now_unix()) - secs).unwrap()
}

fn text_of(d: &Document) -> String {
    match &d.content {
        nugget_connector::Content::Text(t) => t.clone(),
        other => panic!("not text: {other:?}"),
    }
}

/// Each key upserted at most once, and the upserted keys as a sorted list.
fn unique_upserts(sink: &RecSink) -> Vec<String> {
    let mut keys: Vec<String> = sink.upserts().iter().map(|d| d.key.to_string()).collect();
    let n = keys.len();
    keys.sort();
    keys.dedup();
    assert_eq!(
        keys.len(),
        n,
        "a key was upserted twice: {:?}",
        sink.summary()
    );
    keys
}

fn rate_limited_ms<T: std::fmt::Debug>(r: &Result<T>) -> Option<u64> {
    match r {
        Err(Error::RateLimited { retry_after_ms }) => *retry_after_ms,
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

// Recorded responses (scrubbed): shapes taken from the real APIs, ids and addresses replaced.
const SLACK_AUTH_TEST: &str = r#"{"ok":true,"url":"https://acme.slack.com/","team":"Acme","user":"ada","team_id":"T0000001","user_id":"U1"}"#;
const SLACK_USER: &str = r#"{"ok":true,"user":{"id":"U2","name":"bob","real_name":"Bob","profile":{"display_name":"Bob","email":"user@example.com"}}}"#;
const NOTION_ME: &str = r#"{"object":"user","id":"bot","type":"bot","bot":{"owner":{"type":"user","user":{"object":"user","id":"11111111-1111-1111-1111-111111111111","name":"Ada","person":{"email":"user@example.com"}}},"workspace_name":"Acme"}}"#;
const GITHUB_VIEWER: &str = r#"{"data":{"viewer":{"login":"ada"}}}"#;
const LINEAR_VIEWER: &str = r#"{"data":{"viewer":{"id":"me","name":"Ada Lovelace","displayName":"ada","email":"user@example.com"}}}"#;

#[test]
fn recorded_fixtures_are_scrubbed() {
    for f in [
        SLACK_AUTH_TEST,
        SLACK_USER,
        NOTION_ME,
        GITHUB_VIEWER,
        LINEAR_VIEWER,
    ] {
        assert_scrubbed(f);
    }
    // A raw recording with a token and a real address is caught, and scrubbing fixes it.
    let raw = r#"{"ok":true,"access_token":"abc","user":{"profile":{"email":"ada@corp.io"}}}"#;
    assert!(std::panic::catch_unwind(|| assert_scrubbed(raw)).is_err());
    let mut v: Value = serde_json::from_str(raw).unwrap();
    testkit::scrub_json(&mut v);
    assert_scrubbed(&v.to_string());
}

fn fixture(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

// ---------------------------------------------------------------------------------------------
// Slack
// ---------------------------------------------------------------------------------------------

mod slack {
    use super::*;

    pub(super) fn factory(h: &Harness, retries: u32) -> SlackFactory {
        SlackFactory::new()
            .with_endpoints(h.endpoints())
            .with_retry(fast_retry(retries))
    }

    pub(super) fn ok(body: Value) -> ResponseTemplate {
        let mut b = body;
        b["ok"] = json!(true);
        ResponseTemplate::new(200).set_body_json(b)
    }

    pub(super) async fn mount_base(server: &MockServer, chans: Value) {
        Mock::given(method("GET"))
            .and(path("/slack/auth.test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(SLACK_AUTH_TEST)))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/slack/users.info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(SLACK_USER)))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/slack/users.conversations"))
            .respond_with(ok(
                json!({"channels": chans, "response_metadata": {"next_cursor": ""}}),
            ))
            .mount(server)
            .await; // "Anything newer?" probes: nothing, unless a test mounts its own.
        Mock::given(method("GET"))
            .and(path("/slack/conversations.history"))
            .and(query_param("limit", "1"))
            .respond_with(ok(json!({"messages": [], "has_more": false})))
            .with_priority(10)
            .mount(server)
            .await;
    }

    /// A range read (`limit=200`) of `chan`, page `cursor` (none: the first page).
    pub(super) async fn mount_history(
        server: &MockServer,
        chan: &str,
        cursor: Option<&str>,
        resp: ResponseTemplate,
    ) {
        let m = Mock::given(method("GET"))
            .and(path("/slack/conversations.history"))
            .and(query_param("channel", chan))
            .and(query_param("limit", "200"));
        let m = match cursor {
            Some(c) => m.and(query_param("cursor", c)),
            None => m.and(query_param_is_missing("cursor")),
        };
        m.respond_with(resp).mount(server).await;
    }

    pub(super) fn page(msgs: Value, next: Option<&str>) -> ResponseTemplate {
        ok(json!({
            "messages": msgs, "has_more": next.is_some(),
            "response_metadata": {"next_cursor": next.unwrap_or("")}
        }))
    }

    pub(super) async fn mount_probe(server: &MockServer, chan: &str, msgs: Value) {
        Mock::given(method("GET"))
            .and(path("/slack/conversations.history"))
            .and(query_param("channel", chan))
            .and(query_param("limit", "1"))
            .respond_with(ok(json!({"messages": msgs, "has_more": false})))
            .mount(server)
            .await;
    }

    /// A ts `days` days before today, `n` minutes after 01:00 UTC.
    pub(super) fn ts(days: i64, n: i64) -> String {
        let secs = day_start(now_unix()) - days * DAY_SECS + 3600 + n * 60;
        format!("{secs}.{:06}", n)
    }

    pub(super) fn day_key(chan: &str, days: i64) -> String {
        format!(
            "{chan}/day/{}",
            date(day_start(now_unix()) - days * DAY_SECS + 3600)
        )
    }

    pub(super) fn msg(ts: &str, text: &str) -> Value {
        json!({"ts": ts, "user": "U2", "text": text})
    }

    /// Channel C1 with 6 history pages, page `k` holding two messages `k + 1` days ago.
    pub(super) async fn mount_six_pages(server: &MockServer) {
        for k in 0..6i64 {
            let cursor = (k > 0).then(|| format!("h{k}"));
            let next = (k < 5).then(|| format!("h{}", k + 1));
            mount_history(
                server,
                "C1",
                cursor.as_deref(),
                page(
                    json!([
                        msg(&ts(k + 1, 2), &format!("c1-d{k}-b")),
                        msg(&ts(k + 1, 1), &format!("c1-d{k}-a")),
                    ]),
                    next.as_deref(),
                ),
            )
            .await;
        }
    }

    #[tokio::test]
    async fn pagination_across_conversations_and_history_pages() {
        let h = Harness::new().await;
        h.paste("xoxp-1");
        // users.conversations over 5 pages (one channel each).
        Mock::given(method("GET"))
            .and(path("/slack/auth.test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(SLACK_AUTH_TEST)))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/slack/users.info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(SLACK_USER)))
            .mount(&h.server)
            .await;
        for k in 1..=5 {
            let m = Mock::given(method("GET")).and(path("/slack/users.conversations"));
            let m = if k == 1 {
                m.and(query_param_is_missing("cursor"))
            } else {
                m.and(query_param("cursor", format!("cv{k}").as_str()))
            };
            let next = if k < 5 {
                format!("cv{}", k + 1)
            } else {
                String::new()
            };
            m.respond_with(ok(json!({
                "channels": [{"id": format!("C{k}"), "name": format!("chan{k}")}],
                "response_metadata": {"next_cursor": next}
            })))
            .mount(&h.server)
            .await;
        }
        mount_six_pages(&h.server).await;
        for k in 2..=5 {
            mount_history(&h.server, &format!("C{k}"), None, page(json!([]), None)).await;
        }

        let conn = h.build(&factory(&h, 2), json!({"window_days": 30}));
        let sink = h.sink();
        run(&conn, None, &sink).await.unwrap();
        let mut want: Vec<String> = (1..=6).map(|d| day_key("C1", d)).collect();
        want.sort();
        assert_eq!(unique_upserts(&sink), want);
        let all: String = sink.upserts().iter().map(text_of).collect();
        for k in 0..6 {
            for s in ["a", "b"] {
                assert_eq!(all.matches(&format!("c1-d{k}-{s}")).count(), 1, "{all}");
            }
        }
        h.assert_egress("sync:slack").await;
    }

    #[tokio::test]
    async fn throttling_and_server_errors_are_retried() {
        let h = Harness::new().await;
        h.paste("xoxp-1");
        Mock::given(method("GET"))
            .and(path("/slack/auth.test"))
            .respond_with(Sequence::new(vec![
                throttled_now_by_date(),
                ResponseTemplate::new(200).set_body_json(fixture(SLACK_AUTH_TEST)),
            ]))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/slack/users.info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(SLACK_USER)))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/slack/users.conversations"))
            .respond_with(ok(json!({"channels": [{"id": "C1", "name": "general"}]})))
            .mount(&h.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/slack/conversations.history"))
            .and(query_param("limit", "200"))
            .respond_with(Sequence::new(vec![
                too_many_requests(0),
                server_error(503, Some(0)),
                // `ratelimited` inside a 200, as Slack also does.
                ResponseTemplate::new(200)
                    .insert_header("Retry-After", "0")
                    .set_body_json(json!({"ok": false, "error": "ratelimited"})),
                page(json!([msg(&ts(1, 0), "after the storm")]), None),
            ]))
            .mount(&h.server)
            .await;
        let conn = h.build(&factory(&h, 4), json!({"window_days": 7}));
        let sink = h.sink();
        run(&conn, None, &sink).await.unwrap();
        assert_eq!(unique_upserts(&sink), vec![day_key("C1", 1)]);
        h.assert_egress("sync:slack").await;
    }

    #[tokio::test]
    async fn retry_budget_surfaces_the_error_and_a_later_pass_resumes() {
        let h = Harness::new().await;
        h.paste("xoxp-1");
        let chans = json!([{"id": "C1", "name": "general"}]);
        mount_base(&h.server, chans.clone()).await;
        // Page 1 finishes day 1 (checkpointed); page 2 keeps failing.
        mount_history(
            &h.server,
            "C1",
            None,
            page(
                json!([msg(&ts(1, 0), "one"), msg(&ts(2, 0), "two")]),
                Some("h1"),
            ),
        )
        .await;
        mount_history(&h.server, "C1", Some("h1"), server_error(503, None)).await;
        let settings = json!({"window_days": 7});
        let replica = Replica::new();
        let sink = ReplicaSink::new(replica.clone());
        let conn = build_over(&h, &factory(&h, 2), &settings, replica.clone());
        let r = run_pass(&*conn, None, &sink).await;
        assert!(matches!(r, Err(Error::Network(_))), "{r:?}");
        let committed = sink.committed().expect("progress was checkpointed");
        assert_eq!(replica.keys(), vec![day_key("C1", 1)]);

        h.server.reset().await;
        mount_base(&h.server, chans).await;
        mount_history(
            &h.server,
            "C1",
            None,
            page(
                json!([msg(&ts(1, 0), "one"), msg(&ts(2, 0), "two")]),
                Some("h1"),
            ),
        )
        .await;
        mount_history(
            &h.server,
            "C1",
            Some("h1"),
            page(json!([msg(&ts(3, 0), "three")]), None),
        )
        .await;
        let resume = ReplicaSink::new(replica.clone());
        let conn = build_over(&h, &factory(&h, 2), &settings, replica.clone());
        run_pass(&*conn, Some(committed), &resume).await.unwrap();
        let mut want = vec![day_key("C1", 1), day_key("C1", 2), day_key("C1", 3)];
        want.sort();
        assert_eq!(replica.keys(), want);
    }

    #[tokio::test]
    async fn revoked_token_mid_sync_is_unauthorized() {
        for (route, revoke) in [
            (
                "history",
                ResponseTemplate::new(200)
                    .set_body_json(json!({"ok": false, "error": "token_revoked"})),
            ),
            (
                "history",
                ResponseTemplate::new(401).set_body_json(json!({"ok": false})),
            ),
            (
                "replies",
                ResponseTemplate::new(200)
                    .set_body_json(json!({"ok": false, "error": "invalid_auth"})),
            ),
        ] {
            let h = Harness::new().await;
            h.paste("xoxp-1");
            mount_base(&h.server, json!([{"id": "C1", "name": "general"}])).await;
            let parent = ts(1, 0);
            mount_history(
                &h.server,
                "C1",
                None,
                page(
                    json!([{"ts": parent, "user": "U2", "text": "thread", "thread_ts": parent,
                            "reply_count": 1, "latest_reply": ts(1, 1)},
                           msg(&ts(2, 0), "older")]),
                    Some("h1"),
                ),
            )
            .await;
            if route == "history" {
                mount_history(&h.server, "C1", Some("h1"), revoke).await;
                Mock::given(method("GET"))
                    .and(path("/slack/conversations.replies"))
                    .respond_with(ok(json!({"messages": [], "has_more": false})))
                    .mount(&h.server)
                    .await;
            } else {
                mount_history(&h.server, "C1", Some("h1"), page(json!([]), None)).await;
                Mock::given(method("GET"))
                    .and(path("/slack/conversations.replies"))
                    .respond_with(revoke)
                    .mount(&h.server)
                    .await;
            }
            let conn = h.build(&factory(&h, 2), json!({"window_days": 7}));
            let r = run(&conn, None, &h.sink()).await;
            assert!(is_unauthorized(&r), "{route}: {r:?}");
        }
    }

    #[tokio::test]
    async fn edits_deletions_and_renames_after_sync() {
        let h = Harness::new().await;
        h.paste("xoxp-1");
        mount_base(&h.server, json!([{"id": "C1", "name": "general"}])).await;
        let a = ts(1, 0);
        mount_history(
            &h.server,
            "C1",
            None,
            page(
                json!([msg(&a, "original text"), msg(&ts(2, 0), "to be deleted")]),
                None,
            ),
        )
        .await;
        let settings = json!({"window_days": 30, "recheck_days": 3});
        let conn = h.build(&factory(&h, 2), settings.clone());
        let mut cursor = run(&conn, None, &h.sink()).await.unwrap();
        assert_eq!(h.known_keys().len(), 2);

        h.server.reset().await;
        mount_base(&h.server, json!([{"id": "C1", "name": "announcements"}])).await;
        mount_probe(&h.server, "C1", json!([])).await;
        mount_history(
            &h.server,
            "C1",
            None,
            page(
                json!([{"ts": a, "user": "U2", "text": "edited text", "edited": {"ts": ts(0, 0)}}]),
                None,
            ),
        )
        .await;
        // The re-check is due.
        cursor["channels"]["C1"]["checked_at"] = json!(0);
        let sink = h.sink();
        run(&conn, Some(cursor), &sink).await.unwrap();
        assert_eq!(
            sink.summary(),
            vec![
                format!("+{}", day_key("C1", 1)),
                format!("-{}", day_key("C1", 2))
            ]
        );
        let doc = sink.doc(&day_key("C1", 1));
        let text = text_of(&doc);
        assert!(text.contains("edited text (edited)"), "{text}");
        assert!(!text.contains("original text"));
        assert!(doc.title.unwrap().starts_with("#announcements"));
        assert_eq!(h.known_keys(), vec![day_key("C1", 1)]);
    }

    #[tokio::test]
    async fn huge_days_and_messages_are_capped_and_unicode_survives() {
        let h = Harness::new().await;
        h.paste("xoxp-1");
        mount_base(&h.server, json!([{"id": "C1", "name": "general"}])).await;
        // Day 1: one 100 kB message and 2100 more messages (over the per-document cap).
        let base = day_start(now_unix()) - DAY_SECS + 7200;
        let mut day1 = vec![msg(&format!("{}.000000", base + 600), &big_text(100_000))];
        for i in 0..2100 {
            day1.push(msg(&format!("{base}.{i:06}"), &format!("m{i}")));
        }
        // Day 2: unicode samples and mrkdwn that must render as plain text.
        let mut day2: Vec<Value> = UNICODE_SAMPLES
            .iter()
            .enumerate()
            .map(|(i, s)| msg(&ts(2, i as i64 + 1), s))
            .collect();
        day2.push(msg(
            &ts(2, 30),
            "*bold* _it_ ~gone~ `code` <https://example.com/x|a link> &amp; &lt;tag&gt; <@U2> <!here>",
        ));
        day2.reverse();
        mount_history(&h.server, "C1", None, page(json!(day1), Some("h1"))).await;
        mount_history(&h.server, "C1", Some("h1"), page(json!(day2), None)).await;
        let conn = h.build(&factory(&h, 2), json!({"window_days": 7}));
        let sink = h.sink();
        run(&conn, None, &sink).await.unwrap();

        let big = sink.text(&day_key("C1", 1));
        assert!(big.contains("more messages not shown"), "cap note missing");
        assert!(big.contains(&format!("{}…", big_text(20_000))));
        assert!(!big.contains(&big_text(20_001)));
        assert!(big.chars().count() < 500_000);

        let uni = sink.text(&day_key("C1", 2));
        for s in UNICODE_SAMPLES {
            assert!(uni.contains(s), "{s:?} lost in {uni}");
        }
        assert!(
            uni.contains("bold it gone code a link (https://example.com/x) & <tag> @Bob @here")
        );
        for junk in ["*bold*", "<https", "&amp;", "&lt;", "<@U2>", "<!here>"] {
            assert!(!uni.contains(junk), "{junk} leaked: {uni}");
        }
    }

    /// Slack stamps messages with its own clock. With ours behind, a range read that ended at
    /// our `now` left out messages "from the future" until our clock caught up.
    #[tokio::test]
    async fn regression_slack_message_ahead_of_local_clock_is_indexed() {
        let h = Harness::new().await;
        h.paste("xoxp-1");
        mount_base(&h.server, json!([{"id": "C1", "name": "general"}])).await;
        let ahead = now_unix() + 600;
        mount_history(
            &h.server,
            "C1",
            None,
            page(
                json!([msg(
                    &format!("{ahead}.000100"),
                    "sent by a clock ahead of ours"
                )]),
                None,
            ),
        )
        .await;
        let conn = h.build(&factory(&h, 2), json!({"window_days": 7}));
        let sink = h.sink();
        let cursor = run(&conn, None, &sink).await.unwrap();
        let key = format!("C1/day/{}", date(ahead));
        assert!(sink.text(&key).contains("sent by a clock ahead of ours"));
        assert_eq!(
            cursor["channels"]["C1"]["latest"],
            json!(format!("{ahead}.000100"))
        );
    }

    /// One thread whose replies cannot be read (HTTP 500 after retries) used to fail the
    /// whole pass; the day, the other conversations and later passes must go on.
    #[tokio::test]
    async fn regression_slack_thread_that_fails_to_load_does_not_stop_the_sync() {
        let h = Harness::new().await;
        h.paste("xoxp-1");
        mount_base(
            &h.server,
            json!([{"id": "C1", "name": "general"}, {"id": "C2", "name": "random"}]),
        )
        .await;
        let parent = ts(1, 5);
        mount_history(
            &h.server,
            "C1",
            None,
            page(
                json!([
                    {"ts": parent, "user": "U2", "text": "broken thread", "thread_ts": parent,
                     "reply_count": 3, "latest_reply": ts(1, 9)},
                    {"ts": 17, "text": "numeric ts"},
                    "not even an object",
                    {"ts": "x.y", "text": "bad ts"},
                    {"ts": ts(1, 3), "user": {"nested": true}, "text": "bad user field"},
                    msg(&ts(1, 1), "fine message"),
                ]),
                None,
            ),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/slack/conversations.replies"))
            .respond_with(server_error(500, None))
            .mount(&h.server)
            .await;
        mount_history(
            &h.server,
            "C2",
            None,
            page(json!([msg(&ts(1, 0), "other channel")]), None),
        )
        .await;
        let conn = h.build(&factory(&h, 1), json!({"window_days": 7}));
        let sink = h.sink();
        run(&conn, None, &sink).await.unwrap();
        let mut want = vec![day_key("C1", 1), day_key("C2", 1)];
        want.sort();
        assert_eq!(unique_upserts(&sink), want);
        let c1 = sink.text(&day_key("C1", 1));
        assert!(
            c1.contains("broken thread") && c1.contains("fine message"),
            "{c1}"
        );
        assert!(sink.text(&day_key("C2", 1)).contains("other channel"));
    }

    #[tokio::test]
    async fn crash_at_any_point_resumes_backfill_and_incremental() {
        let h = Harness::new().await;
        h.paste("xoxp-1");
        mount_base(&h.server, json!([{"id": "C1", "name": "general"}])).await;
        mount_six_pages(&h.server).await;
        let settings = json!({"window_days": 30});
        let f = factory(&h, 1);
        // A stale day outside the window is dropped by the first pass.
        let seed = Replica::new();
        seed.apply(&Change::Upsert(Document::new(
            "C1/day/2001-01-01",
            "text/plain",
            nugget_connector::Content::Text("old".into()),
        )));
        let report = crash_matrix(&h, &f, &settings, None, seed.snapshot()).await;
        assert!(report.sends >= 7 && report.checkpoints >= 6, "{report:?}");

        // The state after a clean backfill, then an incremental pass with a new message.
        let replica = Replica::new();
        replica.restore(seed.snapshot());
        let sink = ReplicaSink::new(replica.clone());
        let conn = build_over(&h, &f, &settings, replica.clone());
        let cursor = run_pass(&*conn, None, &sink).await.unwrap();
        assert_eq!(replica.len(), 6);

        h.server.reset().await;
        mount_base(&h.server, json!([{"id": "C1", "name": "general"}])).await;
        let new = format!("{}.000001", now_unix() - 5);
        mount_probe(&h.server, "C1", json!([msg(&new, "new")])).await;
        mount_history(
            &h.server,
            "C1",
            None,
            page(
                json!([
                    msg(&new, "brand new"),
                    msg(&ts(1, 2), "c1-d0-b"),
                    msg(&ts(1, 1), "c1-d0-a")
                ]),
                None,
            ),
        )
        .await;
        let report = crash_matrix(&h, &f, &settings, Some(cursor), replica.snapshot()).await;
        assert!(report.sends >= 2, "{report:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// Notion
// ---------------------------------------------------------------------------------------------

mod notion {
    use super::*;

    pub(super) fn factory(h: &Harness, retries: u32) -> NotionFactory {
        NotionFactory::new()
            .with_endpoints(h.endpoints())
            .with_retry(fast_retry(retries))
    }

    pub(super) fn id(n: u32) -> String {
        format!("aaaaaaaa-0000-0000-0000-{n:012}")
    }

    pub(super) async fn mount_me(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/notion/v1/users/me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(NOTION_ME)))
            .mount(server)
            .await;
    }

    pub(super) fn search_page(results: Value, next: Option<&str>) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "object": "list", "results": results, "has_more": next.is_some(), "next_cursor": next
        }))
    }

    pub(super) async fn mount_search(
        server: &MockServer,
        cursor: Option<&str>,
        resp: ResponseTemplate,
    ) {
        let m = Mock::given(method("POST")).and(path("/notion/v1/search"));
        match cursor {
            Some(c) => {
                m.and(body_partial_json(json!({"start_cursor": c})))
                    .respond_with(resp)
                    .with_priority(2)
                    .mount(server)
                    .await
            }
            None => m.respond_with(resp).with_priority(5).mount(server).await,
        }
    }

    pub(super) async fn mount_children(
        server: &MockServer,
        block: &str,
        cursor: Option<&str>,
        blocks: Value,
        next: Option<&str>,
    ) {
        let m = Mock::given(method("GET")).and(path(format!("/notion/v1/blocks/{block}/children")));
        let m = match cursor {
            Some(c) => m.and(query_param("start_cursor", c)),
            None => m.and(query_param_is_missing("start_cursor")),
        };
        m.respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list", "results": blocks, "has_more": next.is_some(), "next_cursor": next
        })))
        .mount(server)
        .await;
    }

    pub(super) fn page(id: &str, edited: &str, title: &str) -> Value {
        json!({
            "object": "page", "id": id, "last_edited_time": edited, "created_time": edited,
            "created_by": {"object": "user", "id": "u1", "name": "Bob"},
            "last_edited_by": {"object": "user", "id": "u1", "name": "Bob"},
            "archived": false, "in_trash": false, "url": format!("https://www.notion.so/{id}"),
            "parent": {"type": "workspace", "workspace": true},
            "properties": {"title": {"id": "title", "type": "title", "title": [{"plain_text": title}]}}
        })
    }

    pub(super) fn para(id: &str, text: &str) -> Value {
        json!({"object": "block", "id": id, "type": "paragraph", "has_children": false,
               "paragraph": {"rich_text": [{"plain_text": text}]}})
    }

    /// Six search pages of one page each (newest first); page 0's blocks span five pages.
    pub(super) async fn mount_six(server: &MockServer) {
        for k in 0..6u32 {
            let cursor = (k > 0).then(|| format!("s{k}"));
            let next = (k < 5).then(|| format!("s{}", k + 1));
            mount_search(
                server,
                cursor.as_deref(),
                search_page(
                    json!([page(
                        &id(k),
                        &stable_ago(3600 * i64::from(k + 1)),
                        &format!("Page {k}")
                    )]),
                    next.as_deref(),
                ),
            )
            .await;
            if k > 0 {
                mount_children(
                    server,
                    &id(k),
                    None,
                    json!([para(&format!("b{k}"), &format!("body {k}"))]),
                    None,
                )
                .await;
            }
        }
        for b in 0..5u32 {
            let cursor = (b > 0).then(|| format!("c{b}"));
            let next = (b < 4).then(|| format!("c{}", b + 1));
            mount_children(
                server,
                &id(0),
                cursor.as_deref(),
                json!([para(&format!("p0b{b}"), &format!("block {b}"))]),
                next.as_deref(),
            )
            .await;
        }
    }

    #[tokio::test]
    async fn pagination_of_search_and_block_children() {
        let h = Harness::new().await;
        h.paste("ntn_1");
        mount_me(&h.server).await;
        mount_six(&h.server).await;
        let conn = h.build(&factory(&h, 2), json!({}));
        let sink = h.sink();
        run(&conn, None, &sink).await.unwrap();
        let mut want: Vec<String> = (0..6).map(|k| format!("page/{}", id(k))).collect();
        want.sort();
        assert_eq!(unique_upserts(&sink), want);
        let p0 = sink.text(&format!("page/{}", id(0)));
        let mut at = 0;
        for b in 0..5 {
            let needle = format!("block {b}");
            assert_eq!(p0.matches(&needle).count(), 1, "{p0}");
            let i = p0.find(&needle).unwrap();
            assert!(i > at, "blocks out of order: {p0}");
            at = i;
        }
        h.assert_egress("sync:notion").await;
    }

    #[tokio::test]
    async fn throttling_and_server_errors_are_retried() {
        let h = Harness::new().await;
        h.paste("ntn_1");
        Mock::given(method("GET"))
            .and(path("/notion/v1/users/me"))
            .respond_with(Sequence::new(vec![
                throttled_now_by_date(),
                ResponseTemplate::new(200).set_body_json(fixture(NOTION_ME)),
            ]))
            .mount(&h.server)
            .await;
        Mock::given(method("POST"))
            .and(path("/notion/v1/search"))
            .respond_with(Sequence::new(vec![
                too_many_requests(0),
                server_error(502, Some(0)),
                server_error(503, None),
                search_page(json!([page(&id(1), &ago(7200), "Calm")]), None),
            ]))
            .mount(&h.server)
            .await;
        mount_children(&h.server, &id(1), None, json!([para("b", "fine")]), None).await;
        let conn = h.build(&factory(&h, 4), json!({}));
        let sink = h.sink();
        run(&conn, None, &sink).await.unwrap();
        assert_eq!(unique_upserts(&sink), vec![format!("page/{}", id(1))]);
        h.assert_egress("sync:notion").await;
    }

    #[tokio::test]
    async fn retry_budget_surfaces_the_error_and_a_later_pass_resumes() {
        let h = Harness::new().await;
        h.paste("ntn_1");
        mount_me(&h.server).await;
        mount_six(&h.server).await;
        // Search page 3 keeps failing.
        Mock::given(method("POST"))
            .and(path("/notion/v1/search"))
            .and(body_partial_json(json!({"start_cursor": "s3"})))
            .respond_with(server_error(503, None))
            .with_priority(1)
            .mount(&h.server)
            .await;
        let settings = json!({});
        let replica = Replica::new();
        let sink = ReplicaSink::new(replica.clone());
        let conn = build_over(&h, &factory(&h, 2), &settings, replica.clone());
        let r = run_pass(&*conn, None, &sink).await;
        assert!(matches!(r, Err(Error::Network(_))), "{r:?}");
        // Pages read before the failure are kept (and skipped cheaply next time).
        assert_eq!(replica.len(), 3);

        h.server.reset().await;
        mount_me(&h.server).await;
        mount_six(&h.server).await;
        let conn = build_over(&h, &factory(&h, 2), &settings, replica.clone());
        let resume = ReplicaSink::new(replica.clone());
        run_pass(&*conn, sink.committed(), &resume).await.unwrap();
        assert_eq!(replica.len(), 6);
        // Already-indexed pages were not re-sent.
        assert_eq!(resume.summary().len(), 3, "{:?}", resume.summary());
    }

    #[tokio::test]
    async fn revoked_token_mid_sync_is_unauthorized() {
        for status in [401u16, 403] {
            let h = Harness::new().await;
            h.paste("ntn_1");
            mount_me(&h.server).await;
            mount_search(
                &h.server,
                None,
                search_page(json!([page(&id(1), &ago(7200), "A")]), Some("s1")),
            )
            .await;
            mount_children(&h.server, &id(1), None, json!([para("b", "x")]), None).await;
            mount_search(
                &h.server,
                Some("s1"),
                ResponseTemplate::new(status).set_body_json(json!({"code": "unauthorized"})),
            )
            .await;
            let conn = h.build(&factory(&h, 2), json!({}));
            let r = run(&conn, None, &h.sink()).await;
            assert!(is_unauthorized(&r), "{status}: {r:?}");
        }
        // A 401 on one page's blocks is not "one bad item": it stops the pass.
        let h = Harness::new().await;
        h.paste("ntn_1");
        mount_me(&h.server).await;
        mount_search(
            &h.server,
            None,
            search_page(json!([page(&id(1), &ago(7200), "A")]), None),
        )
        .await;
        Mock::given(method("GET"))
            .and(path(format!("/notion/v1/blocks/{}/children", id(1))))
            .respond_with(ResponseTemplate::new(401))
            .mount(&h.server)
            .await;
        let conn = h.build(&factory(&h, 2), json!({}));
        let r = run(&conn, None, &h.sink()).await;
        assert!(is_unauthorized(&r), "{r:?}");
    }

    #[tokio::test]
    async fn moves_renames_deletions_and_edits_after_sync() {
        let h = Harness::new().await;
        h.paste("ntn_1");
        mount_me(&h.server).await;
        let (a, b, c) = (id(1), id(2), id(3));
        mount_search(
            &h.server,
            None,
            search_page(
                json!([
                    page(&a, &ago(7200), "Old title"),
                    page(&b, &ago(7300), "Doomed")
                ]),
                None,
            ),
        )
        .await;
        mount_children(&h.server, &a, None, json!([para("x", "first body")]), None).await;
        mount_children(&h.server, &b, None, json!([para("y", "bye")]), None).await;
        let conn = h.build(&factory(&h, 2), json!({}));
        let cursor = run(&conn, None, &h.sink()).await.unwrap();

        h.server.reset().await;
        mount_me(&h.server).await;
        let mut moved = page(&a, &ago(900), "New title");
        moved["parent"] = json!({"type": "page_id", "page_id": id(9)});
        let mut gone = page(&b, &ago(1000), "Doomed");
        gone["archived"] = json!(true);
        mount_search(
            &h.server,
            None,
            search_page(json!([page(&c, &ago(600), "Fresh"), moved, gone]), None),
        )
        .await;
        mount_children(&h.server, &a, None, json!([para("x", "edited body")]), None).await;
        mount_children(&h.server, &c, None, json!([para("z", "hello")]), None).await;
        let sink = h.sink();
        run(&conn, Some(cursor), &sink).await.unwrap();
        assert_eq!(
            sink.summary(),
            vec![
                format!("+page/{c}"),
                format!("+page/{a}"),
                format!("-page/{b}")
            ]
        );
        let d = sink.doc(&format!("page/{a}"));
        assert_eq!(d.title.as_deref(), Some("New title"));
        assert_eq!(d.metadata["parent_type"], json!("page_id"));
        let t = text_of(&d);
        assert!(
            t.starts_with("# New title") && t.contains("edited body") && !t.contains("first body")
        );
    }

    #[tokio::test]
    async fn huge_pages_are_capped_and_rich_text_and_unicode_render_cleanly() {
        let h = Harness::new().await;
        h.paste("ntn_1");
        mount_me(&h.server).await;
        let (big, uni) = (id(1), id(2));
        let mut title = page(&uni, &ago(7200), "");
        title["properties"]["title"]["title"] = json!(
            UNICODE_SAMPLES
                .iter()
                .map(|s| json!({"plain_text": s}))
                .collect::<Vec<_>>()
        );
        mount_search(
            &h.server,
            None,
            search_page(json!([page(&big, &ago(3600), "Big"), title]), None),
        )
        .await;
        // 3 pages x 100 blocks x 2 kB, plus one 60 kB block: over every cap.
        let mut first = vec![para("huge", &big_text(60_000))];
        for k in 0..3 {
            let blocks: Vec<Value> = (0..100)
                .map(|i| para(&format!("b{k}-{i}"), &big_text(2_000)))
                .collect();
            let cursor = (k > 0).then(|| format!("c{k}"));
            let next = (k < 2).then(|| format!("c{}", k + 1));
            if k == 0 {
                first.extend(blocks);
                mount_children(&h.server, &big, None, json!(first), next.as_deref()).await;
            } else {
                mount_children(
                    &h.server,
                    &big,
                    cursor.as_deref(),
                    json!(blocks),
                    next.as_deref(),
                )
                .await;
            }
        }
        let mut blocks: Vec<Value> = UNICODE_SAMPLES
            .iter()
            .enumerate()
            .map(|(i, s)| para(&format!("u{i}"), s))
            .collect();
        blocks.push(json!({"object": "block", "id": "rt", "type": "paragraph", "has_children": false,
            "paragraph": {"rich_text": [
                {"type": "text", "plain_text": "bold", "annotations": {"bold": true}},
                {"type": "text", "plain_text": " and "},
                {"type": "text", "plain_text": "link", "href": "https://example.com/l"},
                {"type": "mention", "plain_text": " @Ada", "mention": {"type": "user", "user": {"id": "u"}}},
                {"type": "equation", "plain_text": " x²", "equation": {"expression": "x^2"}},
                {"type": "text", "text": {"content": "no plain_text"}}
            ]}}));
        blocks.push(
            json!({"object": "block", "id": "weird", "type": "unsupported", "has_children": false,
            "unsupported": {}}),
        );
        mount_children(&h.server, &uni, None, json!(blocks), None).await;
        let conn = h.build(&factory(&h, 2), json!({}));
        let sink = h.sink();
        run(&conn, None, &sink).await.unwrap();

        let b = sink.text(&format!("page/{big}"));
        assert!(b.chars().count() <= crate::notion::MAX_DOC_CHARS + 1);
        assert!(b.contains(&format!("{}…", big_text(50_000))));
        assert!(!b.contains(&big_text(50_001)));

        let d = sink.doc(&format!("page/{uni}"));
        let u = text_of(&d);
        for s in UNICODE_SAMPLES {
            assert!(u.contains(s), "{s:?} lost in {u}");
        }
        assert!(d.title.unwrap().contains("日本語のタイトル"));
        assert!(
            u.contains("**bold** and [link](https://example.com/l) @Ada x²"),
            "{u}"
        );
        for junk in ["plain_text", "{\"", "null", "undefined", "unsupported"] {
            assert!(!u.contains(junk), "{junk} leaked: {u}");
        }
    }

    /// The incremental listing re-reads a margin before `since` (Notion rounds edit times), so
    /// a page edited just before the watermark is not missed.
    #[tokio::test]
    async fn incremental_watermark_overlaps() {
        let h = Harness::new().await;
        h.paste("ntn_1");
        mount_me(&h.server).await;
        let since = now_unix() - 3600;
        mount_search(
            &h.server,
            None,
            search_page(
                json!([
                    page(&id(1), &rfc3339(since - 60).unwrap(), "Edited at the edge"),
                    page(&id(2), &rfc3339(since - 900).unwrap(), "Older"),
                ]),
                None,
            ),
        )
        .await;
        mount_children(&h.server, &id(1), None, json!([para("a", "x")]), None).await;
        let conn = h.build(&factory(&h, 2), json!({}));
        let sink = h.sink();
        let cursor = json!({"v": 1, "since": rfc3339(since), "reconciled_at": now_unix()});
        run(&conn, Some(cursor), &sink).await.unwrap();
        assert_eq!(sink.summary(), vec![format!("+page/{}", id(1))]);
    }

    /// "Edited recently" (provisional version) was judged by the local clock; with ours running
    /// ahead, a just-edited page got a final version and a second edit in the same rounded
    /// minute was never read.
    #[tokio::test]
    async fn regression_notion_provisional_version_uses_server_clock() {
        let h = Harness::new().await;
        h.paste("ntn_1");
        mount_me(&h.server).await;
        let edited = SKEWED - 30;
        mount_search(
            &h.server,
            None,
            search_page(
                json!([page(&id(1), &rfc3339(edited).unwrap(), "Just now")]),
                None,
            )
            .insert_header("Date", http_date(SKEWED).as_str()),
        )
        .await;
        mount_children(&h.server, &id(1), None, json!([para("a", "x")]), None).await;
        let conn = h.build(&factory(&h, 2), json!({}));
        let sink = h.sink();
        run(&conn, None, &sink).await.unwrap();
        let v = sink.doc(&format!("page/{}", id(1))).version.unwrap();
        assert_eq!(v, format!("{}~", rfc3339(edited).unwrap()));
    }

    /// A page whose blocks answer 500 (after retries) used to fail the whole pass. It is now
    /// skipped, the rest is indexed, and `since` stays at or before it so the next pass reads it.
    #[tokio::test]
    async fn regression_notion_page_whose_blocks_fail_is_skipped_and_retried() {
        let h = Harness::new().await;
        h.paste("ntn_1");
        mount_me(&h.server).await;
        let (p0, p1, p2) = (id(0), id(1), id(2));
        let (e0, e1, e2) = (ago(3600), ago(7200), ago(10_800));
        let results = json!([
            page(&p0, &e0, "Fine"),
            page(&p1, &e1, "Broken"),
            page(&p2, &e2, "Odd blocks")
        ]);
        mount_search(&h.server, None, search_page(results.clone(), None)).await;
        mount_children(&h.server, &p0, None, json!([para("a", "fine")]), None).await;
        Mock::given(method("GET"))
            .and(path(format!("/notion/v1/blocks/{p1}/children")))
            .respond_with(server_error(500, None))
            .mount(&h.server)
            .await;
        // Malformed blocks: no type, rich text of the wrong shape, not an object.
        mount_children(
            &h.server,
            &p2,
            None,
            json!([{"id": "q"}, {"type": "paragraph", "paragraph": {"rich_text": 7}}, 42, para("ok", "survivor")]),
            None,
        )
        .await;
        let conn = h.build(&factory(&h, 1), json!({}));
        let sink = h.sink();
        let cursor = run(&conn, None, &sink).await.unwrap();
        let mut want = vec![format!("page/{p0}"), format!("page/{p2}")];
        want.sort();
        assert_eq!(unique_upserts(&sink), want);
        assert!(sink.text(&format!("page/{p2}")).contains("survivor"));
        assert_eq!(
            cursor["since"],
            json!(e1),
            "the watermark holds at the failed page"
        );

        h.server.reset().await;
        mount_me(&h.server).await;
        mount_search(&h.server, None, search_page(results, None)).await;
        mount_children(&h.server, &p1, None, json!([para("b", "recovered")]), None).await;
        let sink = h.sink();
        let cursor = run(&conn, Some(cursor), &sink).await.unwrap();
        assert_eq!(sink.summary(), vec![format!("+page/{p1}")]);
        assert!(sink.text(&format!("page/{p1}")).contains("recovered"));
        assert_eq!(cursor["since"], json!(e0));
    }

    #[tokio::test]
    async fn crash_at_any_point_resumes_backfill_and_incremental() {
        let h = Harness::new().await;
        h.paste("ntn_1");
        mount_me(&h.server).await;
        mount_six(&h.server).await;
        let f = factory(&h, 1);
        let settings = json!({});
        let seed = Replica::new();
        seed.apply(&Change::Upsert(Document::new(
            "page/stale",
            "text/markdown",
            nugget_connector::Content::Text("old".into()),
        )));
        let report = crash_matrix(&h, &f, &settings, None, seed.snapshot()).await;
        assert!(report.sends >= 7, "{report:?}");

        let replica = Replica::new();
        replica.restore(seed.snapshot());
        let conn = build_over(&h, &f, &settings, replica.clone());
        let cursor = run_pass(&*conn, None, &ReplicaSink::new(replica.clone()))
            .await
            .unwrap();
        assert_eq!(replica.len(), 6);

        h.server.reset().await;
        mount_me(&h.server).await;
        let mut gone = page(&id(2), &ago(1200), "Page 2");
        gone["in_trash"] = json!(true);
        mount_search(
            &h.server,
            None,
            search_page(
                json!([
                    page(&id(7), &ago(600), "New"),
                    page(&id(1), &ago(900), "Page 1 edited"),
                    gone,
                    page(&id(0), &ago(3600), "Page 0"),
                ]),
                None,
            ),
        )
        .await;
        mount_children(&h.server, &id(7), None, json!([para("n", "new")]), None).await;
        mount_children(&h.server, &id(1), None, json!([para("e", "edited")]), None).await;
        let report = crash_matrix(&h, &f, &settings, Some(cursor), replica.snapshot()).await;
        assert!(report.sends >= 3, "{report:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// GitHub
// ---------------------------------------------------------------------------------------------

mod github {
    use super::*;

    const GQL: &str = "/github/graphql";

    pub(super) fn factory(h: &Harness, retries: u32) -> GitHubFactory {
        GitHubFactory::new()
            .with_endpoints(h.endpoints())
            .with_retry(fast_retry(retries))
    }

    pub(super) fn data(v: Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({"data": v}))
    }

    pub(super) async fn mount_viewer(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path(GQL))
            .and(body_string_contains("viewer { login }"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(GITHUB_VIEWER)))
            .mount(server)
            .await;
    }

    pub(super) fn search(nodes: Value, next: Option<&str>) -> ResponseTemplate {
        data(json!({
            "rateLimit": {"remaining": 4000, "resetAt": ago(-3600)},
            "search": {"pageInfo": {"hasNextPage": next.is_some(), "endCursor": next}, "nodes": nodes}
        }))
    }

    pub(super) async fn mount_search(
        server: &MockServer,
        after: Option<&str>,
        resp: ResponseTemplate,
    ) {
        Mock::given(method("POST"))
            .and(path(GQL))
            .and(body_partial_json(
                json!({"variables": {"type": "ISSUE", "first": 20, "after": after}}),
            ))
            .respond_with(resp)
            .mount(server)
            .await;
    }

    pub(super) async fn mount_ids(server: &MockServer, nodes: Value) {
        Mock::given(method("POST"))
            .and(path(GQL))
            .and(body_partial_json(json!({"variables": {"first": 100}})))
            .respond_with(search(nodes, None))
            .mount(server)
            .await;
    }

    pub(super) fn issue(repo: &str, n: i64, updated: &str, title: &str) -> Value {
        json!({
            "__typename": "Issue", "id": format!("I_{repo}_{n}"), "number": n, "title": title,
            "url": format!("https://github.com/{repo}/issues/{n}"), "state": "OPEN",
            "body": format!("Body of {title}."), "createdAt": ago(90_000), "updatedAt": updated,
            "author": {"login": "bob"}, "repository": {"nameWithOwner": repo},
            "labels": {"nodes": []}, "assignees": {"nodes": []},
            "comments": {"totalCount": 0, "pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": []}
        })
    }

    pub(super) fn settings() -> Value {
        json!({"include_discussions": false, "window_days": 30})
    }

    /// Six search pages of two issues each, oldest first.
    pub(super) async fn mount_six(server: &MockServer) {
        for k in 0..6i64 {
            let after = (k > 0).then(|| format!("g{k}"));
            let next = (k < 5).then(|| format!("g{}", k + 1));
            let nodes = json!([
                issue(
                    "o/r",
                    2 * k + 1,
                    &stable_ago(80_000 - 1000 * k),
                    &format!("Issue {}", 2 * k + 1)
                ),
                issue(
                    "o/r",
                    2 * k + 2,
                    &stable_ago(79_500 - 1000 * k),
                    &format!("Issue {}", 2 * k + 2)
                ),
            ]);
            mount_search(server, after.as_deref(), search(nodes, next.as_deref())).await;
        }
    }

    pub(super) fn keys(range: std::ops::RangeInclusive<i64>) -> Vec<String> {
        let mut k: Vec<String> = range.map(|n| format!("issue/o/r/{n}")).collect();
        k.sort();
        k
    }

    #[tokio::test]
    async fn pagination_across_six_search_pages() {
        let h = Harness::new().await;
        h.paste("github_pat_1");
        mount_viewer(&h.server).await;
        mount_six(&h.server).await;
        let conn = h.build(&factory(&h, 2), settings());
        let sink = h.sink();
        let cursor = run(&conn, None, &sink).await.unwrap();
        assert_eq!(unique_upserts(&sink), keys(1..=12));
        // One checkpoint per page, the watermark at the newest item seen.
        assert_eq!(sink.checkpoints.lock().unwrap().len(), 6);
        let newest = crate::text::parse_rfc3339(&stable_ago(79_500 - 5000))
            .unwrap()
            .unix_timestamp();
        assert!(
            (cursor["scopes"]["issue|involves:ada"]["since"]
                .as_i64()
                .unwrap()
                - newest)
                .abs()
                <= 1
        );
        h.assert_egress("sync:github").await;
    }

    #[tokio::test]
    async fn throttling_and_server_errors_are_retried() {
        let h = Harness::new().await;
        h.paste("github_pat_1");
        Mock::given(method("POST"))
            .and(path(GQL))
            .and(body_string_contains("viewer { login }"))
            .respond_with(Sequence::new(vec![
                // GraphQL RATE_LIMITED inside a 200.
                ResponseTemplate::new(200)
                    .insert_header("Retry-After", "0")
                    .set_body_json(
                        json!({"errors": [{"type": "RATE_LIMITED", "message": "slow down"}]}),
                    ),
                ResponseTemplate::new(200).set_body_json(fixture(GITHUB_VIEWER)),
            ]))
            .mount(&h.server)
            .await;
        Mock::given(method("POST"))
            .and(path(GQL))
            .and(body_partial_json(
                json!({"variables": {"type": "ISSUE", "after": null}}),
            ))
            .respond_with(Sequence::new(vec![
                // Secondary rate limit: 403 with an HTTP-date Retry-After (= its Date: now).
                ResponseTemplate::new(403)
                    .insert_header("Date", http_date(SKEWED).as_str())
                    .insert_header("Retry-After", http_date(SKEWED).as_str())
                    .set_body_string("You have exceeded a secondary rate limit."),
                too_many_requests(0),
                server_error(502, None),
                search(json!([issue("o/r", 1, &ago(5000), "Calm")]), None),
            ]))
            .mount(&h.server)
            .await;
        let conn = h.build(&factory(&h, 4), settings());
        let sink = h.sink();
        run(&conn, None, &sink).await.unwrap();
        assert_eq!(unique_upserts(&sink), keys(1..=1));
        h.assert_egress("sync:github").await;
    }

    #[tokio::test]
    async fn retry_budget_surfaces_the_error_and_a_later_pass_resumes() {
        let h = Harness::new().await;
        h.paste("github_pat_1");
        mount_viewer(&h.server).await;
        mount_six(&h.server).await;
        Mock::given(method("POST"))
            .and(path(GQL))
            .and(body_partial_json(json!({"variables": {"after": "g3"}})))
            .respond_with(server_error(502, None))
            .with_priority(1)
            .mount(&h.server)
            .await;
        let s = settings();
        let replica = Replica::new();
        let sink = ReplicaSink::new(replica.clone());
        let conn = build_over(&h, &factory(&h, 2), &s, replica.clone());
        let r = run_pass(&*conn, None, &sink).await;
        assert!(matches!(r, Err(Error::Network(_))), "{r:?}");
        assert_eq!(replica.len(), 6);
        let committed = sink.committed().unwrap();

        h.server.reset().await;
        mount_viewer(&h.server).await;
        mount_six(&h.server).await;
        // The resumed pass reconciles too; the ids listing returns every live item.
        let all: Vec<Value> = (1..=12).map(|n| issue("o/r", n, &ago(1), "x")).collect();
        mount_ids(&h.server, json!(all)).await;
        let conn = build_over(&h, &factory(&h, 2), &s, replica.clone());
        run_pass(&*conn, Some(committed), &ReplicaSink::new(replica.clone()))
            .await
            .unwrap();
        assert_eq!(replica.keys(), keys(1..=12));
    }

    async fn mount_refreshing_pages(h: &Harness) {
        // Page 1 works with the old token; page 2 rejects it (401) and works with the new one.
        mount_search(
            &h.server,
            None,
            search(json!([issue("o/r", 1, &ago(5000), "One")]), Some("g1")),
        )
        .await;
        Mock::given(method("POST"))
            .and(path(GQL))
            .and(header("authorization", "Bearer old_tok"))
            .and(body_partial_json(json!({"variables": {"after": "g1"}})))
            .respond_with(
                ResponseTemplate::new(401).set_body_json(json!({"message": "Bad credentials"})),
            )
            .with_priority(1)
            .mount(&h.server)
            .await;
        mount_search(
            &h.server,
            Some("g1"),
            search(json!([issue("o/r", 2, &ago(4000), "Two")]), None),
        )
        .await;
    }

    #[tokio::test]
    async fn expired_token_mid_sync_is_refreshed_and_the_pass_continues() {
        let h = Harness::new().await;
        h.seed_oauth("old_tok", false);
        mount_viewer(&h.server).await;
        mount_refreshing_pages(&h).await;
        Mock::given(method("POST"))
            .and(path("/github/login/oauth/access_token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "new_tok", "refresh_token": "rt2", "expires_in": 28800, "token_type": "bearer"
            })))
            .expect(1)
            .mount(&h.server)
            .await;
        let mut s = settings();
        s["client_id"] = json!("Iv1.conformance");
        let conn = h.build(&factory(&h, 2), s.clone());
        let sink = h.sink();
        run(&conn, None, &sink).await.unwrap();
        assert_eq!(unique_upserts(&sink), keys(1..=2));
        let stored = h
            .secrets
            .get(&nugget_core::secrets::oauth_account(SOURCE))
            .unwrap()
            .unwrap();
        assert!(stored.contains("new_tok"));
        h.assert_egress("sync:github").await;
    }

    #[tokio::test]
    async fn failed_refresh_or_revoked_token_is_unauthorized() {
        // OAuth: the refresh is refused (invalid_grant).
        let h = Harness::new().await;
        h.seed_oauth("old_tok", false);
        mount_viewer(&h.server).await;
        mount_refreshing_pages(&h).await;
        Mock::given(method("POST"))
            .and(path("/github/login/oauth/access_token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})),
            )
            .mount(&h.server)
            .await;
        let mut s = settings();
        s["client_id"] = json!("Iv1.conformance");
        let conn = h.build(&factory(&h, 2), s.clone());
        let sink = h.sink();
        let r = run(&conn, None, &sink).await;
        assert!(is_unauthorized(&r), "{r:?}");
        // Progress up to the failure was checkpointed, nothing was reported as complete.
        assert_eq!(sink.checkpoints.lock().unwrap().len(), 1);

        // A pasted token revoked mid-sync.
        let h = Harness::new().await;
        h.paste("github_pat_1");
        mount_viewer(&h.server).await;
        mount_search(
            &h.server,
            None,
            search(json!([issue("o/r", 1, &ago(5000), "One")]), Some("g1")),
        )
        .await;
        mount_search(&h.server, Some("g1"), ResponseTemplate::new(401)).await;
        let conn = h.build(&factory(&h, 2), settings());
        let r = run(&conn, None, &h.sink()).await;
        assert!(is_unauthorized(&r), "{r:?}");
    }

    #[tokio::test]
    async fn transfers_renames_deletions_and_edits_after_sync() {
        let h = Harness::new().await;
        h.paste("github_pat_1");
        mount_viewer(&h.server).await;
        mount_search(
            &h.server,
            None,
            search(
                json!([
                    issue("o/r", 5, &ago(9000), "Will move"),
                    issue("o/r", 6, &ago(8000), "Will change")
                ]),
                None,
            ),
        )
        .await;
        let conn = h.build(&factory(&h, 2), settings());
        let mut cursor = run(&conn, None, &h.sink()).await.unwrap();

        h.server.reset().await;
        mount_viewer(&h.server).await;
        let mut edited = issue("o/r", 6, &ago(600), "Changed title");
        edited["body"] = json!("Edited body.");
        let moved = issue("o/other", 9, &ago(500), "Moved and renamed");
        mount_ids(&h.server, json!([edited, moved])).await;
        mount_search(&h.server, None, search(json!([edited, moved]), None)).await;
        cursor["reconciled_at"] = json!(0);
        let sink = h.sink();
        run(&conn, Some(cursor), &sink).await.unwrap();
        assert_eq!(
            sink.summary(),
            vec!["+issue/o/r/6", "+issue/o/other/9", "-issue/o/r/5"]
        );
        let d = sink.doc("issue/o/r/6");
        assert_eq!(d.title.as_deref(), Some("Changed title (o/r#6)"));
        assert!(text_of(&d).contains("Edited body."));
        assert_eq!(
            sink.doc("issue/o/other/9").metadata["repo"],
            json!("o/other")
        );
    }

    #[tokio::test]
    async fn huge_issues_are_capped_and_unicode_markdown_survives() {
        let h = Harness::new().await;
        h.paste("github_pat_1");
        mount_viewer(&h.server).await;
        let mut big = issue("o/r", 1, &ago(5000), "Huge");
        big["body"] = json!(big_text(200_000));
        big["comments"]["nodes"] = json!((0..50)
            .map(|i| json!({"author": {"login": "bob"}, "body": big_text(60_000), "createdAt": ago(4000 - i)}))
            .collect::<Vec<_>>());
        let title = UNICODE_SAMPLES[4];
        let mut uni = issue("o/r", 2, &ago(4000), title);
        uni["body"] = json!(format!(
            "{}\n\n```rust\nlet x = 1;\n```\n- [ ] task\u{0007}bell",
            UNICODE_SAMPLES.join("\n")
        ));
        mount_search(&h.server, None, search(json!([big, uni]), None)).await;
        let conn = h.build(&factory(&h, 2), settings());
        let sink = h.sink();
        run(&conn, None, &sink).await.unwrap();

        let b = sink.text("issue/o/r/1");
        assert!(b.chars().count() <= 400_001);
        assert!(b.contains(&format!("{}…", big_text(60_000))));
        assert!(!b.contains(&big_text(60_001)));

        let d = sink.doc("issue/o/r/2");
        assert!(d.title.as_deref().unwrap().contains(title));
        let u = text_of(&d);
        for s in UNICODE_SAMPLES.iter().filter(|s| !s.contains('\t')) {
            assert!(u.contains(s), "{s:?} lost in {u}");
        }
        assert!(
            u.contains("```rust\nlet x = 1;\n```") && u.contains("taskbell"),
            "{u}"
        );
    }

    /// `x-ratelimit-reset` is a Unix time on GitHub's clock; with ours an hour behind, the
    /// wait was an hour instead of seconds.
    #[tokio::test]
    async fn regression_github_ratelimit_reset_uses_server_date() {
        let h = Harness::new().await;
        h.paste("github_pat_1");
        let server_now = now_unix() + 3600;
        Mock::given(method("POST"))
            .and(path(GQL))
            .respond_with(
                ResponseTemplate::new(403)
                    .insert_header("Date", http_date(server_now).as_str())
                    .insert_header("x-ratelimit-remaining", "0")
                    .insert_header("x-ratelimit-reset", (server_now + 2).to_string().as_str()),
            )
            .mount(&h.server)
            .await;
        let conn = h.build(&factory(&h, 0), settings());
        let ms = rate_limited_ms(&run(&conn, None, &h.sink()).await).unwrap();
        assert!((2_000..=4_000).contains(&ms), "{ms}");
    }

    /// The incremental search starts before the watermark: GitHub's search index lags, so an
    /// item updated just before the newest one seen can appear in search later.
    #[tokio::test]
    async fn regression_github_incremental_search_overlaps_the_watermark() {
        let h = Harness::new().await;
        h.paste("github_pat_1");
        mount_viewer(&h.server).await;
        mount_search(&h.server, None, search(json!([]), None)).await;
        let since = now_unix() - 7200;
        let cursor = json!({"v": 1, "viewer": "ada", "window_from": since - 86_400,
            "reconciled_at": now_unix(), "scopes": {"issue|involves:ada": {"since": since}}});
        let conn = h.build(&factory(&h, 2), settings());
        run(&conn, Some(cursor), &h.sink()).await.unwrap();
        let reqs = h.server.received_requests().await.unwrap();
        let q = reqs
            .iter()
            .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
            .find_map(|b| b["variables"]["q"].as_str().map(str::to_string))
            .unwrap();
        let from = rfc3339(since - crate::github::WATERMARK_OVERLAP_SECS).unwrap();
        assert!(q.contains(&format!("updated:>={from}")), "{q}");
    }

    /// One issue whose extra comments fail to load (500 after retries) used to fail the whole
    /// pass. It is now skipped, the rest is indexed, and the watermark holds so it is retried.
    #[tokio::test]
    async fn regression_github_item_whose_comments_fail_does_not_stop_the_sync() {
        let h = Harness::new().await;
        h.paste("github_pat_1");
        mount_viewer(&h.server).await;
        let (t1, t2, t3) = (ago(9000), ago(8000), ago(7000));
        let mut broken = issue("o/r", 2, &t2, "Many comments");
        broken["comments"]["pageInfo"] = json!({"hasNextPage": true, "endCursor": "cc"});
        let nodes = json!([
            issue("o/r", 1, &t1, "One"),
            broken,
            {"__typename": "Issue", "number": 77, "updatedAt": t2},
            {"__typename": "Issue", "number": "eight", "repository": {"nameWithOwner": "o/r"}},
            issue("o/r", 3, &t3, "Three"),
        ]);
        mount_search(&h.server, None, search(nodes.clone(), None)).await;
        Mock::given(method("POST"))
            .and(path(GQL))
            .and(body_partial_json(json!({"variables": {"after": "cc"}})))
            .respond_with(server_error(500, None))
            .mount(&h.server)
            .await;
        let conn = h.build(&factory(&h, 1), settings());
        let sink = h.sink();
        let cursor = run(&conn, None, &sink).await.unwrap();
        assert_eq!(unique_upserts(&sink), vec!["issue/o/r/1", "issue/o/r/3"]);
        let held = cursor["scopes"]["issue|involves:ada"]["since"]
            .as_i64()
            .unwrap();
        assert_eq!(
            held,
            crate::text::parse_rfc3339(&t2).unwrap().unix_timestamp()
        );

        h.server.reset().await;
        mount_viewer(&h.server).await;
        mount_search(&h.server, None, search(nodes, None)).await;
        Mock::given(method("POST"))
            .and(path(GQL))
            .and(body_partial_json(json!({"variables": {"after": "cc"}})))
            .respond_with(data(json!({"node": {"comments": {"pageInfo": {"hasNextPage": false},
                "nodes": [{"author": {"login": "bob"}, "body": "late comment", "createdAt": t2}]}}})))
            .mount(&h.server)
            .await;
        let sink = h.sink();
        run(&conn, Some(cursor), &sink).await.unwrap();
        assert_eq!(sink.summary(), vec!["+issue/o/r/2"]);
        assert!(sink.text("issue/o/r/2").contains("late comment"));
    }

    #[tokio::test]
    async fn crash_at_any_point_resumes_backfill_and_incremental() {
        let h = Harness::new().await;
        h.paste("github_pat_1");
        mount_viewer(&h.server).await;
        mount_six(&h.server).await;
        let all: Vec<Value> = (1..=12).map(|n| issue("o/r", n, &ago(1), "x")).collect();
        mount_ids(&h.server, json!(all)).await;
        let f = factory(&h, 1);
        let s = settings();
        let seed = Replica::new();
        seed.apply(&Change::Upsert(Document::new(
            "issue/o/r/999",
            "text/markdown",
            nugget_connector::Content::Text("gone".into()),
        )));
        let report = crash_matrix(&h, &f, &s, None, seed.snapshot()).await;
        assert!(report.sends >= 13 && report.checkpoints >= 7, "{report:?}");

        let replica = Replica::new();
        replica.restore(seed.snapshot());
        let conn = build_over(&h, &f, &s, replica.clone());
        let cursor = run_pass(&*conn, None, &ReplicaSink::new(replica.clone()))
            .await
            .unwrap();
        assert_eq!(replica.len(), 12);

        h.server.reset().await;
        mount_viewer(&h.server).await;
        mount_search(
            &h.server,
            None,
            search(
                json!([
                    issue("o/r", 3, &ago(600), "Edited three"),
                    issue("o/r", 13, &ago(500), "Thirteen")
                ]),
                Some("n1"),
            ),
        )
        .await;
        mount_search(
            &h.server,
            Some("n1"),
            search(json!([issue("o/r", 14, &ago(400), "Fourteen")]), None),
        )
        .await;
        let report = crash_matrix(&h, &f, &s, Some(cursor), replica.snapshot()).await;
        assert!(report.sends >= 3, "{report:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// Linear
// ---------------------------------------------------------------------------------------------

mod linear {
    use super::*;

    const GQL: &str = "/linear/graphql";

    pub(super) fn factory(h: &Harness, retries: u32) -> LinearFactory {
        LinearFactory::new()
            .with_endpoints(h.endpoints())
            .with_retry(fast_retry(retries))
    }

    pub(super) fn data(v: Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({"data": v}))
    }

    pub(super) async fn mount_viewer(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path(GQL))
            .and(body_string_contains("viewer {"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture(LINEAR_VIEWER)))
            .mount(server)
            .await;
    }

    pub(super) fn issues(nodes: Value, next: Option<&str>) -> ResponseTemplate {
        data(
            json!({"issues": {"pageInfo": {"hasNextPage": next.is_some(), "endCursor": next}, "nodes": nodes}}),
        )
    }

    pub(super) async fn mount_issues(
        server: &MockServer,
        after: Option<&str>,
        resp: ResponseTemplate,
    ) {
        Mock::given(method("POST"))
            .and(path(GQL))
            .and(body_partial_json(
                json!({"variables": {"first": 50, "after": after}}),
            ))
            .respond_with(resp)
            .mount(server)
            .await;
    }

    pub(super) async fn mount_ids(server: &MockServer, nodes: Value) {
        Mock::given(method("POST"))
            .and(path(GQL))
            .and(body_partial_json(json!({"variables": {"first": 250}})))
            .respond_with(issues(nodes, None))
            .mount(server)
            .await;
    }

    pub(super) fn issue(id: &str, ident: &str, updated: &str, title: &str) -> Value {
        json!({
            "id": id, "identifier": ident, "title": title, "description": format!("About {title}."),
            "url": format!("https://linear.app/acme/issue/{ident}"), "priorityLabel": "High",
            "createdAt": ago(90_000), "updatedAt": updated, "archivedAt": null, "trashed": null,
            "state": {"name": "Todo", "type": "unstarted"}, "team": {"key": "ENG", "name": "Engineering"},
            "project": null, "assignee": {"id": "me", "name": "Ada Lovelace", "displayName": "ada"},
            "creator": {"id": "bob", "name": "Bob", "displayName": "bob"},
            "labels": {"nodes": []},
            "comments": {"pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": []}
        })
    }

    pub(super) fn settings() -> Value {
        json!({"window_days": 30})
    }

    /// Six pages of two issues each.
    pub(super) async fn mount_six(server: &MockServer) {
        for k in 0..6i64 {
            let after = (k > 0).then(|| format!("l{k}"));
            let next = (k < 5).then(|| format!("l{}", k + 1));
            let nodes = json!([
                issue(
                    &format!("i{}", 2 * k + 1),
                    &format!("ENG-{}", 2 * k + 1),
                    &stable_ago(80_000 - 1000 * k),
                    "a"
                ),
                issue(
                    &format!("i{}", 2 * k + 2),
                    &format!("ENG-{}", 2 * k + 2),
                    &stable_ago(79_500 - 1000 * k),
                    "b"
                ),
            ]);
            mount_issues(server, after.as_deref(), issues(nodes, next.as_deref())).await;
        }
    }

    pub(super) fn keys(range: std::ops::RangeInclusive<i64>) -> Vec<String> {
        let mut k: Vec<String> = range.map(|n| format!("issue/i{n}")).collect();
        k.sort();
        k
    }

    #[tokio::test]
    async fn pagination_across_six_pages() {
        let h = Harness::new().await;
        h.paste("lin_api_1");
        mount_viewer(&h.server).await;
        mount_six(&h.server).await;
        let conn = h.build(&factory(&h, 2), settings());
        let sink = h.sink();
        let cursor = run(&conn, None, &sink).await.unwrap();
        assert_eq!(unique_upserts(&sink), keys(1..=12));
        assert_eq!(sink.checkpoints.lock().unwrap().len(), 5);
        assert_eq!(cursor["since"], json!(stable_ago(79_500 - 5000)));
        assert!(cursor.get("pass").is_none());
        h.assert_egress("sync:linear").await;
    }

    #[tokio::test]
    async fn throttling_and_server_errors_are_retried() {
        let h = Harness::new().await;
        h.paste("lin_api_1");
        mount_viewer(&h.server).await;
        let ratelimited = |r: ResponseTemplate| {
            r.set_body_json(json!({"errors": [{"message": "Rate limit exceeded",
                "extensions": {"code": "RATELIMITED"}}]}))
        };
        Mock::given(method("POST"))
            .and(path(GQL))
            .and(body_partial_json(json!({"variables": {"first": 50}})))
            .respond_with(Sequence::new(vec![
                ratelimited(ResponseTemplate::new(400).insert_header("Retry-After", "0")),
                ratelimited(
                    ResponseTemplate::new(400)
                        .insert_header("Date", http_date(SKEWED).as_str())
                        .insert_header("Retry-After", http_date(SKEWED).as_str()),
                ),
                too_many_requests(0),
                server_error(503, Some(0)),
                issues(json!([issue("i1", "ENG-1", &ago(5000), "Calm")]), None),
            ]))
            .mount(&h.server)
            .await;
        let conn = h.build(&factory(&h, 5), settings());
        let sink = h.sink();
        run(&conn, None, &sink).await.unwrap();
        assert_eq!(unique_upserts(&sink), keys(1..=1));
        h.assert_egress("sync:linear").await;
    }

    #[tokio::test]
    async fn retry_budget_surfaces_the_error_and_a_later_pass_resumes() {
        let h = Harness::new().await;
        h.paste("lin_api_1");
        mount_viewer(&h.server).await;
        mount_six(&h.server).await;
        Mock::given(method("POST"))
            .and(path(GQL))
            .and(body_partial_json(json!({"variables": {"after": "l3"}})))
            .respond_with(server_error(503, None))
            .with_priority(1)
            .mount(&h.server)
            .await;
        let s = settings();
        let replica = Replica::new();
        let sink = ReplicaSink::new(replica.clone());
        let conn = build_over(&h, &factory(&h, 2), &s, replica.clone());
        let r = run_pass(&*conn, None, &sink).await;
        assert!(matches!(r, Err(Error::Network(_))), "{r:?}");
        assert_eq!(replica.len(), 6);
        let committed = sink.committed().unwrap();
        assert_eq!(committed["pass"]["after"], json!("l3"));

        h.server.reset().await;
        mount_viewer(&h.server).await;
        mount_six(&h.server).await;
        let all: Vec<Value> = (1..=12).map(|n| json!({"id": format!("i{n}")})).collect();
        mount_ids(&h.server, json!(all)).await;
        let conn = build_over(&h, &factory(&h, 2), &s, replica.clone());
        let resume = ReplicaSink::new(replica.clone());
        run_pass(&*conn, Some(committed), &resume).await.unwrap();
        assert_eq!(replica.keys(), keys(1..=12));
        // The resumed pass started at the page it stopped on.
        assert_eq!(resume.summary().len(), 6, "{:?}", resume.summary());
    }

    async fn mount_refreshing_pages(h: &Harness) {
        mount_issues(
            &h.server,
            None,
            issues(json!([issue("i1", "ENG-1", &ago(5000), "One")]), Some("l1")),
        )
        .await;
        Mock::given(method("POST"))
            .and(path(GQL))
            .and(header("authorization", "Bearer lin_oauth_old"))
            .and(body_partial_json(json!({"variables": {"after": "l1"}})))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({"errors": [
                {"message": "Authentication required", "extensions": {"code": "AUTHENTICATION_ERROR"}}
            ]})))
            .with_priority(1)
            .mount(&h.server)
            .await;
        mount_issues(
            &h.server,
            Some("l1"),
            issues(json!([issue("i2", "ENG-2", &ago(4000), "Two")]), None),
        )
        .await;
    }

    #[tokio::test]
    async fn expired_token_mid_sync_is_refreshed_and_the_pass_continues() {
        let h = Harness::new().await;
        h.seed_oauth("lin_oauth_old", false);
        mount_viewer(&h.server).await;
        mount_refreshing_pages(&h).await;
        Mock::given(method("POST"))
            .and(path("/linear/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "lin_oauth_new", "refresh_token": "rt2", "expires_in": 86400, "token_type": "Bearer"
            })))
            .expect(1)
            .mount(&h.server)
            .await;
        let conn = h.build(
            &factory(&h, 2),
            json!({"client_id": "lin-client", "window_days": 30}),
        );
        let sink = h.sink();
        run(&conn, None, &sink).await.unwrap();
        assert_eq!(unique_upserts(&sink), keys(1..=2));
        h.assert_egress("sync:linear").await;
    }

    #[tokio::test]
    async fn failed_refresh_or_revoked_key_is_unauthorized() {
        let h = Harness::new().await;
        h.seed_oauth("lin_oauth_old", false);
        mount_viewer(&h.server).await;
        mount_refreshing_pages(&h).await;
        Mock::given(method("POST"))
            .and(path("/linear/token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})),
            )
            .mount(&h.server)
            .await;
        let conn = h.build(
            &factory(&h, 2),
            json!({"client_id": "lin-client", "window_days": 30}),
        );
        let r = run(&conn, None, &h.sink()).await;
        assert!(is_unauthorized(&r), "{r:?}");

        // An expired OAuth token that cannot be refreshed before the pass starts.
        let h = Harness::new().await;
        h.seed_oauth("lin_oauth_old", true);
        Mock::given(method("POST"))
            .and(path("/linear/token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})),
            )
            .mount(&h.server)
            .await;
        let conn = h.build(&factory(&h, 2), json!({"client_id": "lin-client"}));
        let r = run(&conn, None, &h.sink()).await;
        assert!(is_unauthorized(&r), "{r:?}");

        // A personal key revoked mid-sync (HTTP 401).
        let h = Harness::new().await;
        h.paste("lin_api_1");
        mount_viewer(&h.server).await;
        mount_issues(
            &h.server,
            None,
            issues(json!([issue("i1", "ENG-1", &ago(5000), "One")]), Some("l1")),
        )
        .await;
        mount_issues(&h.server, Some("l1"), ResponseTemplate::new(401)).await;
        let conn = h.build(&factory(&h, 2), settings());
        let r = run(&conn, None, &h.sink()).await;
        assert!(is_unauthorized(&r), "{r:?}");
    }

    #[tokio::test]
    async fn moves_between_teams_renames_deletions_and_edits_after_sync() {
        let h = Harness::new().await;
        h.paste("lin_api_1");
        mount_viewer(&h.server).await;
        mount_issues(
            &h.server,
            None,
            issues(
                json!([
                    issue("i1", "ENG-7", &ago(9000), "Original"),
                    issue("i2", "ENG-8", &ago(8000), "Archive me"),
                    issue("i3", "ENG-9", &ago(7000), "Leaves scope")
                ]),
                None,
            ),
        )
        .await;
        let conn = h.build(&factory(&h, 2), settings());
        let mut cursor = run(&conn, None, &h.sink()).await.unwrap();

        h.server.reset().await;
        mount_viewer(&h.server).await;
        let mut moved = issue("i1", "OPS-3", &ago(600), "Renamed");
        moved["team"] = json!({"key": "OPS", "name": "Operations"});
        let mut archived = issue("i2", "ENG-8", &ago(500), "Archive me");
        archived["archivedAt"] = json!(ago(500));
        mount_ids(&h.server, json!([{"id": "i1"}, {"id": "i2"}])).await;
        mount_issues(&h.server, None, issues(json!([moved, archived]), None)).await;
        cursor["reconciled_at"] = json!(0);
        let sink = h.sink();
        run(&conn, Some(cursor), &sink).await.unwrap();
        assert_eq!(sink.summary(), vec!["+issue/i1", "-issue/i2", "-issue/i3"]);
        let d = sink.doc("issue/i1");
        assert_eq!(d.title.as_deref(), Some("OPS-3: Renamed"));
        assert_eq!(d.metadata["team"], json!("OPS"));
        assert!(text_of(&d).contains("Team: Operations"));
    }

    #[tokio::test]
    async fn huge_issues_are_capped_and_unicode_markdown_survives() {
        let h = Harness::new().await;
        h.paste("lin_api_1");
        mount_viewer(&h.server).await;
        let mut big = issue("i1", "ENG-1", &ago(5000), "Huge");
        big["description"] = json!(big_text(200_000));
        big["comments"]["nodes"] = json!((0..50)
            .map(|i| json!({"id": format!("c{i}"), "body": big_text(60_000), "createdAt": ago(4000 - i),
                "user": {"id": "bob", "name": "Bob"}, "parent": null}))
            .collect::<Vec<_>>());
        let mut uni = issue("i2", "ENG-2", &ago(4000), UNICODE_SAMPLES[2]);
        uni["description"] = json!(format!(
            "{}\n\n## Heading\n\n- item\u{0000}nul",
            UNICODE_SAMPLES.join("\n")
        ));
        mount_issues(&h.server, None, issues(json!([big, uni]), None)).await;
        let conn = h.build(&factory(&h, 2), settings());
        let sink = h.sink();
        run(&conn, None, &sink).await.unwrap();

        let b = sink.text("issue/i1");
        assert!(b.chars().count() <= 400_001);
        assert!(b.contains(&format!("{}…", big_text(60_000))));
        assert!(!b.contains(&big_text(60_001)));

        let d = sink.doc("issue/i2");
        assert!(d.title.as_deref().unwrap().contains(UNICODE_SAMPLES[2]));
        let u = text_of(&d);
        for s in UNICODE_SAMPLES.iter().filter(|s| !s.contains('\t')) {
            assert!(u.contains(s), "{s:?} lost in {u}");
        }
        assert!(u.contains("## Heading") && u.contains("itemnul"), "{u}");
    }

    /// `X-RateLimit-Requests-Reset` is a time on Linear's clock; with ours an hour behind, the
    /// wait was an hour instead of seconds.
    #[tokio::test]
    async fn regression_linear_ratelimit_reset_uses_server_date() {
        let h = Harness::new().await;
        h.paste("lin_api_1");
        let server_now = now_unix() + 3600;
        Mock::given(method("POST"))
            .and(path(GQL))
            .respond_with(
                ResponseTemplate::new(400)
                    .insert_header("Date", http_date(server_now).as_str())
                    .insert_header(
                        "x-ratelimit-requests-reset",
                        ((server_now + 2) * 1000).to_string().as_str(),
                    )
                    .set_body_json(json!({"errors": [{"message": "Rate limit exceeded",
                        "extensions": {"code": "RATELIMITED"}}]})),
            )
            .mount(&h.server)
            .await;
        let conn = h.build(&factory(&h, 0), settings());
        let ms = rate_limited_ms(&run(&conn, None, &h.sink()).await).unwrap();
        assert!((1_000..=3_000).contains(&ms), "{ms}");
    }

    /// An interrupted pass resumes at its saved page cursor. When that cursor has gone stale
    /// Linear answers HTTP 400 (not a 200 with errors), which was not recognised: every later
    /// pass failed on the same cursor and the source never synced again.
    #[tokio::test]
    async fn regression_linear_stale_page_cursor_400_restarts_the_pass() {
        let h = Harness::new().await;
        h.paste("lin_api_1");
        mount_viewer(&h.server).await;
        mount_issues(
            &h.server,
            Some("stale"),
            ResponseTemplate::new(400)
                .set_body_json(json!({"errors": [{"message": "Invalid cursor",
                "extensions": {"code": "INVALID_INPUT"}}]})),
        )
        .await;
        mount_issues(
            &h.server,
            None,
            issues(json!([issue("i1", "ENG-1", &ago(5000), "One")]), None),
        )
        .await;
        let cursor = json!({"v": 1, "viewer": "me", "window_from": now_unix() - 86_400 * 30,
            "since": ago(86_400), "reconciled_at": now_unix(), "pass": {"after": "stale"}});
        let conn = h.build(&factory(&h, 2), settings());
        let sink = h.sink();
        let next = run(&conn, Some(cursor), &sink).await.unwrap();
        assert_eq!(sink.summary(), vec!["+issue/i1"]);
        assert!(next.get("pass").is_none());
    }

    /// The incremental filter starts before `since`, so an issue whose `updatedAt` lands just
    /// behind the newest one seen is still read.
    #[tokio::test]
    async fn regression_linear_incremental_filter_overlaps_the_watermark() {
        let h = Harness::new().await;
        h.paste("lin_api_1");
        mount_viewer(&h.server).await;
        mount_issues(&h.server, None, issues(json!([]), None)).await;
        let since = now_unix() - 7200;
        let cursor = json!({"v": 1, "viewer": "me", "window_from": since - 86_400,
            "since": rfc3339(since), "reconciled_at": now_unix()});
        let conn = h.build(&factory(&h, 2), settings());
        run(&conn, Some(cursor), &h.sink()).await.unwrap();
        let reqs = h.server.received_requests().await.unwrap();
        let body = reqs
            .iter()
            .map(|r| String::from_utf8_lossy(&r.body).into_owned())
            .find(|b| b.contains("\"first\":50"))
            .unwrap();
        let from = rfc3339(since - crate::linear::WATERMARK_OVERLAP_SECS).unwrap();
        assert!(body.contains(&format!("{{\"gte\":\"{from}\"}}")), "{body}");
    }

    /// One issue whose extra comments fail to load used to fail the whole pass. It is now
    /// skipped and `since` stays at or before it, so the next pass reads it.
    #[tokio::test]
    async fn regression_linear_issue_whose_comments_fail_does_not_stop_the_sync() {
        let h = Harness::new().await;
        h.paste("lin_api_1");
        mount_viewer(&h.server).await;
        let (t1, t2, t3) = (ago(9000), ago(8000), ago(7000));
        let mut broken = issue("i2", "ENG-2", &t2, "Many comments");
        broken["comments"]["pageInfo"] = json!({"hasNextPage": true, "endCursor": "cc"});
        let nodes = json!([
            issue("i3", "ENG-3", &t3, "Three"),
            broken,
            null,
            {"id": "../evil", "updatedAt": t1},
            issue("i1", "ENG-1", &t1, "One"),
        ]);
        mount_issues(&h.server, None, issues(nodes.clone(), None)).await;
        Mock::given(method("POST"))
            .and(path(GQL))
            .and(body_partial_json(json!({"variables": {"id": "i2"}})))
            .respond_with(server_error(500, None))
            .mount(&h.server)
            .await;
        let conn = h.build(&factory(&h, 1), settings());
        let sink = h.sink();
        let cursor = run(&conn, None, &sink).await.unwrap();
        assert_eq!(unique_upserts(&sink), vec!["issue/i1", "issue/i3"]);
        assert_eq!(cursor["since"], json!(t2));

        h.server.reset().await;
        mount_viewer(&h.server).await;
        mount_issues(&h.server, None, issues(nodes, None)).await;
        Mock::given(method("POST"))
            .and(path(GQL))
            .and(body_partial_json(json!({"variables": {"id": "i2"}})))
            .respond_with(data(json!({"issue": {"comments": {"pageInfo": {"hasNextPage": false},
                "nodes": [{"id": "c9", "body": "late comment", "createdAt": t2, "user": {"id": "bob"}}]}}})))
            .mount(&h.server)
            .await;
        let sink = h.sink();
        let cursor = run(&conn, Some(cursor), &sink).await.unwrap();
        assert_eq!(sink.summary(), vec!["+issue/i2"]);
        assert!(sink.text("issue/i2").contains("late comment"));
        assert_eq!(cursor["since"], json!(t3));
    }

    #[tokio::test]
    async fn crash_at_any_point_resumes_backfill_and_incremental() {
        let h = Harness::new().await;
        h.paste("lin_api_1");
        mount_viewer(&h.server).await;
        mount_six(&h.server).await;
        let all: Vec<Value> = (1..=12).map(|n| json!({"id": format!("i{n}")})).collect();
        mount_ids(&h.server, json!(all)).await;
        let f = factory(&h, 1);
        let s = settings();
        let seed = Replica::new();
        seed.apply(&Change::Upsert(Document::new(
            "issue/stale",
            "text/markdown",
            nugget_connector::Content::Text("gone".into()),
        )));
        let report = crash_matrix(&h, &f, &s, None, seed.snapshot()).await;
        assert!(report.sends >= 13 && report.checkpoints >= 6, "{report:?}");

        let replica = Replica::new();
        replica.restore(seed.snapshot());
        let conn = build_over(&h, &f, &s, replica.clone());
        let cursor = run_pass(&*conn, None, &ReplicaSink::new(replica.clone()))
            .await
            .unwrap();
        assert_eq!(replica.len(), 12);

        h.server.reset().await;
        mount_viewer(&h.server).await;
        let mut archived = issue("i4", "ENG-4", &ago(300), "b");
        archived["trashed"] = json!(true);
        mount_issues(
            &h.server,
            None,
            issues(
                json!([issue("i13", "ENG-13", &ago(200), "new"), archived]),
                Some("n1"),
            ),
        )
        .await;
        mount_issues(
            &h.server,
            Some("n1"),
            issues(json!([issue("i3", "ENG-3", &ago(400), "edited")]), None),
        )
        .await;
        let report = crash_matrix(&h, &f, &s, Some(cursor), replica.snapshot()).await;
        assert!(report.sends >= 3 && report.checkpoints >= 2, "{report:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// Cross-connector: Retry-After as an HTTP-date
// ---------------------------------------------------------------------------------------------

/// `Retry-After` may be an HTTP-date; it was ignored (only delta-seconds were read), so the
/// server's wait was lost (or GitHub's default minute used). It is now honoured, measured
/// against the response's own `Date` so a skewed local clock does not distort it.
#[tokio::test]
async fn regression_retry_after_http_date_is_honoured_by_every_connector() {
    // Slack.
    let h = Harness::new().await;
    h.paste("xoxp-1");
    Mock::given(method("GET"))
        .and(path("/slack/auth.test"))
        .respond_with(throttled_until_date())
        .mount(&h.server)
        .await;
    let conn = h.build(&slack::factory(&h, 0), json!({}));
    assert_eq!(
        rate_limited_ms(&run(&conn, None, &h.sink()).await),
        Some(30_000)
    );

    // Notion.
    let h = Harness::new().await;
    h.paste("ntn_1");
    Mock::given(method("GET"))
        .and(path("/notion/v1/users/me"))
        .respond_with(throttled_until_date())
        .mount(&h.server)
        .await;
    let conn = h.build(&notion::factory(&h, 0), json!({}));
    assert_eq!(
        rate_limited_ms(&run(&conn, None, &h.sink()).await),
        Some(30_000)
    );

    // GitHub: the secondary rate limit, a 403 with Retry-After.
    let h = Harness::new().await;
    h.paste("github_pat_1");
    Mock::given(method("POST"))
        .and(path("/github/graphql"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("Date", http_date(SKEWED).as_str())
                .insert_header("Retry-After", http_date(SKEWED + 30).as_str())
                .set_body_string("You have exceeded a secondary rate limit."),
        )
        .mount(&h.server)
        .await;
    let conn = h.build(&github::factory(&h, 0), github::settings());
    assert_eq!(
        rate_limited_ms(&run(&conn, None, &h.sink()).await),
        Some(30_000)
    );

    // Linear: RATELIMITED in a 400.
    let h = Harness::new().await;
    h.paste("lin_api_1");
    Mock::given(method("POST"))
        .and(path("/linear/graphql"))
        .respond_with(
            throttled_until_date()
                .set_body_json(json!({"errors": [{"extensions": {"code": "RATELIMITED"}}]})),
        )
        .mount(&h.server)
        .await;
    let conn = h.build(&linear::factory(&h, 0), linear::settings());
    assert_eq!(
        rate_limited_ms(&run(&conn, None, &h.sink()).await),
        Some(30_000)
    );
}
