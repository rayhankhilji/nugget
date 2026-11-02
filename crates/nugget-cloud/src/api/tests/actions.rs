//! Actions (approval, idempotency, expiry, audit), the apps catalogue and triggers, over the
//! router with test doubles and a real Postgres.

use super::*;
use crate::actions::store::{Actions, Proposal, verify_chain};
use crate::actions::{ActionKind, default_policy};

const ALL: &[(&str, &str)] = &[
    ("gmail", "ac_gmail"),
    ("googlecalendar", "ac_cal"),
    ("outlook", "ac_outlook"),
    ("slack", "ac_slack"),
    ("notion", "ac_notion"),
    ("googledrive", "ac_googledrive"),
    ("microsoft_teams", "ac_microsoft_teams"),
];

async fn connect(h: &Harness, u: Uuid, toolkits: &[&str]) {
    for t in toolkits {
        let (s, v) = call(
            &h.app,
            "POST",
            "/v1/connections",
            Some(u),
            Some(json!({"toolkit": t})),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{t}: {v}");
    }
    let (s, _) = call(&h.app, "GET", "/v1/connections", Some(u), None).await;
    assert_eq!(s, StatusCode::OK);
}

fn mail(key: &str) -> Value {
    json!({"kind": "send_mail", "toolkit": "gmail", "idempotency_key": key,
           "input": {"to": ["bob@example.com"], "subject": "Lease", "body": "Signed copy attached."}})
}

fn executions(h: &Harness, tool: &str) -> Vec<Value> {
    h.composio
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.1 == tool)
        .map(|c| c.2.clone())
        .collect()
}

