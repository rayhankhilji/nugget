//! End-to-end M1 flow over the router with test doubles and a real Postgres (see testkit).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use super::*;
use crate::gateway::{Embedder, Llm};
use crate::plans::Catalog;
use crate::testkit::{
    FakeComposio, FakeEmbedder, FakeLlm, FakeStripe, add_user, fakes, subscriber, test_db,
    test_prices,
};

mod billing;

mod security;

struct Harness {
    app: Router,
    db: crate::db::Db,
    pool: crate::testkit::Pg,
    composio: Arc<FakeComposio>,
    llm: Arc<FakeLlm>,
    auth: Arc<crate::testkit::FakeAuth>,
    stripe: Arc<FakeStripe>,
    embedder: Arc<FakeEmbedder>,
}

mod actions;

async fn harness(embeddings: bool) -> Option<Harness> {
    harness_with(
        embeddings,
        &[("gmail", "ac_gmail"), ("googlecalendar", "ac_cal")],
    )
    .await
}

async fn harness_with(embeddings: bool, auth_configs: &[(&str, &str)]) -> Option<Harness> {
    harness_opts(embeddings, auth_configs, std::time::Duration::from_secs(20)).await
}

async fn harness_opts(
    embeddings: bool,
    auth_configs: &[(&str, &str)],
    sync_wait: std::time::Duration,
) -> Option<Harness> {
    let (db, pool) = test_db().await?;
    let (auth, composio, llm) = fakes();
    let fake_embedder = Arc::new(FakeEmbedder::default());
    let embedder = embeddings.then(|| fake_embedder.clone() as Arc<dyn Embedder>);
    let gateway = Gateway::new(
        Some(llm.clone() as Arc<dyn Llm>),
        embedder,
        "claude-haiku-5-5".into(),
        "claude-sonnet-5-5".into(),
    );
    let configs = auth_configs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    let plans = Catalog::parse(crate::plans::DEFAULT_TOML, test_prices).unwrap();
    let stripe = Arc::new(FakeStripe::default());
    let mut state = AppState::new(db.clone(), auth.clone(), gateway, Arc::new(plans));
    state.composio = Some(composio.clone() as Arc<dyn Composio>);
    state.auth_configs = configs;
    state.webhook_secret = Some("whsec".into());
    state.stripe = Some(stripe.clone() as Arc<dyn crate::stripe::Stripe>);
    state.stripe_webhook_secret = Some("whsec_stripe".into());
    state.public_url = Some("https://api.test".into());
    state.site_origins = vec!["https://heed.test".into()];
    state.sync_wait = sync_wait;
    // Functional tests run without rate limits; tests/security.rs covers them.
    let state = state.with_limits(crate::ratelimit::Limits::unlimited());
    composio.data.lock().unwrap().insert(
        "GMAIL_FETCH_EMAILS".into(),
        json!({"messages": [
            {"messageId": "m1", "threadId": "t1", "messageTimestamp": "2026-10-30T09:15:00Z",
             "sender": "Landlord <ll@example.com>", "subject": "Lease renewal",
             "messageText": "Hi, your lease renews on 1 December. The rent stays the same."},
            {"messageId": "m2", "threadId": "t2", "messageTimestamp": "2026-10-29T10:00:00Z",
             "sender": "Shop", "subject": "Your order shipped", "messageText": "Parcel arrives Friday."}
        ]}),
    );
    composio.data.lock().unwrap().insert(
        "GOOGLECALENDAR_EVENTS_LIST".into(),
        json!({"items": [{"id": "e1", "summary": "Dentist", "start": {"dateTime": "2026-11-05T10:00:00Z"},
                          "end": {"dateTime": "2026-11-05T10:30:00Z"}}]}),
    );
    Some(Harness {
        app: router(Arc::new(state)),
        db,
        pool,
        composio,
        llm,
        auth,
        stripe,
        embedder: fake_embedder,
    })
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<Uuid>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    call_with(app, method, uri, user, body, &[]).await
}

