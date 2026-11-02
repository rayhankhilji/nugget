//! Regression tests for the security and ops fixes (docs/security/cloud-threat-model.md).

use super::*;
use crate::ratelimit::Limits;

/// The harness's parts behind a router with the given limits.
fn app_with(h: &Harness, limits: Limits) -> Router {
    let gateway = Gateway::new(
        Some(h.llm.clone() as Arc<dyn Llm>),
        None,
        "claude-haiku-5-5".into(),
        "claude-sonnet-5-5".into(),
    );
    let plans = Catalog::parse(crate::plans::DEFAULT_TOML, crate::testkit::test_prices).unwrap();
    let mut state = AppState::new(h.db.clone(), h.auth.clone(), gateway, Arc::new(plans));
    state.composio = Some(h.composio.clone() as Arc<dyn Composio>);
    state.auth_configs = [
        ("gmail".to_owned(), "ac_gmail".to_owned()),
        ("googlecalendar".to_owned(), "ac_cal".to_owned()),
    ]
    .into_iter()
    .collect();
    state.webhook_secret = Some("whsec".into());
    router(Arc::new(state.with_limits(limits)))
}

async fn raw(app: &Router, req: Request<Body>) -> axum::response::Response {
    app.clone().oneshot(req).await.unwrap()
}

#[tokio::test]
async fn asks_and_syncs_are_rate_limited_per_user() {
    let Some(h) = harness(false).await else {
        return;
    };
    let app = app_with(
        &h,
        Limits {
            ask_per_min: 2,
            sync_every_secs: 60,
            ..Limits::unlimited()
        },
    );
    let (u, other) = (
        subscriber(&h.pool, "heed").await,
        subscriber(&h.pool, "heed").await,
    );
    let ask = json!({"question": "lease?"});
    for _ in 0..2 {
        let (s, v) = call(&app, "POST", "/v1/ask", Some(u), Some(ask.clone())).await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    let req = Request::post("/v1/ask")
        .header("authorization", format!("Bearer tok-{u}"))
        .header("content-type", "application/json")
        .body(Body::from(ask.to_string()))
        .unwrap();
    let res = raw(&app, req).await;
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry: u64 = res.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=30).contains(&retry), "{retry}");
    let (s, _) = call(&app, "POST", "/v1/ask", Some(other), Some(ask)).await;
    assert_eq!(s, StatusCode::OK, "another user is not limited");

    let (s, _) = call(&app, "POST", "/v1/sync", Some(u), Some(json!({}))).await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = call(&app, "POST", "/v1/sync", Some(u), Some(json!({}))).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{v}");
    assert_eq!(v["error"], "rate_limited");
}

