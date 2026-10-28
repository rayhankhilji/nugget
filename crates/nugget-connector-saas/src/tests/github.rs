use std::time::Duration;

use serde_json::json;
use wiremock::matchers::{body_partial_json, body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::GitHubFactory;
use crate::text::{now_unix, rfc3339};

const GQL: &str = "/github/graphql";

fn factory(h: &Harness) -> GitHubFactory {
    GitHubFactory::new()
        .with_endpoints(h.endpoints())
        .with_retry(fast_retry(2))
        .with_device_poll_interval(Duration::from_millis(10))
}

fn github(h: &Harness, settings: Value) -> Arc<dyn Connector> {
    h.build(&factory(h), settings)
}

fn ago(secs: i64) -> String {
    rfc3339(now_unix() - secs).unwrap()
}

fn data(v: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"data": v}))
}

async fn mount_viewer(server: &MockServer, token: &str) {
    Mock::given(method("POST"))
        .and(path(GQL))
        .and(header("authorization", format!("Bearer {token}").as_str()))
        .and(body_string_contains("viewer { login }"))
        .respond_with(data(json!({"viewer": {"login": "ada"}})))
        .mount(server)
        .await;
}

fn page(nodes: Value, next: Option<&str>) -> ResponseTemplate {
    data(json!({
        "rateLimit": {"remaining": 4000, "resetAt": ago(-3600)},
        "search": {"pageInfo": {"hasNextPage": next.is_some(), "endCursor": next}, "nodes": nodes}
    }))
}

/// A full search page for `ty` (`first: 20`), optionally after `after`.
async fn mount_search(
    server: &MockServer,
    ty: &str,
    after: Option<&str>,
    nodes: Value,
    next: Option<&str>,
) {
    let vars = match after {
        Some(a) => json!({"variables": {"type": ty, "first": 20, "after": a}}),
        None => json!({"variables": {"type": ty, "first": 20, "after": null}}),
    };
    Mock::given(method("POST"))
        .and(path(GQL))
        .and(body_partial_json(vars))
        .respond_with(page(nodes, next))
        .mount(server)
        .await;
}

fn issue(repo: &str, n: i64, updated: &str, author: &str) -> Value {
    json!({
        "__typename": "Issue", "id": format!("I_{n}"), "number": n, "title": format!("Issue {n}"),
        "url": format!("https://github.com/{repo}/issues/{n}"), "state": "OPEN", "body": "It breaks.",
        "createdAt": ago(90_000), "updatedAt": updated, "author": {"login": author},
        "repository": {"nameWithOwner": repo},
        "labels": {"nodes": [{"name": "bug"}]}, "assignees": {"nodes": [{"login": "ada"}]},
        "comments": {"totalCount": 0, "pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": []}
    })
}

#[tokio::test]
async fn sync_without_token_is_unauthorized() {
    let h = Harness::new().await;
    let conn = github(&h, json!({}));
    assert!(is_unauthorized(&run(&conn, None, &h.sink()).await));
    assert!(h.server.received_requests().await.unwrap().is_empty());

    h.paste("github_pat_bad");
    Mock::given(method("POST"))
        .and(path(GQL))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"message": "Bad credentials"})),
        )
        .mount(&h.server)
        .await;
    let r = run(&conn, None, &h.sink()).await;
    assert!(is_unauthorized(&r), "{r:?}");
}

