use serde_json::json;
use wiremock::matchers::{body_partial_json, body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::LinearFactory;
use crate::text::{now_unix, rfc3339};

const GQL: &str = "/linear/graphql";

fn linear(h: &Harness, settings: Value) -> Arc<dyn Connector> {
    h.build(
        &LinearFactory::new()
            .with_endpoints(h.endpoints())
            .with_retry(fast_retry(2)),
        settings,
    )
}

fn ago(secs: i64) -> String {
    rfc3339(now_unix() - secs).unwrap()
}

fn data(v: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"data": v}))
}

async fn mount_viewer(server: &MockServer, auth: &str) {
    Mock::given(method("POST"))
        .and(path(GQL))
        .and(header("authorization", auth))
        .and(body_string_contains("viewer {"))
        .respond_with(data(
            json!({"viewer": {"id": "me", "name": "Ada Lovelace", "displayName": "ada"}}),
        ))
        .mount(server)
        .await;
}

fn issues(nodes: Value, next: Option<&str>) -> ResponseTemplate {
    data(
        json!({"issues": {"pageInfo": {"hasNextPage": next.is_some(), "endCursor": next}, "nodes": nodes}}),
    )
}

/// A full `issues` page (`first: 50`), optionally after `after`.
async fn mount_issues(server: &MockServer, after: Option<&str>, nodes: Value, next: Option<&str>) {
    Mock::given(method("POST"))
        .and(path(GQL))
        .and(body_partial_json(
            json!({"variables": {"first": 50, "after": after}}),
        ))
        .respond_with(issues(nodes, next))
        .mount(server)
        .await;
}

fn issue(id: &str, ident: &str, updated: &str, creator: &str) -> Value {
    json!({
        "id": id, "identifier": ident, "title": format!("Title {ident}"), "description": "Do the thing.",
        "url": format!("https://linear.app/acme/issue/{ident}"), "priorityLabel": "High",
        "createdAt": ago(90_000), "updatedAt": updated, "archivedAt": null, "trashed": null,
        "state": {"name": "In Progress", "type": "started"}, "team": {"key": "ENG", "name": "Engineering"},
        "project": {"name": "Q4"}, "assignee": {"id": "me", "name": "Ada Lovelace", "displayName": "ada"},
        "creator": {"id": creator, "name": creator, "displayName": creator},
        "labels": {"nodes": [{"name": "bug"}]},
        "comments": {"pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": []}
    })
}

#[tokio::test]
async fn sync_without_token_is_unauthorized() {
    let h = Harness::new().await;
    let conn = linear(&h, json!({}));
    assert!(is_unauthorized(&run(&conn, None, &h.sink()).await));
    assert!(h.server.received_requests().await.unwrap().is_empty());

    h.paste("lin_api_bad");
    Mock::given(method("POST"))
        .and(path(GQL))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "errors": [{"message": "Authentication required", "extensions": {"code": "AUTHENTICATION_ERROR"}}]
        })))
        .mount(&h.server)
        .await;
    let r = run(&conn, None, &h.sink()).await;
    assert!(is_unauthorized(&r), "{r:?}");
}

