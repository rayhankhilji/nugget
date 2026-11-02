//! `POST /v1/waitlist` on real Postgres: one answer for every well-formed body, duplicates,
//! rate limit, body cap, CORS, the table's privacy, and removal with the account.

use super::security::app_with;
use super::*;
use crate::ratelimit::Limits;

async fn post(app: &Router, body: &str, ip: &str) -> (StatusCode, Vec<u8>) {
    let req = Request::post("/v1/waitlist")
        .header("content-type", "application/json")
        .header("fly-client-ip", ip)
        .body(Body::from(body.to_owned()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    (
        status,
        res.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

async fn listed(h: &Harness) -> Vec<(String, String, Option<String>)> {
    h.pool
        .query(
            "select email::text, source, ip_hash from public.waitlist order by email",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect()
}

#[tokio::test]
async fn same_answer_for_new_duplicate_and_invalid_addresses() {
    let Some(h) = harness(false).await else {
        return;
    };
    let app = app_with(&h, Limits::unlimited());
    let mut answers = Vec::new();
    for body in [
        r#"{"email": "  Ada@Example.com "}"#,
        r#"{"email": "ADA@example.COM"}"#,
        r#"{"email": "not-an-address"}"#,
        r#"{"email": "bob@example.org", "source": "pricing"}"#,
        r#"{"email": 42}"#,
        r#"{}"#,
        r#"{"email": "x\u0007@example.com"}"#,
    ] {
        answers.push(post(&app, body, "203.0.113.9").await);
    }
    assert!(
        answers
            .iter()
            .all(|a| *a == (StatusCode::ACCEPTED, Vec::new())),
        "{answers:?}"
    );
    let rows = listed(&h).await;
    assert_eq!(
        rows.len(),
        2,
        "duplicates (any case) and invalid addresses are not stored: {rows:?}"
    );
    assert_eq!(
        (rows[0].0.as_str(), rows[0].1.as_str()),
        ("ada@example.com", "site")
    );
    assert_eq!(
        (rows[1].0.as_str(), rows[1].1.as_str()),
        ("bob@example.org", "pricing")
    );
    let hash = rows[0].2.as_deref().unwrap();
    assert_eq!(
        hash,
        waitlist_ip_hash("203.0.113.9"),
        "salted hash of the IP, never the IP"
    );
    // Only malformed JSON is told apart.
    assert_eq!(
        post(&app, "{email:", "203.0.113.9").await.0,
        StatusCode::BAD_REQUEST
    );
    // The body is capped at 1 KiB.
    let big = format!(r#"{{"email": "a@b.co", "pad": "{}"}}"#, "x".repeat(2000));
    assert_eq!(
        post(&app, &big, "203.0.113.9").await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

fn waitlist_ip_hash(ip: &str) -> String {
    crate::api::waitlist::ip_hash("test-salt", ip.parse().unwrap())
}

#[tokio::test]
async fn rate_limited_per_ip() {
    let Some(h) = harness(false).await else {
        return;
    };
    let app = app_with(
        &h,
        Limits {
            waitlist_per_hour: 5,
            ..Limits::unlimited()
        },
    );
    for i in 0..5 {
        let (s, _) = post(
            &app,
            &format!(r#"{{"email": "u{i}@example.com"}}"#),
            "198.51.100.1",
        )
        .await;
        assert_eq!(s, StatusCode::ACCEPTED);
    }
    let (s, _) = post(&app, r#"{"email": "u6@example.com"}"#, "198.51.100.1").await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    let (s, _) = post(&app, r#"{"email": "u7@example.com"}"#, "198.51.100.2").await;
    assert_eq!(s, StatusCode::ACCEPTED, "another IP has its own allowance");
    assert_eq!(listed(&h).await.len(), 6);
}

#[tokio::test]
async fn cors_for_the_site_only() {
    let Some(h) = harness(false).await else {
        return;
    };
    let pre = |origin: &'static str| {
        Request::builder()
            .method("OPTIONS")
            .uri("/v1/waitlist")
            .header("origin", origin)
            .header("access-control-request-method", "POST")
            .header("access-control-request-headers", "content-type")
            .body(Body::empty())
            .unwrap()
    };
    let res = h
        .app
        .clone()
        .oneshot(pre("https://heed.test"))
        .await
        .unwrap();
    assert_eq!(
        res.headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("https://heed.test")
    );
    let res = h
        .app
        .clone()
        .oneshot(pre("https://evil.test"))
        .await
        .unwrap();
    assert!(res.headers().get("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn api_roles_cannot_read_and_service_cannot_read_back() {
    let Some(h) = harness(false).await else {
        return;
    };
    let app = app_with(&h, Limits::unlimited());
    post(&app, r#"{"email": "ada@example.com"}"#, "203.0.113.9").await;
    for role in ["anon", "authenticated", "nugget_service"] {
        let err = h
            .pool
            .batch_execute(&format!(
                "begin; set local role {role}; select * from public.waitlist; rollback;"
            ))
            .await
            .expect_err(role);
        h.pool.batch_execute("rollback").await.ok();
        let msg = err
            .as_db_error()
            .map(|e| e.message().to_owned())
            .unwrap_or_default();
        assert!(msg.contains("permission denied"), "{role}: {err:?}");
    }
}

#[tokio::test]
async fn deleting_an_account_removes_its_waitlist_entry() {
    let Some(h) = harness(false).await else {
        return;
    };
    let app = app_with(&h, Limits::unlimited());
    let u = add_user(&h.pool).await;
    // FakeAuth gives each user the address <id>@example.com.
    post(
        &app,
        &format!(r#"{{"email": "{u}@example.com"}}"#),
        "203.0.113.9",
    )
    .await;
    post(
        &app,
        r#"{"email": "someone.else@example.com"}"#,
        "203.0.113.9",
    )
    .await;
    assert_eq!(listed(&h).await.len(), 2);
    let (s, _) = call(&app, "DELETE", "/v1/me", Some(u), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let rows = listed(&h).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "someone.else@example.com");
}