#[tokio::test]
async fn full_sync_threads_comments_reviews_and_discussions() {
    let h = Harness::new().await;
    h.paste("github_pat_1");
    mount_viewer(&h.server, "github_pat_1").await;
    let t1 = ago(7200);
    let t2 = ago(3600);
    let mut i = issue("o/r", 1, &t1, "bob");
    i["comments"] = json!({"totalCount": 2, "pageInfo": {"hasNextPage": true, "endCursor": "c1"},
        "nodes": [{"author": {"login": "ada"}, "body": "first!", "createdAt": ago(80_000)}]});
    let pr = json!({
        "__typename": "PullRequest", "id": "PR_2", "number": 2, "title": "Fix it", "isDraft": false, "merged": true,
        "url": "https://github.com/o/r/pull/2", "state": "MERGED", "body": "Fixes #1", "createdAt": ago(70_000),
        "updatedAt": t2, "author": {"login": "ada"}, "repository": {"nameWithOwner": "o/r"},
        "labels": {"nodes": []}, "assignees": {"nodes": []},
        "comments": {"totalCount": 0, "pageInfo": {"hasNextPage": false}, "nodes": []},
        "reviews": {"nodes": [{"author": {"login": "bob"}, "state": "APPROVED", "body": "LGTM", "submittedAt": ago(60_000),
            "comments": {"nodes": [{"author": {"login": "bob"}, "body": "nit", "path": "src/lib.rs"}]}}]}
    });
    mount_search(
        &h.server,
        "ISSUE",
        None,
        json!([i, {"__typename": "Issue", "number": 5}]),
        Some("p2"),
    )
    .await;
    mount_search(&h.server, "ISSUE", Some("p2"), json!([pr]), None).await;
    Mock::given(method("POST"))
        .and(path(GQL))
        .and(body_partial_json(
            json!({"variables": {"id": "I_1", "after": "c1"}}),
        ))
        .respond_with(data(
            json!({"node": {"comments": {"pageInfo": {"hasNextPage": false},
            "nodes": [{"author": {"login": "bob"}, "body": "second", "createdAt": ago(75_000)}]}}}),
        ))
        .expect(1)
        .mount(&h.server)
        .await;
    let disc = json!({
        "__typename": "Discussion", "id": "D_3", "number": 3, "title": "Ideas", "url": "https://github.com/o/r/discussions/3",
        "body": "What next?", "createdAt": ago(50_000), "updatedAt": t2, "author": {"login": "carol"},
        "repository": {"nameWithOwner": "o/r"}, "category": {"name": "Q&A"}, "answer": null, "labels": {"nodes": []},
        "comments": {"totalCount": 1, "nodes": [{"id": "DC1", "author": {"login": "ada"}, "body": "More tests", "createdAt": ago(40_000),
            "replies": {"nodes": [{"author": {"login": "carol"}, "body": "agreed", "createdAt": ago(30_000)}]}}]}
    });
    mount_search(&h.server, "DISCUSSION", None, json!([disc]), None).await;
    h.know("issue/o/r/99", "old");

    let conn = github(&h, json!({"window_days": 30}));
    let sink = h.sink();
    let cursor = run(&conn, None, &sink).await.unwrap();
    assert_eq!(
        sink.summary(),
        vec![
            "+issue/o/r/1",
            "+issue/o/r/2",
            "+discussion/o/r/3",
            "-issue/o/r/99"
        ]
    );
    let md = sink.text("issue/o/r/1");
    assert!(
        md.starts_with("# Issue 1\n\nIssue o/r#1 · OPEN · opened by @bob"),
        "{md}"
    );
    assert!(
        md.contains("Labels: bug") && md.contains("Assignees: @ada") && md.contains("It breaks.")
    );
    assert!(
        md.find("first!").unwrap() < md.find("second").unwrap(),
        "comments in order"
    );
    let doc = sink.doc("issue/o/r/1");
    assert_eq!(doc.version.as_deref(), Some(t1.as_str()));
    assert_eq!(doc.url.as_deref(), Some("https://github.com/o/r/issues/1"));
    assert_eq!(doc.metadata["from_me"], json!(false));
    assert_eq!(doc.metadata["my_message_count"], json!(1));
    assert_eq!(doc.metadata["labels"], json!(["bug"]));
    assert_eq!(doc.metadata["me"], json!("ada"));

    let pr = sink.text("issue/o/r/2");
    assert!(pr.contains("Pull request o/r#2 · MERGED"), "{pr}");
    assert!(
        pr.contains("**@bob** reviewed (APPROVED)") && pr.contains("> `src/lib.rs` (@bob): nit")
    );
    assert_eq!(
        sink.doc("issue/o/r/2").metadata["type"],
        json!("pull_request")
    );
    assert_eq!(sink.doc("issue/o/r/2").metadata["from_me"], json!(true));

    let d = sink.text("discussion/o/r/3");
    assert!(
        d.contains("Category: Q&A") && d.contains("> **@carol** replied"),
        "{d}"
    );

    let scopes = cursor["scopes"].as_object().unwrap();
    assert_eq!(
        scopes["issue|involves:ada"]["since"].as_i64(),
        crate::text::parse_rfc3339(&t2).map(|t| t.unix_timestamp())
    );
    assert!(scopes.contains_key("discussion|involves:ada"));
    assert!(cursor["reconciled_at"].as_i64().unwrap() > 0);

    let reqs = h.server.received_requests().await.unwrap();
    let q = reqs
        .iter()
        .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
        .find_map(|b| b["variables"]["q"].as_str().map(str::to_string))
        .unwrap();
    assert!(
        q.starts_with("involves:ada updated:>=") && q.ends_with(" sort:updated-asc"),
        "{q}"
    );
    // One checkpoint per search page.
    assert!(sink.checkpoints.lock().unwrap().len() >= 3);
    h.assert_egress("sync:github").await;
}