#[tokio::test]
async fn full_sync_pages_threads_comments_and_deletes_archived() {
    let h = Harness::new().await;
    h.paste("lin_api_1");
    mount_viewer(&h.server, "lin_api_1").await;
    let t1 = ago(7200);
    let t2 = ago(3600);
    let mut a = issue("i1", "ENG-1", &t1, "bob");
    a["comments"] = json!({"pageInfo": {"hasNextPage": true, "endCursor": "cc"}, "nodes": [
        {"id": "c1", "body": "Looks hard", "createdAt": ago(80_000), "user": {"id": "bob", "name": "Bob"}, "parent": null}
    ]});
    let mut gone = issue("i3", "ENG-3", &t2, "me");
    gone["archivedAt"] = json!(t2);
    mount_issues(&h.server, None, json!([a]), Some("p2")).await;
    mount_issues(
        &h.server,
        Some("p2"),
        json!([issue("i2", "ENG-2", &t2, "me"), gone, {"id": "../x"}]),
        None,
    )
    .await;
    Mock::given(method("POST"))
        .and(path(GQL))
        .and(body_partial_json(json!({"variables": {"id": "i1", "after": "cc"}})))
        .respond_with(data(json!({"issue": {"comments": {"pageInfo": {"hasNextPage": false}, "nodes": [
            {"id": "c2", "body": "I'll take it", "createdAt": ago(70_000), "user": {"id": "me", "displayName": "ada"}, "parent": {"id": "c1"}},
            {"id": "c3", "body": "Done", "createdAt": ago(60_000), "user": {"id": "me", "displayName": "ada"}, "parent": null}
        ]}}})))
        .expect(1)
        .mount(&h.server)
        .await;
    h.know("issue/stale", "x");
    h.know("issue/i3", "x");

    let conn = linear(&h, json!({"window_days": 30}));
    let sink = h.sink();
    let cursor = run(&conn, None, &sink).await.unwrap();
    assert_eq!(
        sink.summary(),
        vec!["+issue/i1", "+issue/i2", "-issue/i3", "-issue/stale"]
    );
    let md = sink.text("issue/i1");
    assert!(md.starts_with("# ENG-1: Title ENG-1\n\nTeam: Engineering · State: In Progress · Priority: High · Project: Q4"), "{md}");
    assert!(
        md.contains("Created by bob") && md.contains("Assignee: ada") && md.contains("Labels: bug")
    );
    let thread = md.find("**Bob** commented").unwrap();
    let reply = md.find("> **ada** replied").unwrap();
    let later = md.find("**ada** commented").unwrap();
    assert!(thread < reply && reply < later, "{md}");
    let d = sink.doc("issue/i1");
    assert_eq!(d.version.as_deref(), Some(t1.as_str()));
    assert_eq!(
        d.url.as_deref(),
        Some("https://linear.app/acme/issue/ENG-1")
    );
    assert_eq!(d.metadata["from_me"], json!(false));
    assert_eq!(d.metadata["my_message_count"], json!(2));
    assert_eq!(d.metadata["team"], json!("ENG"));
    assert_eq!(d.metadata["me"], json!("ada"));
    assert_eq!(sink.doc("issue/i2").metadata["from_me"], json!(true));

    assert_eq!(cursor["since"], json!(t2));
    assert!(cursor.get("pass").is_none());
    assert_eq!(cursor["viewer"], json!("me"));
    let cps = sink.checkpoints.lock().unwrap().clone();
    assert_eq!(cps[0]["pass"]["after"], json!("p2"));

    // The filter: who, plus the update window.
    let reqs = h.server.received_requests().await.unwrap();
    let body: Value = reqs
        .iter()
        .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
        .find(|b| b["variables"]["filter"].is_object())
        .unwrap();
    let f = body["variables"]["filter"]["and"].as_array().unwrap();
    assert_eq!(f[0]["or"].as_array().unwrap().len(), 3);
    assert_eq!(
        f[0]["or"][2],
        json!({"subscribers": {"some": {"isMe": {"eq": true}}}})
    );
    assert!(f[1]["updatedAt"]["gte"].is_string());
    h.assert_egress("sync:linear").await;
}

#[tokio::test]
async fn incremental_skips_unchanged_and_resumes_an_interrupted_pass() {
    let h = Harness::new().await;
    h.paste("lin_api_1");
    mount_viewer(&h.server, "lin_api_1").await;
    let t = ago(3600);
    let newer = ago(60);
    h.know("issue/i1", &t);
    h.know("issue/out-of-scope", &t); // stays: no reconcile is due
    // The stored page cursor has expired: the pass starts over once.
    Mock::given(method("POST"))
        .and(path(GQL))
        .and(body_partial_json(json!({"variables": {"after": "stale"}})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"errors": [{"message": "Invalid cursor"}], "data": null})),
        )
        .expect(1)
        .mount(&h.server)
        .await;
    mount_issues(
        &h.server,
        None,
        json!([
            issue("i1", "ENG-1", &t, "me"),
            issue("i2", "ENG-2", &newer, "bob")
        ]),
        None,
    )
    .await;
    let cursor = json!({"v": 1, "viewer": "me", "window_from": now_unix() - 86_400, "since": ago(7200),
        "reconciled_at": now_unix(), "pass": {"after": "stale", "max_seen": t}});
    let conn = linear(&h, json!({"include": ["assigned"], "teams": ["ENG"]}));
    let sink = h.sink();
    let next = run(&conn, Some(cursor), &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+issue/i2"]);
    assert_eq!(next["since"], json!(newer));
    let reqs = h.server.received_requests().await.unwrap();
    let body: Value = reqs
        .iter()
        .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
        .find(|b| b["variables"]["filter"].is_object())
        .unwrap();
    let f = &body["variables"]["filter"]["and"];
    assert_eq!(f[0]["or"], json!([{"assignee": {"isMe": {"eq": true}}}]));
    assert_eq!(f[1], json!({"team": {"key": {"in": ["ENG"]}}}));
}

#[tokio::test]
async fn periodic_reconcile_deletes_issues_that_left_the_scope() {
    let h = Harness::new().await;
    h.paste("lin_api_1");
    mount_viewer(&h.server, "lin_api_1").await;
    let t = ago(3600);
    h.know("issue/i1", &t);
    h.know("issue/i9", &t);
    Mock::given(method("POST"))
        .and(path(GQL))
        .and(body_partial_json(json!({"variables": {"first": 250}})))
        .respond_with(issues(json!([{"id": "i1"}]), None))
        .expect(1)
        .mount(&h.server)
        .await;
    mount_issues(&h.server, None, json!([]), None).await;
    let cursor = json!({"v": 1, "viewer": "me", "window_from": now_unix() - 86_400, "since": t, "reconciled_at": 1});
    let conn = linear(&h, json!({}));
    let sink = h.sink();
    run(&conn, Some(cursor), &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["-issue/i9"]);
}

