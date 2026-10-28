use serde_json::json;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, ResponseTemplate};

use super::*;
use nugget_connector::{AuthKind, Registry};

use crate::{GitHubFactory, LinearFactory, NotionFactory, SlackFactory};

#[test]
fn registers_four_token_kinds_with_closed_schemas() {
    let mut r = Registry::new();
    crate::register(&mut r);
    let kinds = r.kinds();
    let names: Vec<_> = kinds.iter().map(|k| k.kind).collect();
    assert_eq!(names, vec!["github", "linear", "notion", "slack"]);
    for k in &kinds {
        assert_eq!(k.auth, AuthKind::Password, "{}", k.kind);
        assert_eq!(k.settings_schema["additionalProperties"], json!(false));
        assert!(k.settings_schema["properties"]["token_env"].is_object());
    }
    assert!(kinds[0].settings_schema["properties"]["repos"].is_object());
    assert!(kinds[1].settings_schema["properties"]["include"].is_object());
    assert!(kinds[2].settings_schema["properties"]["max_depth"].is_object());
    assert!(kinds[3].settings_schema["properties"]["conversations"].is_object());
}

#[test]
fn validates_settings() {
    let s = SlackFactory::new();
    assert!(s.validate(&Value::Null).is_ok());
    assert!(s.validate(&json!({"conversations": ["#general", "C0123456789"], "window_days": 30, "recheck_days": 0})).is_ok());
    assert!(
        s.validate(&json!({"include_dms": false, "include_channels": false}))
            .is_err()
    );
    assert!(
        s.validate(
            &json!({"include_dms": false, "include_channels": false, "conversations": ["x"]})
        )
        .is_ok()
    );
    assert!(s.validate(&json!({"max_conversations": 0})).is_err());
    assert!(s.validate(&json!({"recheck_days": 31})).is_err());
    assert!(s.validate(&json!({"conversations": [""]})).is_err());
    assert!(
        s.validate(&json!({"token": "xoxp-in-settings"})).is_err(),
        "tokens never go in settings"
    );
    assert!(s.validate(&json!({"token_env": "SLACK_TOKEN"})).is_ok());
    assert!(s.validate(&json!({"token_env": "1BAD"})).is_err());
    assert!(s.validate(&json!({"token_env": "A-B"})).is_err());

    let n = NotionFactory::new();
    assert!(
        n.validate(&json!({"max_pages": 10, "max_depth": 12, "include_databases": false}))
            .is_ok()
    );
    assert!(n.validate(&json!({"max_depth": 13})).is_err());
    assert!(n.validate(&json!({"max_pages": 0})).is_err());

    let g = GitHubFactory::new();
    assert!(
        g.validate(&json!({"repos": ["o/r", "my.org/re-po_1"], "all_activity": true}))
            .is_ok()
    );
    assert!(g.validate(&json!({"all_activity": true})).is_err());
    assert!(g.validate(&json!({"repos": ["noslash"]})).is_err());
    assert!(
        g.validate(&json!({"repos": ["o/r involves:x"]})).is_err(),
        "no query injection"
    );
    assert!(g.validate(&json!({"repos": ["../.."]})).is_err());
    assert!(g.validate(&json!({"client_id": "a b"})).is_err());
    assert!(
        g.validate(&json!({"client_id": "Iv1.x", "window_days": 1}))
            .is_ok()
    );

    let l = LinearFactory::new();
    assert!(
        l.validate(&json!({"include": ["created"], "teams": ["ENG", "OPS_2"]}))
            .is_ok()
    );
    assert!(l.validate(&json!({"include": []})).is_err());
    assert!(l.validate(&json!({"include": ["everything"]})).is_err());
    assert!(l.validate(&json!({"teams": ["ENG\"}"]})).is_err());
}

#[tokio::test]
async fn kinds_and_poll_intervals() {
    let h = Harness::new().await;
    for (f, kind, secs) in [
        (
            Box::new(SlackFactory::new()) as Box<dyn ConnectorFactory>,
            "slack",
            300,
        ),
        (Box::new(NotionFactory::new()), "notion", 300),
        (Box::new(GitHubFactory::new()), "github", 600),
        (Box::new(LinearFactory::new()), "linear", 300),
    ] {
        let c = h.build(f.as_ref(), json!({}));
        assert_eq!(c.kind(), kind);
        assert_eq!(c.poll_interval(), Duration::from_secs(secs));
        assert!(!c.supports_watch());
    }
    let s = h.build(&SlackFactory::new(), json!({}));
    assert!(matches!(s.begin_auth().await, Err(Error::Unsupported(_))));
}