#[tokio::test]
async fn incremental_skips_unchanged_items_and_uses_repo_scopes() {
    let h = Harness::new().await;
    h.paste("github_pat_1");
    mount_viewer(&h.server, "github_pat_1").await;
    let t = ago(3600);
    h.know("issue/o/r/1", &t);
    mount_search(
        &h.server,
        "ISSUE",
        None,
        json!([issue("o/r", 1, &t, "bob"), issue("o/r", 4, &t, "ada")]),
        None,
    )
    .await;
    let since = now_unix() - 86_400;
    let cursor = json!({"v": 1, "viewer": "ada", "window_from": since - 1000, "reconciled_at": now_unix(),
        "scopes": {"issue|repo:o/r": {"since": since}, "issue|involves:ada": {"since": 5}}});
    let conn = github(
        &h,
        json!({"repos": ["o/r"], "all_activity": true, "include_discussions": false}),
    );
    let sink = h.sink();
    let next = run(&conn, Some(cursor), &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+issue/o/r/4"]);
    assert_eq!(sink.doc("issue/o/r/4").metadata["from_me"], json!(true));
    // The old scope is dropped from the cursor; no reconcile was due.
    let scopes: Vec<&String> = next["scopes"].as_object().unwrap().keys().collect();
    assert_eq!(scopes, vec!["issue|repo:o/r"]);
    let reqs = h.server.received_requests().await.unwrap();
    let q = reqs
        .iter()
        .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
        .find_map(|b| b["variables"]["q"].as_str().map(str::to_string))
        .unwrap();
    assert_eq!(
        q,
        format!(
            "repo:o/r updated:>={} sort:updated-asc",
            rfc3339(since).unwrap()
        )
    );
}

#[tokio::test]
async fn periodic_reconcile_deletes_items_that_left_the_scope() {
    let h = Harness::new().await;
    h.paste("github_pat_1");
    mount_viewer(&h.server, "github_pat_1").await;
    let t = ago(3600);
    h.know("issue/o/r/1", &t);
    h.know("issue/o/r/2", &t);
    h.know("discussion/o/r/3", &t);
    Mock::given(method("POST"))
        .and(path(GQL))
        .and(body_partial_json(json!({"variables": {"type": "ISSUE", "first": 100}})))
        .respond_with(page(json!([{"__typename": "Issue", "number": 1, "updatedAt": t, "repository": {"nameWithOwner": "o/r"}}]), None))
        .expect(1)
        .mount(&h.server)
        .await;
    Mock::given(method("POST"))
        .and(path(GQL))
        .and(body_partial_json(
            json!({"variables": {"type": "DISCUSSION", "first": 100}}),
        ))
        .respond_with(page(json!([]), None))
        .mount(&h.server)
        .await;
    mount_search(&h.server, "ISSUE", None, json!([]), None).await;
    mount_search(&h.server, "DISCUSSION", None, json!([]), None).await;
    let cursor = json!({"v": 1, "viewer": "ada", "window_from": now_unix() - 86_400 * 30, "reconciled_at": 1,
        "scopes": {"issue|involves:ada": {"since": now_unix() - 60}}});
    let conn = github(&h, json!({}));
    let sink = h.sink();
    let next = run(&conn, Some(cursor), &sink).await.unwrap();
    let mut s = sink.summary();
    s.sort();
    assert_eq!(s, vec!["-discussion/o/r/3", "-issue/o/r/2"]);
    assert!(next["reconciled_at"].as_i64().unwrap() > 1);
}

#[tokio::test]
async fn rate_limits_primary_secondary_and_graphql() {
    let h = Harness::new().await;
    h.paste("github_pat_1");
    mount_viewer(&h.server, "github_pat_1").await;
    // Secondary rate limit with a short Retry-After, then a GraphQL RATE_LIMITED error: retried.
    Mock::given(method("POST"))
        .and(path(GQL))
        .and(body_string_contains("search("))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("retry-after", "0")
                .set_body_json(json!({"message": "You have exceeded a secondary rate limit."})),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&h.server)
        .await;
    Mock::given(method("POST"))
        .and(path(GQL))
        .and(body_string_contains("search("))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("retry-after", "0")
                .set_body_json(json!({"errors": [{"type": "RATE_LIMITED", "message": "API rate limit exceeded"}]})),
        )
        .up_to_n_times(1)
        .with_priority(2)
        .mount(&h.server)
        .await;
    mount_search(&h.server, "ISSUE", None, json!([]), None).await;
    let conn = github(&h, json!({"include_discussions": false}));
    run(&conn, None, &h.sink()).await.unwrap();

    // Primary limit exhausted for an hour: returned to the supervisor at once, with the wait.
    h.server.reset().await;
    mount_viewer(&h.server, "github_pat_1").await;
    let reset = now_unix() + 3600;
    Mock::given(method("POST"))
        .and(path(GQL))
        .and(body_string_contains("search("))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset", reset.to_string().as_str())
                .set_body_json(json!({"message": "API rate limit exceeded"})),
        )
        .expect(1)
        .mount(&h.server)
        .await;
    let err = run(&conn, None, &h.sink()).await.unwrap_err();
    match err {
        Error::RateLimited {
            retry_after_ms: Some(ms),
        } => assert!(ms > 3_500_000 && ms <= 3_602_000, "{ms}"),
        other => panic!("{other}"),
    }

    // A plain 403 is an authorization problem.
    h.server.reset().await;
    mount_viewer(&h.server, "github_pat_1").await;
    Mock::given(method("POST"))
        .and(path(GQL))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(
                json!({"message": "Resource not accessible by personal access token"}),
            ),
        )
        .mount(&h.server)
        .await;
    assert!(is_unauthorized(&run(&conn, None, &h.sink()).await));
}