async fn call_with(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<Uuid>,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    if let Some(u) = user {
        req = req.header("authorization", format!("Bearer tok-{u}"));
    }
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn connect_and_sync(h: &Harness, u: Uuid) {
    for t in ["gmail", "googlecalendar"] {
        let (s, v) = call(
            &h.app,
            "POST",
            "/v1/connections",
            Some(u),
            Some(json!({"toolkit": t})),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert!(
            v["redirect_url"]
                .as_str()
                .unwrap()
                .starts_with("https://connect.example/")
        );
    }
    let (s, v) = call(&h.app, "GET", "/v1/connections", Some(u), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["connections"].as_array().unwrap().len(), 2);
    assert!(
        v["connections"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["status"] == "active"),
        "{v}"
    );
    let (s, v) = call(&h.app, "POST", "/v1/sync", Some(u), Some(json!({}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let synced = v["synced"].as_array().unwrap();
    assert_eq!(synced.len(), 2);
    assert!(synced.iter().all(|r| r["error"].is_null()), "{v}");
}

#[tokio::test]
async fn m1_flow_sign_in_connect_sync_ask_with_citation() {
    let Some(h) = harness(true).await else { return };
    let u = subscriber(&h.pool, "heed").await;

    let (s, v) = call(&h.app, "GET", "/v1/me", Some(u), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["plan"], "heed");
    assert_eq!(v["entitlements"]["devices"], 1);
    assert_eq!(v["embeddings"], true);

    let (s, v) = call(&h.app, "GET", "/v1/connections", Some(u), None).await;
    assert_eq!(s, StatusCode::OK);
    let avail = v["available"].as_array().unwrap();
    assert_eq!(avail[0]["connectable"], true, "connectable apps first");
    assert!(
        avail
            .iter()
            .any(|a| a["slug"] == "slack" && a["connectable"] == false)
    );

    connect_and_sync(&h, u).await;

    let (s, v) = call(
        &h.app,
        "POST",
        "/v1/ask",
        Some(u),
        Some(json!({"question": "When does my lease renew?"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["answer"], "Your lease renews on 1 December [1].");
    let c = &v["citations"][0];
    assert_eq!(c["n"], 1);
    assert_eq!(c["title"], "Lease renewal");
    assert_eq!(c["source"], "gmail");
    assert!(c["url"].as_str().unwrap().contains("#all/t1"));
    assert!(c["snippet"].as_str().unwrap().contains("1 December"));
    let prompt = h.llm.prompts.lock().unwrap().last().cloned().unwrap();
    assert!(
        prompt.contains("Lease renewal"),
        "the right mail reached the prompt"
    );

    // Follow-up in the same conversation; usage was metered.
    let conv = v["conversation_id"].as_str().unwrap().to_owned();
    let (s, v2) = call(
        &h.app,
        "POST",
        "/v1/ask",
        Some(u),
        Some(json!({"question": "and the rent?", "conversation_id": conv})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v2["conversation_id"], conv);
    let (_, me) = call(&h.app, "GET", "/v1/me", Some(u), None).await;
    assert!(me["usage"]["month_cost_micros"].as_i64().unwrap() > 0);

    // A second sync stores nothing new (content hashes unchanged) and moves the cursor.
    let (_, v) = call(&h.app, "POST", "/v1/sync", Some(u), Some(json!({}))).await;
    assert!(
        v["synced"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["added"] == 0 && r["updated"] == 0),
        "{v}"
    );
    let gmail_calls: Vec<_> = h
        .composio
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.1 == "GMAIL_FETCH_EMAILS")
        .cloned()
        .collect();
    assert_eq!(gmail_calls.len(), 2);
    let after = |i: usize| gmail_calls[i].2["query"].as_str().unwrap().to_owned();
    assert_ne!(after(0), after(1), "cursor advanced");

    // Export holds the user's data.
    let (s, v) = call(&h.app, "GET", "/v1/export", Some(u), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["items"].as_array().unwrap().len(), 3);
    assert_eq!(v["messages"].as_array().unwrap().len(), 4);
}

#[tokio::test]
async fn full_text_only_without_embeddings() {
    let Some(h) = harness(false).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    connect_and_sync(&h, u).await;
    let (s, v) = call(
        &h.app,
        "POST",
        "/v1/ask",
        Some(u),
        Some(json!({"question": "dentist appointment"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["citations"][0]["title"], "Dentist");
    let (_, me) = call(&h.app, "GET", "/v1/me", Some(u), None).await;
    assert_eq!(me["embeddings"], false);
}

#[tokio::test]
async fn users_are_isolated() {
    let Some(h) = harness(true).await else { return };
    let a = subscriber(&h.pool, "heed").await;
    let b = subscriber(&h.pool, "heed").await;
    connect_and_sync(&h, a).await;
    let (s, v) = call(
        &h.app,
        "POST",
        "/v1/ask",
        Some(b),
        Some(json!({"question": "When does my lease renew?"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        v["citations"].as_array().unwrap().is_empty(),
        "B must not see A's mail: {v}"
    );
    assert!(
        h.llm
            .prompts
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .contains("No sources matched.")
    );
    let (_, conns) = call(&h.app, "GET", "/v1/connections", Some(b), None).await;
    assert!(conns["connections"].as_array().unwrap().is_empty());
    // B cannot delete A's connection.
    let (_, ca) = call(&h.app, "GET", "/v1/connections", Some(a), None).await;
    let id = ca["connections"][0]["id"].as_str().unwrap();
    let (s, _) = call(
        &h.app,
        "DELETE",
        &format!("/v1/connections/{id}"),
        Some(b),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, ex) = call(&h.app, "GET", "/v1/export", Some(b), None).await;
    assert!(ex["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn auth_quota_validation_and_unsupported() {
    let Some(h) = harness(false).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    let (s, v) = call(&h.app, "GET", "/v1/me", None, None).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("not_signed_in"))
    );
    let (s, _) = call(
        &h.app,
        "POST",
        "/v1/ask",
        Some(u),
        Some(json!({"question": "  "})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = call(
        &h.app,
        "POST",
        "/v1/connections",
        Some(u),
        Some(json!({"toolkit": "slack"})),
    )
    .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("unsupported_toolkit"))
    );
    // Reach Heed's fair-use hard cap ($12), then Ask is refused before any model call.
    h.pool
        .execute("insert into public.usage_events (user_id, kind, cost_micros) values ($1, 'llm', 12000000)", &[&u])
        .await
        .unwrap();
    let before = h.llm.prompts.lock().unwrap().len();
    let (s, v) = call(
        &h.app,
        "POST",
        "/v1/ask",
        Some(u),
        Some(json!({"question": "lease"})),
    )
    .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("quota_exceeded"))
    );
    assert_eq!(h.llm.prompts.lock().unwrap().len(), before);
    // Tool-call costs do not count against the AI quota.
    let w = subscriber(&h.pool, "heed").await;
    h.pool
        .execute("insert into public.usage_events (user_id, kind, cost_micros) values ($1, 'tool', 99000000)", &[&w])
        .await
        .unwrap();
    let (s, _) = call(
        &h.app,
        "POST",
        "/v1/ask",
        Some(w),
        Some(json!({"question": "lease"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn webhook_marks_due_and_rejects_bad_signatures() {
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
    assert!(!due().await, "just synced, not due");
    let body =
        json!({"type": "composio.trigger.message", "metadata": {"connected_account_id": acc}})
            .to_string();
    let ts = time::OffsetDateTime::now_utc().unix_timestamp().to_string();
    let send = |sig: String| {
        let req = Request::post("/v1/webhooks/composio")
            .header("webhook-id", "msg_1")
            .header("webhook-timestamp", ts.clone())
            .header("webhook-signature", sig)
            .body(Body::from(body.clone()))
            .unwrap();
        h.app.clone().oneshot(req)
    };
    assert_eq!(
        send("v1,AAAA".into()).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(!due().await);
    let sig = crate::webhook::sign("whsec", "msg_1", &ts, body.as_bytes());
    assert_eq!(send(sig).await.unwrap().status(), StatusCode::OK);
    assert!(due().await, "webhook made the connection due");
}

#[tokio::test]
async fn disconnect_and_delete_account() {
    let Some(h) = harness(false).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    connect_and_sync(&h, u).await;
    let (_, ca) = call(&h.app, "GET", "/v1/connections", Some(u), None).await;
    let gmail = ca["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["toolkit"] == "gmail")
        .unwrap()
        .clone();
    let (s, _) = call(
        &h.app,
        "DELETE",
        &format!("/v1/connections/{}", gmail["id"].as_str().unwrap()),
        Some(u),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, ex) = call(&h.app, "GET", "/v1/export", Some(u), None).await;
    let items = ex["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "gmail items removed with the connection");
    assert_eq!(items[0]["source"], "googlecalendar");
    assert!(
        h.composio
            .deleted
            .lock()
            .unwrap()
            .contains(&gmail["composio_account_id"].as_str().unwrap().to_owned())
    );

    let (s, _) = call(&h.app, "DELETE", "/v1/me", Some(u), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(h.auth.deleted.lock().unwrap().as_slice(), &[u]);
    assert!(
        h.composio
            .accounts
            .lock()
            .unwrap()
            .get(&u.to_string())
            .unwrap()
            .is_empty(),
        "Composio access revoked"
    );
}

#[tokio::test]
async fn scheduler_syncs_due_connections() {
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
    call(&h.app, "GET", "/v1/connections", Some(u), None).await; // records them active, due now
    let gateway = Gateway::new(None, None, String::new(), String::new());
    let ingest = Ingest::new(
        h.db.clone(),
        h.composio.clone(),
        gateway,
        Arc::new(Catalog::parse(crate::plans::DEFAULT_TOML, test_prices).unwrap()),
    );
    assert_eq!(ingest.run_due(10).await, 2);
    assert_eq!(ingest.run_due(10).await, 0, "next sync scheduled later");
    let n: i64 = h
        .pool
        .query_one("select count(*) from public.items", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 3);
}

#[tokio::test]
async fn embeddings_are_batched_per_page() {
    let Some(h) = harness(true).await else { return };
    let u = subscriber(&h.pool, "heed").await;
    // A Gmail page of 60 messages and a calendar page of 1 event.
    let messages: Vec<Value> = (0..60)
        .map(|i| json!({"messageId": format!("b{i}"), "subject": format!("Note {i}"), "messageText": "short body"}))
        .collect();
    h.composio
        .data
        .lock()
        .unwrap()
        .insert("GMAIL_FETCH_EMAILS".into(), json!({"messages": messages}));
    connect_and_sync(&h, u).await;
    let calls = h.embedder.calls.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        calls, 2,
        "one call per page (60 chunks, then 1), not one per item"
    );
    let n: i64 = h
        .pool
        .query_one(
            "select count(*) from public.chunks where embedding is not null",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 61);
    // Large pages split at EMBED_BATCH inputs.
    let texts: Vec<String> = (0..250).map(|i| format!("text {i}")).collect();
    let before = h.embedder.calls.load(std::sync::atomic::Ordering::SeqCst);
    let gateway = Gateway::new(
        None,
        Some(h.embedder.clone() as Arc<dyn Embedder>),
        String::new(),
        String::new(),
    );
    let v = gateway.embed(&h.db, u, &texts).await.unwrap().unwrap();
    assert_eq!(v.len(), 250);
    assert_eq!(
        h.embedder.calls.load(std::sync::atomic::Ordering::SeqCst) - before,
        3
    );
}

#[tokio::test]
async fn slow_first_backfill_runs_in_background_with_progress() {
    let Some(h) = harness_opts(
        false,
        &[("gmail", "ac_gmail"), ("googlecalendar", "ac_cal")],
        std::time::Duration::from_millis(50),
    )
    .await
    else {
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
    h.composio
        .delay_ms
        .store(300, std::sync::atomic::Ordering::SeqCst);
    let (s, v) = call(&h.app, "POST", "/v1/sync", Some(u), Some(json!({}))).await;
    assert_eq!(
        (s, v["running"].as_bool()),
        (StatusCode::ACCEPTED, Some(true)),
        "{v}"
    );
    let (_, v) = call(&h.app, "GET", "/v1/connections", Some(u), None).await;
    assert!(
        v["connections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["syncing"] == true),
        "{v}"
    );
    // A second request while it runs does not start a parallel run.
    let calls_before = h.composio.calls.lock().unwrap().len();
    let (_, v2) = call(&h.app, "POST", "/v1/sync", Some(u), Some(json!({}))).await;
    // Wait for the background work to finish.
    let mut done = Value::Null;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (_, v) = call(&h.app, "GET", "/v1/connections", Some(u), None).await;
        if v["connections"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["syncing"] == false)
        {
            done = v;
            break;
        }
    }
    let conns = done["connections"]
        .as_array()
        .expect("background sync finished");
    let gmail = conns.iter().find(|c| c["toolkit"] == "gmail").unwrap();
    assert_eq!(
        (
            gmail["items_synced"].as_i64(),
            gmail["backfill_done"].as_bool()
        ),
        (Some(2), Some(true)),
        "{done}"
    );
    assert!(gmail["last_synced_at"].is_string());
    let calls_after = h.composio.calls.lock().unwrap().len();
    assert!(
        calls_after - calls_before <= 2,
        "the overlapping request found runs in progress: {v2}"
    );
    let n: i64 = h
        .pool
        .query_one("select count(*) from public.items", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 3);
}