#[tokio::test]
async fn nothing_is_sent_without_approval_and_approval_runs_once() {
    let Some(h) = harness_with(false, ALL).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    connect(&h, u, &["gmail"]).await;

    let (s, a) = call(&h.app, "POST", "/v1/actions", Some(u), Some(mail("k1"))).await;
    assert_eq!(s, StatusCode::OK, "{a}");
    assert_eq!(a["status"], "pending");
    assert_eq!(a["risk"], "send");
    assert_eq!(a["decision"], "confirm");
    assert_eq!(a["summary"], "Send an email to bob@example.com: “Lease”");
    assert_eq!(
        a["preview"]["fields"][0],
        json!({"label": "To", "value": "bob@example.com"})
    );
    let id = a["id"].as_str().unwrap().to_owned();
    let digest = a["digest"].as_str().unwrap().to_owned();
    assert_eq!(digest.len(), 64);

    // Preview is a dry run: same plan, nothing sent.
    let (s, p) = call(
        &h.app,
        "GET",
        &format!("/v1/actions/{id}/preview"),
        Some(u),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        (p["digest"].as_str(), p["status"].as_str()),
        (Some(digest.as_str()), Some("pending"))
    );
    assert!(
        executions(&h, "GMAIL_SEND_EMAIL").is_empty(),
        "nothing sent before approval"
    );

    // An approval for something else is refused.
    let (s, e) = call(
        &h.app,
        "POST",
        &format!("/v1/actions/{id}/approve"),
        Some(u),
        Some(json!({"digest": "0".repeat(64)})),
    )
    .await;
    assert_eq!(
        (s, e["error"].as_str()),
        (StatusCode::PRECONDITION_FAILED, Some("digest_mismatch"))
    );
    assert!(executions(&h, "GMAIL_SEND_EMAIL").is_empty());

    let (s, done) = call(
        &h.app,
        "POST",
        &format!("/v1/actions/{id}/approve"),
        Some(u),
        Some(json!({"digest": digest})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{done}");
    assert_eq!(done["status"], "succeeded");
    assert!(done["executed_at"].is_string());
    let sent = executions(&h, "GMAIL_SEND_EMAIL");
    assert_eq!(
        sent,
        vec![
            json!({"recipient_email": "bob@example.com", "extra_recipients": [], "cc": [], "bcc": [],
                                  "subject": "Lease", "body": "Signed copy attached.", "is_html": false})
        ]
    );

    // Approving again, or proposing again with the same key, never sends twice.
    let (s, again) = call(
        &h.app,
        "POST",
        &format!("/v1/actions/{id}/approve"),
        Some(u),
        Some(json!({"digest": digest})),
    )
    .await;
    assert_eq!(
        (s, again["status"].as_str()),
        (StatusCode::OK, Some("succeeded"))
    );
    let (s, same) = call(&h.app, "POST", "/v1/actions", Some(u), Some(mail("k1"))).await;
    assert_eq!(
        (s, same["id"].as_str()),
        (StatusCode::OK, Some(id.as_str()))
    );
    assert_eq!(executions(&h, "GMAIL_SEND_EMAIL").len(), 1);
    let (s, e) = call(
        &h.app,
        "POST",
        &format!("/v1/actions/{id}/cancel"),
        Some(u),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        (s, e["error"].as_str()),
        (StatusCode::CONFLICT, Some("not_pending"))
    );

    // Status, list, and the audit chain.
    let (_, st) = call(&h.app, "GET", &format!("/v1/actions/{id}"), Some(u), None).await;
    assert_eq!(st["status"], "succeeded");
    let (_, l) = call(&h.app, "GET", "/v1/actions?status=succeeded", Some(u), None).await;
    assert_eq!(l["actions"].as_array().unwrap().len(), 1);
    let (s, _) = call(&h.app, "GET", "/v1/actions?status=bogus", Some(u), None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let chain = h.db.audit_chain(u).await.unwrap();
    let events: Vec<&str> = chain.iter().map(|e| e.event.as_str()).collect();
    assert_eq!(
        events,
        ["proposed", "approval_rejected", "approved", "succeeded"]
    );
    assert!(
        chain
            .iter()
            .all(|e| !e.detail.to_string().contains("Signed copy")),
        "no bodies in the audit"
    );
    verify_chain(&chain).unwrap();
    // Tampering (only a superuser can) breaks the chain.
    h.pool
        .execute(
            "update public.action_audit set event = 'cancelled' where user_id = $1 and seq = 3",
            &[&u],
        )
        .await
        .unwrap();
    assert_eq!(verify_chain(&h.db.audit_chain(u).await.unwrap()), Err(3));
    // The export carries actions and the audit.
    let (_, ex) = call(&h.app, "GET", "/v1/export", Some(u), None).await;
    assert_eq!(ex["pending_actions"].as_array().unwrap().len(), 1);
    assert_eq!(ex["action_audit"].as_array().unwrap().len(), 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_approvals_send_once() {
    let Some(h) = harness_with(false, ALL).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    connect(&h, u, &["slack"]).await;
    let (_, a) = call(
        &h.app,
        "POST",
        "/v1/actions",
        Some(u),
        Some(json!({
        "kind": "post_message", "toolkit": "slack", "idempotency_key": "slack-1",
        "input": {"channel": "C024BE91L", "text": "Deploy done"}})),
    )
    .await;
    let id = a["id"].as_str().unwrap().to_owned();
    let body = json!({"digest": a["digest"]});
    let uri = format!("/v1/actions/{id}/approve");
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let (app, uri, body) = (h.app.clone(), uri.clone(), body.clone());
        set.spawn(async move { call(&app, "POST", &uri, Some(u), Some(body)).await });
    }
    let results = set.join_all().await;
    assert!(
        results.iter().all(|(s, _)| *s == StatusCode::OK),
        "{results:?}"
    );
    let posted = executions(&h, "SLACK_SEND_MESSAGE");
    assert_eq!(
        posted,
        vec![json!({"channel": "C024BE91L", "markdown_text": "Deploy done"})]
    );
    let approvals =
        h.db.audit_chain(u)
            .await
            .unwrap()
            .iter()
            .filter(|e| e.event == "approved")
            .count();
    assert_eq!(approvals, 1);
}

#[tokio::test]
async fn idempotency_validation_and_connections() {
    let Some(h) = harness_with(false, ALL).await else {
        return;
    };
    // Without a live subscription, proposing is refused (402) before anything else.
    let lapsed = add_user(&h.pool).await;
    let (s, e) = call(
        &h.app,
        "POST",
        "/v1/actions",
        Some(lapsed),
        Some(mail("k0")),
    )
    .await;
    assert_eq!(
        (s, e["error"].as_str()),
        (StatusCode::PAYMENT_REQUIRED, Some("subscription_required"))
    );
    let (s, _) = call(&h.app, "GET", "/v1/actions", Some(lapsed), None).await;
    assert_eq!(s, StatusCode::OK, "reading stays open");
    let u = subscriber(&h.pool, "heed").await;
    // Not connected yet.
    let (s, e) = call(&h.app, "POST", "/v1/actions", Some(u), Some(mail("k1"))).await;
    assert_eq!(
        (s, e["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("not_connected"))
    );
    connect(&h, u, &["gmail", "notion"]).await;
    let (s, a) = call(&h.app, "POST", "/v1/actions", Some(u), Some(mail("k1"))).await;
    assert_eq!(s, StatusCode::OK);
    // Same key, different action.
    let mut other = mail("k1");
    other["input"]["subject"] = json!("Something else");
    let (s, e) = call(&h.app, "POST", "/v1/actions", Some(u), Some(other)).await;
    assert_eq!(
        (s, e["error"].as_str()),
        (StatusCode::CONFLICT, Some("idempotency_conflict"))
    );
    // Bad inputs and keys.
    for (body, code) in [
        (
            json!({"kind": "send_mail", "toolkit": "gmail", "idempotency_key": "k2",
                "input": {"to": ["not-an-address"], "subject": "s", "body": "b"}}),
            "bad_action",
        ),
        (
            json!({"kind": "launch_rocket", "toolkit": "gmail", "idempotency_key": "k3", "input": {}}),
            "bad_action",
        ),
        (
            json!({"kind": "post_message", "toolkit": "gmail", "idempotency_key": "k4", "input": {}}),
            "bad_action",
        ),
        (
            json!({"kind": "send_mail", "toolkit": "gmail", "idempotency_key": "has space", "input": {}}),
            "bad_action",
        ),
        (
            json!({"kind": "post_message", "toolkit": "slack", "idempotency_key": "k5",
                "input": {"channel": "C1", "text": "hi"}}),
            "not_connected",
        ),
    ] {
        let (s, e) = call(&h.app, "POST", "/v1/actions", Some(u), Some(body.clone())).await;
        assert_eq!(
            (s, e["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some(code)),
            "{body}"
        );
    }
    // Cancel: then approval is refused and nothing runs.
    let id = a["id"].as_str().unwrap();
    let (s, c) = call(
        &h.app,
        "POST",
        &format!("/v1/actions/{id}/cancel"),
        Some(u),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        (s, c["status"].as_str()),
        (StatusCode::OK, Some("cancelled"))
    );
    let (s, e) = call(
        &h.app,
        "POST",
        &format!("/v1/actions/{id}/approve"),
        Some(u),
        Some(json!({"digest": a["digest"]})),
    )
    .await;
    assert_eq!(
        (s, e["error"].as_str()),
        (StatusCode::CONFLICT, Some("not_pending"))
    );
    // Expiry.
    let (_, n) = call(
        &h.app,
        "POST",
        "/v1/actions",
        Some(u),
        Some(json!({
        "kind": "create_page", "toolkit": "notion", "idempotency_key": "page-1",
        "input": {"parent_id": "59833787-2cf9-4fdf-8782-e53db20768a5", "title": "Notes"}})),
    )
    .await;
    let nid: Uuid = n["id"].as_str().unwrap().parse().unwrap();
    h.pool
        .execute("update public.pending_actions set expires_at = now() - interval '1 minute' where id = $1", &[&nid])
        .await
        .unwrap();
    let (s, e) = call(
        &h.app,
        "POST",
        &format!("/v1/actions/{nid}/approve"),
        Some(u),
        Some(json!({"digest": n["digest"]})),
    )
    .await;
    assert_eq!(
        (s, e["error"].as_str()),
        (StatusCode::GONE, Some("expired"))
    );
    let (_, st) = call(&h.app, "GET", &format!("/v1/actions/{nid}"), Some(u), None).await;
    assert_eq!(st["status"], "expired");
    assert!(
        h.composio
            .calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| !c.1.contains("SEND") && !c.1.contains("CREATE"))
    );
    verify_chain(&h.db.audit_chain(u).await.unwrap()).unwrap();
}

#[tokio::test]
async fn actions_are_per_user() {
    let Some(h) = harness_with(false, ALL).await else {
        return;
    };
    let a = subscriber(&h.pool, "heed").await;
    let b = subscriber(&h.pool, "heed").await;
    connect(&h, a, &["gmail"]).await;
    connect(&h, b, &["gmail"]).await;
    let (_, act) = call(&h.app, "POST", "/v1/actions", Some(a), Some(mail("k1"))).await;
    let id = act["id"].as_str().unwrap();
    for (m, uri, body) in [
        ("GET", format!("/v1/actions/{id}"), None),
        ("GET", format!("/v1/actions/{id}/preview"), None),
        (
            "POST",
            format!("/v1/actions/{id}/approve"),
            Some(json!({"digest": act["digest"]})),
        ),
        ("POST", format!("/v1/actions/{id}/cancel"), Some(json!({}))),
    ] {
        let (s, _) = call(&h.app, m, &uri, Some(b), body).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{m} {uri}");
    }
    // B may reuse A's key: keys are per user.
    let (s, mine) = call(&h.app, "POST", "/v1/actions", Some(b), Some(mail("k1"))).await;
    assert_eq!(s, StatusCode::OK);
    assert_ne!(mine["id"], act["id"]);
    assert_ne!(
        mine["digest"], act["digest"],
        "bound to each user's own account"
    );
    let (_, l) = call(&h.app, "GET", "/v1/actions", Some(b), None).await;
    assert_eq!(l["actions"].as_array().unwrap().len(), 1);
    assert!(executions(&h, "GMAIL_SEND_EMAIL").is_empty());
}

#[tokio::test]
async fn failures_are_recorded_and_not_retried() {
    let Some(h) = harness_with(false, ALL).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    connect(&h, u, &["gmail"]).await;
    h.composio
        .fail
        .lock()
        .unwrap()
        .insert("GMAIL_SEND_EMAIL".into(), "insufficient scopes".into());
    let (_, a) = call(&h.app, "POST", "/v1/actions", Some(u), Some(mail("k1"))).await;
    let uri = format!("/v1/actions/{}/approve", a["id"].as_str().unwrap());
    let (s, r) = call(
        &h.app,
        "POST",
        &uri,
        Some(u),
        Some(json!({"digest": a["digest"]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r["status"], "failed");
    assert!(r["error"].as_str().unwrap().contains("insufficient scopes"));
    let (_, r2) = call(
        &h.app,
        "POST",
        &uri,
        Some(u),
        Some(json!({"digest": a["digest"]})),
    )
    .await;
    assert_eq!(r2["status"], "failed");
    assert_eq!(
        executions(&h, "GMAIL_SEND_EMAIL").len(),
        1,
        "a failed send is not retried"
    );
    // Disconnecting before approval: the action cannot run.
    h.composio.fail.lock().unwrap().clear();
    let (_, b) = call(&h.app, "POST", "/v1/actions", Some(u), Some(mail("k2"))).await;
    let (_, conns) = call(&h.app, "GET", "/v1/connections", Some(u), None).await;
    let cid = conns["connections"][0]["id"].as_str().unwrap();
    call(
        &h.app,
        "DELETE",
        &format!("/v1/connections/{cid}"),
        Some(u),
        None,
    )
    .await;
    let (s, e) = call(
        &h.app,
        "POST",
        &format!("/v1/actions/{}/approve", b["id"].as_str().unwrap()),
        Some(u),
        Some(json!({"digest": b["digest"]})),
    )
    .await;
    assert_eq!(
        (s, e["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("not_connected"))
    );
    assert_eq!(executions(&h, "GMAIL_SEND_EMAIL").len(), 1);
}

#[tokio::test]
async fn update_event_previews_a_diff_against_the_synced_event() {
    let Some(h) = harness_with(false, ALL).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    connect(&h, u, &["googlecalendar"]).await;
    call(&h.app, "POST", "/v1/sync", Some(u), Some(json!({}))).await; // stores e1 "Dentist"
    let (s, a) = call(&h.app, "POST", "/v1/actions", Some(u), Some(json!({
        "kind": "update_event", "toolkit": "googlecalendar", "idempotency_key": "move-dentist",
        "input": {"event_id": "e1", "start": "2026-11-06T10:00:00Z", "end": "2026-11-06T10:30:00Z"}}))).await;
    assert_eq!(s, StatusCode::OK, "{a}");
    assert_eq!(a["summary"], "Update the event “Dentist”");
    assert_eq!(
        a["preview"]["diff"][0],
        json!({"field": "start", "before": "2026-11-05T10:00:00Z",
                                                "after": "2026-11-06 10:00:00 UTC"})
    );
    let (_, done) = call(
        &h.app,
        "POST",
        &format!("/v1/actions/{}/approve", a["id"].as_str().unwrap()),
        Some(u),
        Some(json!({"digest": a["digest"]})),
    )
    .await;
    assert_eq!(done["status"], "succeeded");
    assert_eq!(
        executions(&h, "GOOGLECALENDAR_PATCH_EVENT"),
        vec![json!({
        "calendar_id": "primary", "event_id": "e1", "start_time": "2026-11-06T10:00:00Z",
        "end_time": "2026-11-06T10:30:00Z", "send_updates": "all"})]
    );
}

#[tokio::test]
async fn policy_denials_are_stored_and_never_run() {
    let Some(h) = harness_with(false, ALL).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    connect(&h, u, &["slack"]).await;
    let mut cfg = nugget_policy::PolicyConfig::default();
    cfg.rules.push(
        nugget_policy::Rule::new("test:no-slack", nugget_policy::Effect::Deny).targets(["slack"]),
    );
    let actions = Actions {
        db: h.db.clone(),
        composio: Some(h.composio.clone() as Arc<dyn Composio>),
        policy: Arc::new(nugget_policy::PolicyEngine::new(cfg)),
    };
    let input = json!({"channel": "C1", "text": "hi"});
    let e = actions
        .propose(
            u,
            Proposal {
                kind: ActionKind::PostMessage,
                toolkit: "slack",
                input: &input,
                idempotency_key: "d1",
            },
        )
        .await
        .unwrap_err();
    assert!(e.to_string().contains("test:no-slack"), "{e}");
    let (_, l) = call(&h.app, "GET", "/v1/actions?status=denied", Some(u), None).await;
    let denied = &l["actions"][0];
    assert_eq!(denied["decision"], "deny");
    let (s, e) = call(
        &h.app,
        "POST",
        &format!("/v1/actions/{}/approve", denied["id"].as_str().unwrap()),
        Some(u),
        Some(json!({"digest": denied["digest"]})),
    )
    .await;
    assert_eq!(
        (s, e["error"].as_str()),
        (StatusCode::CONFLICT, Some("not_pending"))
    );
    assert!(executions(&h, "SLACK_SEND_MESSAGE").is_empty());
    let events: Vec<_> =
        h.db.audit_chain(u)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.event)
            .collect();
    assert_eq!(events, ["proposed", "denied"]);
    // The default policy still asks.
    let _ = default_policy();
}

#[tokio::test]
async fn apps_catalogue_lists_logos_capabilities_and_connections() {
    let Some(h) = harness_with(false, &[("gmail", "ac_gmail"), ("slack", "ac_slack")]).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    connect(&h, u, &["gmail"]).await;
    let (s, v) = call(&h.app, "GET", "/v1/apps", Some(u), None).await;
    assert_eq!(s, StatusCode::OK);
    let apps = v["apps"].as_array().unwrap();
    assert_eq!(apps.len(), crate::apps::CATALOG.len());
    let app = |slug: &str| apps.iter().find(|a| a["slug"] == slug).unwrap().clone();
    let gmail = app("gmail");
    assert_eq!(gmail["connected"], true);
    assert_eq!(gmail["connectable"], true);
    assert_eq!(gmail["category"], "mail");
    assert_eq!(gmail["actions"], json!(["send_mail", "reply_mail"]));
    assert_eq!(gmail["syncs"], true);
    let slack = app("slack");
    assert_eq!(
        (slack["connected"].as_bool(), slack["connectable"].as_bool()),
        (Some(false), Some(true))
    );
    assert_eq!(
        slack["logo_url"], "https://logos.composio.dev/api/slack",
        "fallback when the catalogue has none"
    );
    // The desktop CSP allows only data: images; the test state never fetches, so a monogram.
    assert!(
        apps.iter()
            .all(|a| a["logo"].as_str().unwrap().starts_with("data:image/")),
        "{v}"
    );
    assert_eq!(
        app("notion")["connectable"],
        false,
        "no auth config in this deployment"
    );
    assert_eq!(app("notion")["sync_mode"], "trigger");
    assert_eq!(app("microsoft_teams")["categories"], json!(["chat"]));
    let (s, _) = call(&h.app, "GET", "/v1/apps", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Slack is connectable now that this deployment has its auth config.
    let (s, _) = call(
        &h.app,
        "POST",
        "/v1/connections",
        Some(u),
        Some(json!({"toolkit": "slack"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn trigger_apps_enable_triggers_and_webhooks_must_name_the_owner() {
    let Some(h) = harness_with(false, ALL).await else {
        return;
    };
    let u = subscriber(&h.pool, "heed").await;
    let other = subscriber(&h.pool, "heed").await;
    connect(&h, u, &["notion", "slack"]).await;
    let (_, v) = call(&h.app, "POST", "/v1/sync", Some(u), Some(json!({}))).await;
    assert!(
        v["synced"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["error"].is_null()),
        "{v}"
    );
    let triggers = h.composio.triggers.lock().unwrap().clone();
    let slugs: Vec<&str> = triggers.iter().map(|t| t.2.as_str()).collect();
    assert_eq!(
        slugs,
        ["NOTION_PAGE_CREATED", "NOTION_PAGE_CONTENT_UPDATED"],
        "Notion only; Slack polls"
    );
    // Once: a second sync does not enable them again.
    call(&h.app, "POST", "/v1/sync", Some(u), Some(json!({}))).await;
    assert_eq!(h.composio.triggers.lock().unwrap().len(), 2);
    // Trigger-mode connections poll daily as a safety net.
    let next: bool = h.pool
        .query_one("select next_sync_at > now() + interval '23 hours' from public.connections where toolkit = 'notion' and user_id = $1", &[&u])
        .await.unwrap().get(0);
    assert!(next);

    let account = triggers[0].1.clone();
    let due = || async {
        h.pool.query_one("select next_sync_at <= now() from public.connections where composio_account_id = $1", &[&account])
            .await.unwrap().get::<_, bool>(0)
    };
    let send = |user_id: Uuid| {
        let body = json!({"id": "msg_2", "type": "composio.trigger.message",
                          "metadata": {"trigger_slug": "NOTION_PAGE_CONTENT_UPDATED", "connected_account_id": account,
                                       "user_id": user_id.to_string()},
                          "data": {}}).to_string();
        let ts = time::OffsetDateTime::now_utc().unix_timestamp().to_string();
        let sig = crate::webhook::sign("whsec", "msg_2", &ts, body.as_bytes());
        let req = Request::post("/v1/webhooks/composio")
            .header("webhook-id", "msg_2")
            .header("webhook-timestamp", ts)
            .header("webhook-signature", sig)
            .body(Body::from(body))
            .unwrap();
        h.app.clone().oneshot(req)
    };
    assert_eq!(send(other).await.unwrap().status(), StatusCode::OK);
    assert!(!due().await, "an event naming another user is ignored");
    assert_eq!(send(u).await.unwrap().status(), StatusCode::OK);
    assert!(
        due().await,
        "the owner's trigger event makes the connection due"
    );
}