#[tokio::test]
async fn low_remaining_points_stop_the_pass_after_checkpointing() {
    let h = Harness::new().await;
    h.paste("github_pat_1");
    mount_viewer(&h.server, "github_pat_1").await;
    Mock::given(method("POST"))
        .and(path(GQL))
        .and(body_string_contains("search("))
        .respond_with(data(
            json!({"rateLimit": {"remaining": 3, "resetAt": ago(-600)},
            "search": {"pageInfo": {"hasNextPage": false}, "nodes": []}}),
        ))
        .mount(&h.server)
        .await;
    let conn = github(&h, json!({}));
    let err = run(&conn, None, &h.sink()).await.unwrap_err();
    assert!(
        matches!(err, Error::RateLimited { retry_after_ms: Some(ms) } if ms > 500_000),
        "{err}"
    );
}

#[tokio::test]
async fn redirects_are_not_followed_and_hostile_payloads_are_capped() {
    let h = Harness::new().await;
    h.paste("github_pat_1");
    let elsewhere = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(307).insert_header("location", format!("{}/x", elsewhere.uri())),
        )
        .mount(&h.server)
        .await;
    let conn = github(&h, json!({}));
    let err = run(&conn, None, &h.sink()).await.unwrap_err();
    assert!(
        matches!(err, Error::Network(ref m) if m.contains("redirect")),
        "{err}"
    );
    assert!(elsewhere.received_requests().await.unwrap().is_empty());

    let h = Harness::new().await;
    h.paste("github_pat_1");
    mount_viewer(&h.server, "github_pat_1").await;
    let mut big = issue("o/r", 7, &ago(100), "bob");
    big["body"] = json!("B".repeat(2_000_000));
    big["comments"]["nodes"] = Value::Array(
        (0..400)
            .map(|i| json!({"author": null, "body": format!("c{i}"), "createdAt": ago(50)}))
            .collect(),
    );
    let evil = json!({"__typename": "Issue", "number": 8, "repository": {"nameWithOwner": "../../etc"}, "updatedAt": ago(1)});
    mount_search(&h.server, "ISSUE", None, json!([big, evil, 42, null]), None).await;
    mount_search(&h.server, "DISCUSSION", None, json!([]), None).await;
    let conn = github(&h, json!({}));
    let sink = h.sink();
    run(&conn, None, &sink).await.unwrap();
    assert_eq!(sink.summary(), vec!["+issue/o/r/7"]);
    let md = sink.text("issue/o/r/7");
    assert!(md.len() < 500_000 * 4, "{}", md.len());
    assert!(md.contains("**@ghost** commented"));
    assert!(!md.contains("c399"), "comments are capped");

    h.server.reset().await;
    mount_viewer(&h.server, "github_pat_1").await;
    Mock::given(method("POST"))
        .and(path(GQL))
        .and(body_string_contains("search("))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
        .mount(&h.server)
        .await;
    let err = run(&conn, None, &h.sink()).await.unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err}");
}

