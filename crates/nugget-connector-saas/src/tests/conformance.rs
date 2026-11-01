//! Robustness regressions for Slack, Notion, GitHub and Linear against a mock server: crash
//! resumption at every send and checkpoint, clock skew, partial failure and watermarks.

use std::collections::BTreeMap;

use nugget_connector::testkit::{
    self, Replica, ReplicaSink, assert_crash_resumable, assert_scrubbed, run_pass, server_error,
};
use serde_json::json;
use time::OffsetDateTime;
use wiremock::matchers::{
    body_partial_json, body_string_contains, method, path, query_param, query_param_is_missing,
};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::text::{DAY_SECS, date, day_start, now_unix, rfc3339};
use crate::{GitHubFactory, LinearFactory, NotionFactory, SlackFactory};

// --- Shared helpers ---

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

fn ago(secs: i64) -> String {
    rfc3339(now_unix() - secs).unwrap()
}

/// `secs` before the start of today (UTC): the same value when fixtures are mounted again a
/// moment later, so re-mounted mocks describe unchanged items.
fn stable_ago(secs: i64) -> String {
    rfc3339(day_start(now_unix()) - secs).unwrap()
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

/// A GraphQL listing responder that honours the request's updated-since filter (GitHub
/// `updated:>=` in `q`, Linear `updatedAt.gte`), so tests prove what the watermark lets in.
struct UpdatedSince {
    nodes: Vec<Value>,
    wrap: fn(Value) -> ResponseTemplate,
}

fn since_of(body: &Value) -> Option<OffsetDateTime> {
    if let Some(q) = body["variables"]["q"].as_str() {
        let rest = q.split("updated:>=").nth(1)?;
        return crate::text::parse_rfc3339(rest.split_whitespace().next()?);
    }
    body["variables"]["filter"]["and"]
        .as_array()?
        .iter()
        .find_map(|c| c["updatedAt"]["gte"].as_str())
        .and_then(crate::text::parse_rfc3339)
}

impl wiremock::Respond for UpdatedSince {
    fn respond(&self, req: &wiremock::Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let from = since_of(&body).expect("the listing carries an updated-since filter");
        let nodes: Vec<Value> = self
            .nodes
            .iter()
            .filter(|n| {
                n["updatedAt"]
                    .as_str()
                    .and_then(crate::text::parse_rfc3339)
                    .is_none_or(|t| t >= from)
            })
            .cloned()
            .collect();
        (self.wrap)(json!(nodes))
    }
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

// --- Slack ---

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

// --- Notion ---

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

// --- GitHub ---

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

    pub(super) async fn mount_search<R: wiremock::Respond + 'static>(
        server: &MockServer,
        after: Option<&str>,
        resp: R,
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
        // Honours `updated:>=`: #2 is only listed because the watermark held at it.
        mount_search(
            &h.server,
            None,
            UpdatedSince {
                nodes: nodes.as_array().unwrap().clone(),
                wrap: |n| search(n, None),
            },
        )
        .await;
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

// --- Linear ---

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

    pub(super) async fn mount_issues<R: wiremock::Respond + 'static>(
        server: &MockServer,
        after: Option<&str>,
        resp: R,
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

// --- Cross-connector: Retry-After as an HTTP-date ---

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

// --- Review follow-ups: failing items, watermarks, cursors ---

mod followups {
    use super::*;
    use crate::api::MAX_ITEM_ATTEMPTS;
    use crate::text::parse_rfc3339;

    fn unix(rfc: &str) -> i64 {
        parse_rfc3339(rfc).unwrap().unix_timestamp()
    }

    // ---- Slack ----

    fn slack_thread(parent: &str, text: &str) -> Value {
        json!({"ts": parent, "user": "U2", "text": text, "thread_ts": parent,
               "reply_count": 1, "latest_reply": parent})
    }

    async fn mount_replies(server: &MockServer, resp: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path("/slack/conversations.replies"))
            .respond_with(resp)
            .mount(server)
            .await;
    }

    /// A thread that failed on a day older than `recheck_days` was never read again: the
    /// backfill moved past its day. It is now kept in the cursor and retried next pass.
    #[tokio::test]
    async fn regression_slack_failed_thread_older_than_recheck_is_retried_next_pass() {
        let h = Harness::new().await;
        h.paste("xoxp-1");
        slack::mount_base(&h.server, json!([{"id": "C1", "name": "general"}])).await;
        let parent = slack::ts(5, 0);
        slack::mount_history(
            &h.server,
            "C1",
            None,
            slack::page(json!([slack_thread(&parent, "old thread")]), None),
        )
        .await;
        mount_replies(&h.server, server_error(500, None)).await;
        let settings = json!({"window_days": 30, "recheck_days": 1});
        let conn = h.build(&slack::factory(&h, 0), settings);
        let sink = h.sink();
        let cursor = run(&conn, None, &sink).await.unwrap();
        assert_eq!(
            sink.summary(),
            vec![format!("+{}", slack::day_key("C1", 5))]
        );
        assert_eq!(
            cursor["channels"]["C1"]["failed_threads"][&parent],
            json!(1)
        );
        assert!(cursor["channels"]["C1"].get("backfill").is_none());

        h.server.reset().await;
        slack::mount_base(&h.server, json!([{"id": "C1", "name": "general"}])).await;
        mount_replies(
            &h.server,
            slack::ok(json!({"messages": [
                slack_thread(&parent, "old thread"),
                {"ts": slack::ts(5, 1), "user": "U2", "text": "the reply", "thread_ts": parent}
            ], "has_more": false})),
        )
        .await;
        let sink = h.sink();
        let cursor = run(&conn, Some(cursor), &sink).await.unwrap();
        let key = format!("C1/thread/{parent}");
        assert_eq!(sink.summary(), vec![format!("+{key}")]);
        assert!(sink.text(&key).contains("the reply"));
        assert!(cursor["channels"]["C1"].get("failed_threads").is_none());
    }

    /// A thread that keeps failing is retried a bounded number of passes, then dropped.
    #[tokio::test]
    async fn regression_slack_thread_failing_every_pass_is_given_up() {
        let h = Harness::new().await;
        h.paste("xoxp-1");
        slack::mount_base(&h.server, json!([{"id": "C1", "name": "general"}])).await;
        let parent = slack::ts(5, 0);
        slack::mount_history(
            &h.server,
            "C1",
            None,
            slack::page(json!([slack_thread(&parent, "cursed")]), None),
        )
        .await;
        mount_replies(&h.server, server_error(500, None)).await;
        let conn = h.build(
            &slack::factory(&h, 0),
            json!({"window_days": 30, "recheck_days": 1}),
        );
        let mut cursor = None;
        for pass in 1..=MAX_ITEM_ATTEMPTS {
            let next = run(&conn, cursor, &h.sink()).await.unwrap();
            let failed = &next["channels"]["C1"]["failed_threads"];
            if pass < MAX_ITEM_ATTEMPTS {
                assert_eq!(failed[&parent], json!(pass), "pass {pass}");
            } else {
                assert!(failed.is_null(), "pass {pass}: {next}");
            }
            cursor = Some(next);
        }
    }

    /// Slack skipped every failing thread without limit, so a broken `conversations.replies`
    /// "succeeded". Five failures in a row now stop the pass.
    #[tokio::test]
    async fn regression_slack_threads_failing_in_a_row_stop_the_pass() {
        let h = Harness::new().await;
        h.paste("xoxp-1");
        slack::mount_base(&h.server, json!([{"id": "C1", "name": "general"}])).await;
        let threads: Vec<Value> = (0..6)
            .rev()
            .map(|i| slack_thread(&slack::ts(1, i), &format!("t{i}")))
            .collect();
        slack::mount_history(&h.server, "C1", None, slack::page(json!(threads), None)).await;
        mount_replies(&h.server, server_error(500, None)).await;
        let conn = h.build(&slack::factory(&h, 0), json!({"window_days": 7}));
        let r = run(&conn, None, &h.sink()).await;
        assert!(matches!(r, Err(Error::Network(_))), "{r:?}");
    }

    // ---- items that fail on every pass stop holding the watermark ----

    #[tokio::test]
    async fn regression_github_item_failing_every_pass_stops_holding_the_watermark() {
        let h = Harness::new().await;
        h.paste("github_pat_1");
        github::mount_viewer(&h.server).await;
        let (t1, t2, t3) = (ago(9000), ago(8000), ago(7000));
        let mut broken = github::issue("o/r", 2, &t2, "Broken");
        broken["comments"]["pageInfo"] = json!({"hasNextPage": true, "endCursor": "cc"});
        github::mount_search(
            &h.server,
            None,
            UpdatedSince {
                nodes: vec![
                    github::issue("o/r", 1, &t1, "One"),
                    broken,
                    github::issue("o/r", 3, &t3, "Three"),
                ],
                wrap: |n| github::search(n, None),
            },
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/github/graphql"))
            .and(body_partial_json(json!({"variables": {"after": "cc"}})))
            .respond_with(server_error(500, None))
            .mount(&h.server)
            .await;
        let conn = h.build(&github::factory(&h, 0), github::settings());
        let mut cursor = None;
        for pass in 1..=MAX_ITEM_ATTEMPTS {
            let next = run(&conn, cursor, &h.sink()).await.unwrap();
            let since = next["scopes"]["issue|involves:ada"]["since"]
                .as_i64()
                .unwrap();
            if pass < MAX_ITEM_ATTEMPTS {
                assert_eq!(since, unix(&t2), "pass {pass}");
                assert_eq!(next["failed"]["issue/o/r/2"], json!(pass));
            } else {
                assert_eq!(since, unix(&t3), "pass {pass}: the watermark moves on");
                assert!(next.get("failed").is_none());
            }
            cursor = Some(next);
        }
    }

    // ---- watermarks and cursors ----

    /// The saved watermark was the overlapped start (stored − 300 s), so it crept backwards
    /// on every pass that found nothing newer.
    #[tokio::test]
    async fn regression_github_watermark_never_moves_back() {
        let h = Harness::new().await;
        h.paste("github_pat_1");
        github::mount_viewer(&h.server).await;
        let since = now_unix() - 7200;
        // An already-indexed item inside the overlap is listed again.
        let inside = rfc3339(since - 60).unwrap();
        h.know("issue/o/r/1", &inside);
        github::mount_search(
            &h.server,
            None,
            github::search(json!([github::issue("o/r", 1, &inside, "Old")]), None),
        )
        .await;
        let cursor = json!({"v": 1, "viewer": "ada", "window_from": since - 86_400,
            "reconciled_at": now_unix(), "scopes": {"issue|involves:ada": {"since": since}}});
        let conn = h.build(&github::factory(&h, 0), github::settings());
        let sink = h.sink();
        let next = run(&conn, Some(cursor), &sink).await.unwrap();
        assert!(sink.summary().is_empty());
        assert_eq!(next["scopes"]["issue|involves:ada"]["since"], json!(since));
    }

    /// A failing issue whose `updatedAt` cannot be read did not hold `since`.
    #[tokio::test]
    async fn regression_linear_failing_issue_without_a_time_holds_since() {
        let h = Harness::new().await;
        h.paste("lin_api_1");
        linear::mount_viewer(&h.server).await;
        let since = ago(20_000);
        let mut broken = linear::issue("i2", "ENG-2", "not a time", "Broken");
        broken["comments"]["pageInfo"] = json!({"hasNextPage": true, "endCursor": "cc"});
        linear::mount_issues(
            &h.server,
            None,
            linear::issues(
                json!([linear::issue("i3", "ENG-3", &ago(7000), "Three"), broken]),
                None,
            ),
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/linear/graphql"))
            .and(body_partial_json(json!({"variables": {"id": "i2"}})))
            .respond_with(server_error(500, None))
            .mount(&h.server)
            .await;
        let cursor = json!({"v": 1, "viewer": "me", "window_from": now_unix() - 86_400 * 30,
            "since": since, "reconciled_at": now_unix()});
        let conn = h.build(&linear::factory(&h, 0), linear::settings());
        let sink = h.sink();
        let next = run(&conn, Some(cursor), &sink).await.unwrap();
        assert_eq!(sink.summary(), vec!["+issue/i3"]);
        assert_eq!(next["since"], json!(since));
    }

    fn invalid_input() -> ResponseTemplate {
        ResponseTemplate::new(400).set_body_json(json!({"errors": [{"message": "Bad filter",
            "extensions": {"code": "INVALID_INPUT"}}]}))
    }

    async fn issue_requests(h: &Harness) -> usize {
        h.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| String::from_utf8_lossy(&r.body).contains("\"first\":50"))
            .count()
    }

    /// After restarting from a stale saved cursor, a second 400 used to leave the stale
    /// position in the cursor, so every poll restarted from it. It is now cleared.
    #[tokio::test]
    async fn regression_linear_second_400_after_restart_clears_the_saved_pass() {
        let h = Harness::new().await;
        h.paste("lin_api_1");
        linear::mount_viewer(&h.server).await;
        linear::mount_issues(&h.server, Some("stale"), invalid_input()).await;
        linear::mount_issues(&h.server, None, invalid_input()).await;
        let cursor = json!({"v": 1, "viewer": "me", "window_from": now_unix() - 86_400 * 30,
            "since": ago(86_400), "reconciled_at": now_unix(), "pass": {"after": "stale"}});
        let conn = h.build(&linear::factory(&h, 0), linear::settings());
        let sink = h.sink();
        let r = run(&conn, Some(cursor), &sink).await;
        assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
        let saved = sink.checkpoints.lock().unwrap().last().cloned().unwrap();
        assert!(saved.get("pass").is_none(), "{saved}");
        assert_eq!(issue_requests(&h).await, 2);
    }

    // ---- credentials and rate limits while loading one item stop the pass ----

    fn throttled_long() -> ResponseTemplate {
        ResponseTemplate::new(429).insert_header("Retry-After", "3600")
    }

    #[tokio::test]
    async fn unauthorized_or_rate_limited_while_loading_an_item_stops_the_pass() {
        // Slack: one thread's replies.
        for (resp, want_auth) in [
            (ResponseTemplate::new(401), true),
            (throttled_long(), false),
        ] {
            let h = Harness::new().await;
            h.paste("xoxp-1");
            slack::mount_base(&h.server, json!([{"id": "C1", "name": "general"}])).await;
            let parent = slack::ts(1, 0);
            slack::mount_history(
                &h.server,
                "C1",
                None,
                slack::page(json!([slack_thread(&parent, "t")]), None),
            )
            .await;
            mount_replies(&h.server, resp).await;
            let conn = h.build(&slack::factory(&h, 2), json!({"window_days": 7}));
            let r = run(&conn, None, &h.sink()).await;
            if want_auth {
                assert!(is_unauthorized(&r), "slack: {r:?}");
            } else {
                assert!(matches!(r, Err(Error::RateLimited { .. })), "slack: {r:?}");
            }
        }

        // Notion: one page's blocks.
        for (resp, want_auth) in [
            (ResponseTemplate::new(401), true),
            (throttled_long(), false),
        ] {
            let h = Harness::new().await;
            h.paste("ntn_1");
            notion::mount_me(&h.server).await;
            notion::mount_search(
                &h.server,
                None,
                notion::search_page(json!([notion::page(&notion::id(1), &ago(7200), "A")]), None),
            )
            .await;
            Mock::given(method("GET"))
                .and(path(format!(
                    "/notion/v1/blocks/{}/children",
                    notion::id(1)
                )))
                .respond_with(resp)
                .mount(&h.server)
                .await;
            let conn = h.build(&notion::factory(&h, 2), json!({}));
            let r = run(&conn, None, &h.sink()).await;
            if want_auth {
                assert!(is_unauthorized(&r), "notion: {r:?}");
            } else {
                assert!(matches!(r, Err(Error::RateLimited { .. })), "notion: {r:?}");
            }
        }

        // GitHub and Linear: one item's extra comments.
        for (resp, want_auth) in [
            (ResponseTemplate::new(401), true),
            (throttled_long(), false),
        ] {
            let h = Harness::new().await;
            h.paste("github_pat_1");
            github::mount_viewer(&h.server).await;
            let mut i = github::issue("o/r", 1, &ago(5000), "One");
            i["comments"]["pageInfo"] = json!({"hasNextPage": true, "endCursor": "cc"});
            github::mount_search(&h.server, None, github::search(json!([i]), None)).await;
            Mock::given(method("POST"))
                .and(path("/github/graphql"))
                .and(body_partial_json(json!({"variables": {"after": "cc"}})))
                .respond_with(resp.clone())
                .mount(&h.server)
                .await;
            let conn = h.build(&github::factory(&h, 2), github::settings());
            let r = run(&conn, None, &h.sink()).await;
            if want_auth {
                assert!(is_unauthorized(&r), "github: {r:?}");
            } else {
                assert!(matches!(r, Err(Error::RateLimited { .. })), "github: {r:?}");
            }

            let h = Harness::new().await;
            h.paste("lin_api_1");
            linear::mount_viewer(&h.server).await;
            let mut i = linear::issue("i1", "ENG-1", &ago(5000), "One");
            i["comments"]["pageInfo"] = json!({"hasNextPage": true, "endCursor": "cc"});
            linear::mount_issues(&h.server, None, linear::issues(json!([i]), None)).await;
            Mock::given(method("POST"))
                .and(path("/linear/graphql"))
                .and(body_partial_json(json!({"variables": {"id": "i1"}})))
                .respond_with(resp)
                .mount(&h.server)
                .await;
            let conn = h.build(&linear::factory(&h, 2), linear::settings());
            let r = run(&conn, None, &h.sink()).await;
            if want_auth {
                assert!(is_unauthorized(&r), "linear: {r:?}");
            } else {
                assert!(matches!(r, Err(Error::RateLimited { .. })), "linear: {r:?}");
            }
        }
    }
}