#[tokio::test]
async fn a_missing_token_env_variable_is_unauthorized_even_with_a_keychain_token() {
    let h = Harness::new().await;
    h.paste("ntn_keychain");
    let conn = h.build(
        &NotionFactory::new().with_endpoints(h.endpoints()),
        json!({"token_env": "NUGGET_SAAS_TEST_UNSET_TOKEN"}),
    );
    let r = run(&conn, None, &h.sink()).await;
    assert!(
        matches!(r, Err(Error::Unauthorized(ref m)) if m.contains("NUGGET_SAAS_TEST_UNSET_TOKEN")),
        "{r:?}"
    );
    assert!(h.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn token_env_reads_an_existing_variable() {
    // CARGO_PKG_NAME is set by cargo for every test binary: use it as the "token".
    let h = Harness::new().await;
    Mock::given(method("GET"))
        .and(path("/notion/v1/users/me"))
        .and(header("authorization", "Bearer nugget-connector-saas"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"bot": {}})))
        .expect(1)
        .mount(&h.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/notion/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
        .mount(&h.server)
        .await;
    let conn = h.build(
        &NotionFactory::new().with_endpoints(h.endpoints()),
        json!({"token_env": "CARGO_PKG_NAME"}),
    );
    run(&conn, None, &h.sink()).await.unwrap();
}

#[tokio::test]
async fn oauth_tokens_win_over_a_pasted_token_and_401_refreshes_once() {
    let h = Harness::new().await;
    h.paste("lin_api_pasted");
    h.seed_oauth("lin_oauth_old", false);
    Mock::given(method("POST"))
        .and(path("/linear/graphql"))
        .and(header("authorization", "Bearer lin_oauth_old"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "errors": [{"message": "expired", "extensions": {"code": "AUTHENTICATION_ERROR"}}]
        })))
        .expect(1)
        .mount(&h.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/linear/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "lin_oauth_new", "expires_in": 3600})),
        )
        .expect(1)
        .mount(&h.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/linear/graphql"))
        .and(header("authorization", "Bearer lin_oauth_new"))
        .and(body_string_contains("viewer {"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data": {"viewer": {"id": "me", "name": "Ada"}}})),
        )
        .mount(&h.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/linear/graphql"))
        .and(header("authorization", "Bearer lin_oauth_new"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"data": {"issues": {"pageInfo": {"hasNextPage": false}, "nodes": []}}}),
        ))
        .mount(&h.server)
        .await;
    let conn = h.build(
        &LinearFactory::new().with_endpoints(h.endpoints()),
        json!({"client_id": "lin-client"}),
    );
    run(&conn, None, &h.sink()).await.unwrap();
    let stored = h
        .secrets
        .get(&nugget_core::secrets::oauth_account(SOURCE))
        .unwrap()
        .unwrap();
    assert!(stored.contains("lin_oauth_new") && stored.contains("\"refresh_token\":\"rt\""));
    h.assert_egress("sync:linear").await;
}

#[tokio::test]
async fn expired_oauth_tokens_without_a_client_are_unauthorized() {
    let h = Harness::new().await;
    h.seed_oauth("gho_old", true);
    let conn = h.build(
        &GitHubFactory::new().with_endpoints(h.endpoints()),
        json!({}),
    );
    assert!(is_unauthorized(&run(&conn, None, &h.sink()).await));
    assert!(h.server.received_requests().await.unwrap().is_empty());

    // Unreadable stored tokens are an authorization problem too, never a panic.
    h.secrets
        .set(&nugget_core::secrets::oauth_account(SOURCE), "{garbage")
        .unwrap();
    assert!(is_unauthorized(&run(&conn, None, &h.sink()).await));
}

#[tokio::test]
async fn a_pasted_token_rejected_with_401_is_unauthorized_without_retries() {
    let h = Harness::new().await;
    h.paste("github_pat_x");
    Mock::given(method("POST"))
        .and(path("/github/graphql"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&h.server)
        .await;
    let conn = h.build(
        &GitHubFactory::new().with_endpoints(h.endpoints()),
        json!({}),
    );
    assert!(is_unauthorized(&run(&conn, None, &h.sink()).await));
}

#[tokio::test]
async fn cancellation_is_honoured() {
    let h = Harness::new().await;
    h.paste("xoxp-1");
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(30))
                .set_body_json(json!({"ok": true})),
        )
        .mount(&h.server)
        .await;
    let conn = h.build(
        &SlackFactory::new().with_endpoints(h.endpoints()),
        json!({}),
    );
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        c2.cancel();
    });
    let started = std::time::Instant::now();
    let r = conn.sync(None, &h.sink(), &cancel).await;
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn unreadable_cursors_start_over() {
    let h = Harness::new().await;
    h.paste("lin_api_1");
    Mock::given(method("POST"))
        .and(path("/linear/graphql"))
        .and(body_string_contains("viewer {"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"data": {"viewer": {"id": "me"}}})),
        )
        .mount(&h.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/linear/graphql"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"data": {"issues": {"nodes": []}}})),
        )
        .mount(&h.server)
        .await;
    let conn = h.build(
        &LinearFactory::new().with_endpoints(h.endpoints()),
        json!({}),
    );
    let next = run(&conn, Some(json!({"v": "garbage", "pass": 5})), &h.sink())
        .await
        .unwrap();
    assert_eq!(next["viewer"], json!("me"));
    assert!(next["window_from"].as_i64().unwrap() > 0);
}