#[tokio::test]
async fn device_flow_stores_tokens_and_sync_uses_them() {
    let h = Harness::new().await;
    Mock::given(method("POST"))
        .and(path("/github/login/device/code"))
        .and(body_string_contains("client_id=Iv1.abc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dc", "user_code": "WDJB-MJHT", "verification_uri": "https://github.com/login/device",
            "expires_in": 900, "interval": 0
        })))
        .mount(&h.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/github/login/oauth/access_token"))
        .and(body_string_contains("device_code=dc"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"error": "authorization_pending"})),
        )
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&h.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/github/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "gho_fresh", "token_type": "bearer", "scope": "repo"
        })))
        .mount(&h.server)
        .await;
    mount_viewer(&h.server, "gho_fresh").await;
    mount_search(&h.server, "ISSUE", None, json!([]), None).await;
    mount_search(&h.server, "DISCUSSION", None, json!([]), None).await;

    let conn = github(&h, json!({"client_id": "Iv1.abc"}));
    assert!(is_unauthorized(&run(&conn, None, &h.sink()).await));
    let flow = conn.begin_auth().await.unwrap();
    assert_eq!(
        flow.url,
        "https://github.com/login/device?user_code=WDJB-MJHT"
    );
    flow.done.await.unwrap();
    let stored = h
        .secrets
        .get(&nugget_core::secrets::oauth_account(SOURCE))
        .unwrap()
        .unwrap();
    assert!(stored.contains("gho_fresh"));
    run(&conn, None, &h.sink()).await.unwrap();
    h.assert_egress("sync:github").await;

    // Without a client id there is no interactive flow.
    let plain = github(&h, json!({}));
    assert!(matches!(
        plain.begin_auth().await,
        Err(Error::Unsupported(_))
    ));
}

#[tokio::test]
async fn denied_device_flow_is_unauthorized() {
    let h = Harness::new().await;
    Mock::given(method("POST"))
        .and(path("/github/login/device/code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dc", "user_code": "X", "verification_uri": "https://github.com/login/device", "interval": 0
        })))
        .mount(&h.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/github/login/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"error": "access_denied"})))
        .mount(&h.server)
        .await;
    let conn = github(&h, json!({"client_id": "Iv1.abc"}));
    let flow = conn.begin_auth().await.unwrap();
    assert!(matches!(flow.done.await, Err(Error::Unauthorized(_))));
    assert!(
        h.secrets
            .get(&nugget_core::secrets::oauth_account(SOURCE))
            .unwrap()
            .is_none()
    );
}

/// The verification URL is opened for the user to type the code into: a hostile device/code
/// answer must not turn it into a look-alike page on another host.
#[tokio::test]
async fn device_flow_refuses_foreign_verification_urls() {
    for uri in [
        "https://github.com.evil.example/login/device",
        "https://evil.example/login/device",
        "https://user:pw@github.com/login/device",
        "javascript:alert(1)",
    ] {
        let h = Harness::new().await;
        Mock::given(method("POST"))
            .and(path("/github/login/device/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "dc", "user_code": "X", "verification_uri": uri, "interval": 0
            })))
            .mount(&h.server)
            .await;
        let conn = github(&h, json!({"client_id": "Iv1.abc"}));
        let r = conn.begin_auth().await;
        assert!(matches!(r, Err(Error::Invalid(_))), "{uri} accepted");
    }
}