#[tokio::test]
async fn ip_limit_uses_the_proxy_header_and_spares_health_checks() {
    let Some(h) = harness(false).await else {
        return;
    };
    let app = app_with(
        &h,
        Limits {
            ip_per_min: 2,
            ..Limits::unlimited()
        },
    );
    let get = |ip: &str, uri: &str| {
        Request::get(uri)
            .header("fly-client-ip", ip)
            .body(Body::empty())
            .unwrap()
    };
    for _ in 0..2 {
        assert_eq!(
            raw(&app, get("203.0.113.9", "/v1/me")).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        raw(&app, get("203.0.113.9", "/v1/me")).await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "unauthenticated floods are limited before reaching Supabase"
    );
    assert_eq!(
        raw(&app, get("203.0.113.10", "/v1/me")).await.status(),
        StatusCode::UNAUTHORIZED
    );
    for _ in 0..5 {
        assert_eq!(
            raw(&app, get("203.0.113.9", "/healthz")).await.status(),
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn oversized_bodies_are_refused() {
    let Some(h) = harness(false).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    let big = json!({"question": "x".repeat(MAX_BODY_BYTES + 1)}).to_string();
    let req = Request::post("/v1/ask")
        .header("authorization", format!("Bearer tok-{u}"))
        .header("content-type", "application/json")
        .body(Body::from(big))
        .unwrap();
    assert_eq!(
        raw(&h.app, req).await.status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn readiness_and_request_ids() {
    let Some(h) = harness(false).await else {
        return;
    };
    let res = raw(&h.app, Request::get("/readyz").body(Body::empty()).unwrap()).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["x-request-id"].len(), 32, "generated");
    let req = Request::get("/healthz")
        .header("x-request-id", "req-1234567890")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        raw(&h.app, req).await.headers()["x-request-id"],
        "req-1234567890"
    );
    let text = crate::ops::METRICS.render(Some(h.db.pool_status()));
    assert!(
        text.contains("route=\"/readyz\""),
        "metrics use the route template"
    );
}

#[tokio::test]
async fn replayed_webhooks_are_not_acted_on() {
    let Some(h) = harness(false).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    connect_and_sync(&h, u).await;
    let (_, ca) = call(&h.app, "GET", "/v1/connections", Some(u), None).await;
    let acc = ca["connections"][0]["composio_account_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let due = || async {
        h.pool
            .query_one("select bool_or(next_sync_at <= now()) from public.connections where composio_account_id = $1", &[&acc])
            .await
            .unwrap()
            .get::<_, bool>(0)
    };
    let body = json!({"metadata": {"connected_account_id": acc}}).to_string();
    let ts = time::OffsetDateTime::now_utc().unix_timestamp().to_string();
    let sig = crate::webhook::sign("whsec", "msg_replay", &ts, body.as_bytes());
    let send = || {
        let req = Request::post("/v1/webhooks/composio")
            .header("webhook-id", "msg_replay")
            .header("webhook-timestamp", ts.clone())
            .header("webhook-signature", sig.clone())
            .body(Body::from(body.clone()))
            .unwrap();
        h.app.clone().oneshot(req)
    };
    assert_eq!(send().await.unwrap().status(), StatusCode::OK);
    assert!(due().await);
    h.pool
        .execute(
            "update public.connections set next_sync_at = now() + interval '1 hour'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        send().await.unwrap().status(),
        StatusCode::OK,
        "acknowledged"
    );
    assert!(!due().await, "but the replay did nothing");
}

#[tokio::test]
async fn concurrent_scheduler_passes_never_sync_a_connection_twice() {
    let Some(h) = harness(false).await else {
        return;
    };
    for _ in 0..3 {
        let u = subscriber(&h.pool, "heed").await;
        for t in ["gmail", "googlecalendar"] {
            call(
                &h.app,
                "POST",
                "/v1/connections",
                Some(u),
                Some(json!({"toolkit": t})),
            )
            .await;
        }
        call(&h.app, "GET", "/v1/connections", Some(u), None).await;
    }
    let ingest = || Ingest {
        db: h.db.clone(),
        composio: h.composio.clone(),
        gateway: Gateway::new(None, None, String::new(), String::new()),
        catalog: Arc::new(
            Catalog::parse(crate::plans::DEFAULT_TOML, crate::testkit::test_prices).unwrap(),
        ),
        limiter: Default::default(),
        triggers: false,
    };
    let (a, b) = (ingest(), ingest());
    let (na, nb) = tokio::join!(a.run_due(10), b.run_due(10));
    assert_eq!(na + nb, 6, "each of the 6 connections synced exactly once");
    let calls = h.composio.calls.lock().unwrap().len();
    assert_eq!(calls, 6, "one fetch per connection, no duplicates");
}

#[tokio::test]
async fn export_is_limited_to_once_a_minute() {
    let Some(h) = harness(false).await else {
        return;
    };
    let app = app_with(&h, Limits::default());
    let u = subscriber(&h.pool, "heed").await;
    let (s, v) = call(&app, "GET", "/v1/export", Some(u), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) = call(&app, "GET", "/v1/export", Some(u), None).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn an_abandoned_sync_request_still_records_its_result() {
    let Some(h) = harness(false).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    for t in ["gmail", "googlecalendar"] {
        call(
            &h.app,
            "POST",
            "/v1/connections",
            Some(u),
            Some(json!({"toolkit": t})),
        )
        .await;
    }
    call(&h.app, "GET", "/v1/connections", Some(u), None).await;
    let req = Request::post("/v1/sync")
        .header("authorization", format!("Bearer tok-{u}"))
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    // The client (or the request deadline) gives up almost at once.
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(5),
        h.app.clone().oneshot(req),
    )
    .await;
    let synced = || async {
        h.pool
            .query_one(
                "select count(*) from public.connections where user_id = $1 and last_synced_at is not null",
                &[&u],
            )
            .await
            .unwrap()
            .get::<_, i64>(0)
    };
    for _ in 0..100 {
        if synced().await == 2 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the sync was dropped with the request; cursors were never recorded");
}

#[tokio::test]
async fn concurrent_first_requests_from_a_new_device_all_succeed() {
    let Some(h) = harness(false).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let app = h.app.clone();
        set.spawn(async move {
            let req = Request::get("/v1/me")
                .header("authorization", format!("Bearer tok-{u}"))
                .header("x-heed-device", "device-abcdef12")
                .body(Body::empty())
                .unwrap();
            app.oneshot(req).await.unwrap().status()
        });
    }
    while let Some(s) = set.join_next().await {
        assert_eq!(
            s.unwrap(),
            StatusCode::OK,
            "no duplicate-key 500 under concurrency"
        );
    }
    let n: i64 = h
        .pool
        .query_one(
            "select count(*) from public.devices where user_id = $1",
            &[&u],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
}