#[tokio::test]
async fn rate_limits_are_retried_or_returned() {
    let h = Harness::new().await;
    h.paste("lin_api_1");
    mount_viewer(&h.server, "lin_api_1").await;
    let soon_ms = (now_unix() * 1000 + 50).to_string();
    Mock::given(method("POST"))
        .and(path(GQL))
        .and(body_string_contains("issues("))
        .respond_with(
            ResponseTemplate::new(400)
                .insert_header("x-ratelimit-requests-reset", soon_ms.as_str())
                .set_body_json(json!({"errors": [{"message": "Rate limit exceeded", "extensions": {"code": "RATELIMITED"}}]})),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&h.server)
        .await;
    mount_issues(&h.server, None, json!([]), None).await;
    let conn = linear(&h, json!({}));
    // The reset is ≥1 s away (minimum), longer than the test policy's max delay: returned.
    let err = run(&conn, None, &h.sink()).await.unwrap_err();
    assert!(
        matches!(
            err,
            Error::RateLimited {
                retry_after_ms: Some(_)
            }
        ),
        "{err}"
    );

    h.server.reset().await;
    mount_viewer(&h.server, "lin_api_1").await;
    Mock::given(method("POST"))
        .and(path(GQL))
        .and(body_string_contains("issues("))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&h.server)
        .await;
    mount_issues(&h.server, None, json!([]), None).await;
    run(&conn, None, &h.sink()).await.unwrap();
}

#[tokio::test]
async fn redirects_are_refused_and_hostile_payloads_are_capped() {
    let h = Harness::new().await;
    h.paste("lin_api_1");
    let elsewhere = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(301).insert_header("location", format!("{}/x", elsewhere.uri())),
        )
        .mount(&h.server)
        .await;
    let conn = linear(&h, json!({}));
    let err = run(&conn, None, &h.sink()).await.unwrap_err();
    assert!(
        matches!(err, Error::Network(ref m) if m.contains("redirect")),
        "{err}"
    );
    assert!(elsewhere.received_requests().await.unwrap().is_empty());

    let h = Harness::new().await;
    h.paste("lin_api_1");
    mount_viewer(&h.server, "lin_api_1").await;
    let mut big = issue("i1", "ENG-1", &ago(10), "bob");
    big["description"] = json!("D".repeat(3_000_000));
    big["title"] = json!(format!("{}\u{0}\u{1b}[31m", "T".repeat(5_000)));
    mount_issues(&h.server, None, json!([big, 7, null, {"id": "i2"}]), None).await;
    let conn = linear(&h, json!({}));
    let sink = h.sink();
    run(&conn, None, &sink).await.unwrap();
    let md = sink.text("issue/i1");
    assert!(md.len() < 200_000, "{}", md.len());
    assert!(!md.contains('\u{1b}') && !md.contains('\u{0}'));
    // A node with no fields is still a document, never a panic.
    assert!(sink.summary().contains(&"+issue/i2".to_string()));
}

#[tokio::test]
async fn oauth_pkce_flow_stores_tokens_and_sync_uses_bearer() {
    let h = Harness::new().await;
    Mock::given(method("POST"))
        .and(path("/linear/token"))
        .and(body_string_contains("code_verifier="))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "lin_oauth_fresh", "refresh_token": "r", "expires_in": 86400, "token_type": "Bearer"
        })))
        .mount(&h.server)
        .await;
    mount_viewer(&h.server, "Bearer lin_oauth_fresh").await;
    mount_issues(&h.server, None, json!([]), None).await;
    let conn = linear(&h, json!({"client_id": "lin-client"}));
    let flow = conn.begin_auth().await.unwrap();
    let url = url::Url::parse(&flow.url).unwrap();
    assert!(url.path().ends_with("/linear/authorize"));
    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(q["client_id"], "lin-client");
    assert_eq!(q["scope"], "read");
    assert_eq!(q["code_challenge_method"], "S256");
    let done = tokio::spawn(flow.done);
    let resp = reqwest::get(format!("{}?state={}&code=c", q["redirect_uri"], q["state"]))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    done.await.unwrap().unwrap();
    run(&conn, None, &h.sink()).await.unwrap();
    h.assert_egress("sync:linear").await;

    let plain = linear(&h, json!({}));
    assert!(matches!(
        plain.begin_auth().await,
        Err(Error::Unsupported(_))
    ));
}
