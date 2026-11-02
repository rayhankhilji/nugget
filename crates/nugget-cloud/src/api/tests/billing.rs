//! Billing: gating without a subscription, Checkout, Stripe webhooks (idempotent, order
//! independent), entitlements (models, devices, connections) against real Postgres.

use super::*;
use crate::stripe::{Subscription, sign};

async fn stripe_event(h: &Harness, id: &str, kind: &str, object: Value) -> StatusCode {
    let body = json!({"id": id, "type": kind, "data": {"object": object}}).to_string();
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let req = Request::post("/v1/webhooks/stripe")
        .header(
            "stripe-signature",
            sign("whsec_stripe", now, body.as_bytes()),
        )
        .body(Body::from(body))
        .unwrap();
    h.app.clone().oneshot(req).await.unwrap().status()
}

fn sub(id: &str, customer: &str, status: &str, price: &str) -> Subscription {
    Subscription {
        id: id.into(),
        customer: customer.into(),
        status: status.into(),
        price_id: price.into(),
        trial_end: None,
        current_period_end: None,
        cancel_at_period_end: false,
        user_id: None,
    }
}

#[tokio::test]
async fn no_subscription_no_service() {
    let Some(h) = harness(false).await else {
        return;
    };
    let u = add_user(&h.pool).await;
    let (s, v) = call(&h.app, "GET", "/v1/me", Some(u), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        v["plan"].is_null() && v["entitlements"].is_null() && v["subscription"].is_null(),
        "{v}"
    );
    for (m, uri, body) in [
        ("POST", "/v1/ask", json!({"question": "lease"})),
        ("POST", "/v1/connections", json!({"toolkit": "gmail"})),
        ("POST", "/v1/sync", json!({})),
    ] {
        let (s, v) = call(&h.app, m, uri, Some(u), Some(body)).await;
        assert_eq!(
            (s, v["error"].as_str()),
            (StatusCode::PAYMENT_REQUIRED, Some("subscription_required")),
            "{uri}"
        );
    }
    // Pricing is public.
    let (s, v) = call(&h.app, "GET", "/v1/billing/plans", None, None).await;
    assert_eq!(s, StatusCode::OK);
    let plans = v["plans"].as_array().unwrap();
    assert_eq!(plans.len(), 2);
    let heed = plans.iter().find(|p| p["id"] == "heed").unwrap();
    assert_eq!(
        (
            heed["monthly_gbp"].as_u64(),
            heed["annual_gbp"].as_u64(),
            heed["trial_days"].as_u64()
        ),
        (Some(39), Some(390), Some(14))
    );
    let (s, v) = call(
        &h.app,
        "POST",
        "/v1/billing/portal",
        Some(u),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (StatusCode::CONFLICT, Some("no_customer"))
    );
}

#[tokio::test]
async fn checkout_webhooks_and_plan_changes() {
    let Some(h) = harness(false).await else {
        return;
    };
    let u = add_user(&h.pool).await;
    let (s, v) = call(
        &h.app,
        "POST",
        "/v1/billing/checkout",
        Some(u),
        Some(json!({"plan": "heed", "interval": "month"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["url"], "https://checkout.stripe.test/price_hm");
    let customer = h.stripe.customers.lock().unwrap()[0].clone();
    assert_eq!(
        h.stripe.checkouts.lock().unwrap()[0],
        (customer.clone(), "price_hm".into(), Some(14)),
        "14-day trial"
    );

    // Checkout completes: Stripe says trialing on Heed monthly.
    h.stripe.subs.lock().unwrap().insert(
        "sub_1".into(),
        sub("sub_1", &customer, "trialing", "price_hm"),
    );
    let completed = json!({"id": "cs_1", "subscription": "sub_1", "customer": customer, "client_reference_id": u.to_string()});
    assert_eq!(
        stripe_event(&h, "evt_1", "checkout.session.completed", completed.clone()).await,
        StatusCode::OK
    );
    let (_, me) = call(&h.app, "GET", "/v1/me", Some(u), None).await;
    assert_eq!(me["plan"], "heed");
    assert_eq!(me["subscription"]["status"], "trialing");
    assert_eq!(me["entitlements"]["max_connections"], 10);
    // The same event again is acknowledged without work.
    let fetches = *h.stripe.fetches.lock().unwrap();
    assert_eq!(
        stripe_event(&h, "evt_1", "checkout.session.completed", completed).await,
        StatusCode::OK
    );
    assert_eq!(*h.stripe.fetches.lock().unwrap(), fetches);

    // A second checkout is refused while subscribed.
    let (s, _) = call(
        &h.app,
        "POST",
        "/v1/billing/checkout",
        Some(u),
        Some(json!({"plan": "heed_max", "interval": "year"})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Upgrade to Max in the portal; a stale event arriving later still stores Stripe's current state.
    h.stripe.subs.lock().unwrap().insert(
        "sub_1".into(),
        sub("sub_1", &customer, "active", "price_xm"),
    );
    assert_eq!(
        stripe_event(
            &h,
            "evt_2",
            "customer.subscription.updated",
            json!({"id": "sub_1"})
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        stripe_event(
            &h,
            "evt_0_late",
            "customer.subscription.updated",
            json!({"id": "sub_1"})
        )
        .await,
        StatusCode::OK
    );
    let (_, me) = call(&h.app, "GET", "/v1/me", Some(u), None).await;
    assert_eq!(
        (me["plan"].as_str(), me["subscription"]["status"].as_str()),
        (Some("heed_max"), Some("active"))
    );
    assert!(me["entitlements"]["max_connections"].is_null(), "unlimited");
    assert_eq!(me["entitlements"]["computer_use"], true);

    // Payment fails: past_due keeps access while Stripe retries (newer invoice shape).
    h.stripe.subs.lock().unwrap().insert(
        "sub_1".into(),
        sub("sub_1", &customer, "past_due", "price_xm"),
    );
    let invoice =
        json!({"id": "in_1", "parent": {"subscription_details": {"subscription": "sub_1"}}});
    assert_eq!(
        stripe_event(&h, "evt_3", "invoice.payment_failed", invoice).await,
        StatusCode::OK
    );
    let (s, _) = call(
        &h.app,
        "POST",
        "/v1/ask",
        Some(u),
        Some(json!({"question": "lease"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // Cancelled: no service; a new checkout gets no second trial.
    h.stripe.subs.lock().unwrap().insert(
        "sub_1".into(),
        sub("sub_1", &customer, "canceled", "price_xm"),
    );
    assert_eq!(
        stripe_event(
            &h,
            "evt_4",
            "customer.subscription.deleted",
            json!({"id": "sub_1"})
        )
        .await,
        StatusCode::OK
    );
    let (s, _) = call(
        &h.app,
        "POST",
        "/v1/ask",
        Some(u),
        Some(json!({"question": "lease"})),
    )
    .await;
    assert_eq!(s, StatusCode::PAYMENT_REQUIRED);
    let (s, _) = call(
        &h.app,
        "POST",
        "/v1/billing/checkout",
        Some(u),
        Some(json!({"plan": "heed", "interval": "year"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        h.stripe.checkouts.lock().unwrap()[1],
        (customer.clone(), "price_ha".into(), None),
        "no repeat trial"
    );
    let (s, v) = call(
        &h.app,
        "POST",
        "/v1/billing/portal",
        Some(u),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        (s, v["url"].as_str()),
        (
            StatusCode::OK,
            Some(format!("https://billing.stripe.test/{customer}").as_str())
        )
    );

    // Unrelated events are acknowledged; bad signatures and unknown prices are not.
    assert_eq!(
        stripe_event(&h, "evt_5", "customer.created", json!({"id": customer})).await,
        StatusCode::OK
    );
    h.stripe.subs.lock().unwrap().insert(
        "sub_2".into(),
        sub("sub_2", &customer, "active", "price_unknown"),
    );
    assert_eq!(
        stripe_event(
            &h,
            "evt_6",
            "customer.subscription.created",
            json!({"id": "sub_2"})
        )
        .await,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let seen: bool = h
        .pool
        .query_one(
            "select exists(select 1 from nugget_private.stripe_events where id = 'evt_6')",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(
        !seen,
        "a failed event stays unprocessed so Stripe retries it"
    );
    let req = Request::post("/v1/webhooks/stripe")
        .header("stripe-signature", "t=1,v1=00")
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(
        h.app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn webhook_for_unknown_customer_uses_our_metadata_only() {
    let Some(h) = harness(false).await else {
        return;
    };
    let u = add_user(&h.pool).await;
    let mut s1 = sub("sub_9", "cus_elsewhere", "active", "price_ha");
    s1.user_id = Some(u.to_string());
    h.stripe.subs.lock().unwrap().insert("sub_9".into(), s1);
    assert_eq!(
        stripe_event(
            &h,
            "evt_9",
            "customer.subscription.created",
            json!({"id": "sub_9"})
        )
        .await,
        StatusCode::OK
    );
    let (_, me) = call(&h.app, "GET", "/v1/me", Some(u), None).await;
    assert_eq!(
        (me["plan"].as_str(), me["subscription"]["interval"].as_str()),
        (Some("heed"), Some("year"))
    );
    // No metadata, no owner: refused (Stripe will retry; nothing is guessed).
    h.stripe.subs.lock().unwrap().insert(
        "sub_10".into(),
        sub("sub_10", "cus_other", "active", "price_ha"),
    );
    assert_eq!(
        stripe_event(
            &h,
            "evt_10",
            "customer.subscription.created",
            json!({"id": "sub_10"})
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[tokio::test]
async fn models_follow_the_plan_and_the_allowance() {
    let Some(h) = harness(false).await else {
        return;
    };
    let heed = subscriber(&h.pool, "heed").await;
    let max = subscriber(&h.pool, "heed_max").await;
    let ask = |u: Uuid, model: Option<&str>| {
        let mut b = json!({"question": "lease"});
        if let Some(m) = model {
            b["model"] = json!(m);
        }
        call(&h.app, "POST", "/v1/ask", Some(u), Some(b))
    };
    let (_, v) = ask(heed, None).await;
    assert_eq!(
        (v["model_tier"].as_str(), v["downgraded"].as_bool()),
        (Some("fast"), Some(false)),
        "Heed defaults to fast"
    );
    let (_, v) = ask(heed, Some("smart")).await;
    assert_eq!(v["model_tier"], "smart", "Heed can spend smart asks");
    let (_, v) = ask(max, None).await;
    assert_eq!(v["model_tier"], "smart", "Max defaults to smart");
    // Max's smart allowance used up: answers fall back to fast and say so.
    h.pool
        .execute(
            "insert into public.usage_events (user_id, kind, model, cost_micros)
             select $1, 'llm', 'claude-sonnet-5-5', 0 from generate_series(1, 900)",
            &[&max],
        )
        .await
        .unwrap();
    let (_, v) = ask(max, None).await;
    assert_eq!(
        (v["model_tier"].as_str(), v["downgraded"].as_bool()),
        (Some("fast"), Some(true))
    );
    let (_, me) = call(&h.app, "GET", "/v1/me", Some(max), None).await;
    assert!(me["usage"]["smart_asks_used"].as_i64().unwrap() >= 900);
    // Past the fair-use budget (not the hard cap), Heed's smart requests downgrade too.
    h.pool
        .execute("insert into public.usage_events (user_id, kind, cost_micros) values ($1, 'llm', 8000000)", &[&heed])
        .await
        .unwrap();
    let (s, v) = ask(heed, Some("smart")).await;
    assert_eq!(
        (s, v["model_tier"].as_str(), v["downgraded"].as_bool()),
        (StatusCode::OK, Some("fast"), Some(true))
    );
}

async fn dev(h: &Harness, u: Uuid, d: &str) -> (StatusCode, Value) {
    call_with(
        &h.app,
        "GET",
        "/v1/me",
        Some(u),
        None,
        &[("x-heed-device", d)],
    )
    .await
}

#[tokio::test]
async fn device_and_connection_limits() {
    let Some(h) = harness(false).await else {
        return;
    };
    let heed = subscriber(&h.pool, "heed").await;
    assert_eq!(dev(&h, heed, "laptop-0001").await.0, StatusCode::OK);
    assert_eq!(
        dev(&h, heed, "laptop-0001").await.0,
        StatusCode::OK,
        "same device again"
    );
    let (s, v) = dev(&h, heed, "desktop-0002").await;
    assert_eq!(
        (s, v["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("device_limit_reached"))
    );
    let max = subscriber(&h.pool, "heed_max").await;
    for d in ["a-device-01", "a-device-02", "a-device-03"] {
        assert_eq!(dev(&h, max, d).await.0, StatusCode::OK);
    }
    assert_eq!(dev(&h, max, "a-device-04").await.0, StatusCode::FORBIDDEN);

    // Heed allows 10 connected apps.
    let u = subscriber(&h.pool, "heed").await;
    for i in 0..10 {
        h.pool
            .execute(
                "insert into public.connections (user_id, toolkit, composio_account_id) values ($1, $2, $3)",
                &[&u, &format!("app{i}"), &format!("ca_x{i}_{}", u.simple())],
            )
            .await
            .unwrap();
    }
    let (s, v) = call(
        &h.app,
        "POST",
        "/v1/connections",
        Some(u),
        Some(json!({"toolkit": "gmail"})),
    )
    .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("connection_limit_reached"))
    );
}

#[tokio::test]
async fn return_page_hands_back_to_the_app() {
    let Some(h) = harness(false).await else {
        return;
    };
    let res = h
        .app
        .clone()
        .oneshot(Request::get("/billing/done").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(html.contains("nugget://billing/done"));
}

#[tokio::test]
async fn account_deletion_cancels_billing_first_and_aborts_on_failure() {
    let Some(h) = harness(false).await else {
        return;
    };
    let u = add_user(&h.pool).await;
    let (s, _) = call(
        &h.app,
        "POST",
        "/v1/billing/checkout",
        Some(u),
        Some(json!({"plan": "heed", "interval": "month"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let customer = h.stripe.customers.lock().unwrap()[0].clone();
    let (_, ex) = call(&h.app, "GET", "/v1/export", Some(u), None).await;
    assert_eq!(
        ex["billing_customers"][0]["stripe_customer_id"],
        customer.as_str()
    );

    // Stripe fails: nothing is deleted, and the provider's text does not leak.
    h.stripe
        .fail_delete
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (s, v) = call(&h.app, "DELETE", "/v1/me", Some(u), None).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (StatusCode::BAD_GATEWAY, Some("billing_error"))
    );
    assert!(!v.to_string().contains("secret detail"));
    assert!(
        h.auth.deleted.lock().unwrap().is_empty(),
        "auth user kept for a retry"
    );

    h.stripe
        .fail_delete
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let (s, _) = call(&h.app, "DELETE", "/v1/me", Some(u), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(
        h.stripe.deleted_customers.lock().unwrap().as_slice(),
        &[customer]
    );
    assert_eq!(h.auth.deleted.lock().unwrap().as_slice(), &[u]);
}

#[tokio::test]
async fn website_cors_return_urls_and_no_device_seat() {
    let Some(h) = harness(false).await else {
        return;
    };
    // Preflight from the site is answered; other origins get no CORS grant.
    let pre = |origin: &'static str| {
        Request::builder()
            .method("OPTIONS")
            .uri("/v1/billing/portal")
            .header("origin", origin)
            .header("access-control-request-method", "POST")
            .header(
                "access-control-request-headers",
                "authorization,content-type",
            )
            .body(Body::empty())
            .unwrap()
    };
    let res = h
        .app
        .clone()
        .oneshot(pre("https://heed.test"))
        .await
        .unwrap();
    assert!(res.status().is_success());
    let hv = |r: &axum::response::Response, k: &str| {
        r.headers().get(k).map(|v| v.to_str().unwrap().to_owned())
    };
    assert_eq!(
        hv(&res, "access-control-allow-origin").as_deref(),
        Some("https://heed.test")
    );
    assert!(
        hv(&res, "access-control-allow-methods")
            .unwrap()
            .contains("DELETE")
    );
    assert!(
        hv(&res, "access-control-allow-credentials").is_none(),
        "bearer auth, no cookies"
    );
    let res = h
        .app
        .clone()
        .oneshot(pre("https://evil.test"))
        .await
        .unwrap();
    assert!(hv(&res, "access-control-allow-origin").is_none());

    // A Heed (1 device) user: the desktop registers, then the website manages billing freely.
    let u = subscriber(&h.pool, "heed").await;
    assert_eq!(dev(&h, u, "laptop-0001").await.0, StatusCode::OK);
    for _ in 0..3 {
        let (s, _) = call(&h.app, "GET", "/v1/me", Some(u), None).await;
        assert_eq!(
            s,
            StatusCode::OK,
            "web requests send no device id and use no seat"
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

    // Return URLs: only on the site's origins.
    let w = add_user(&h.pool).await;
    let (s, v) = call(
        &h.app,
        "POST",
        "/v1/billing/checkout",
        Some(w),
        Some(json!({"plan": "heed", "interval": "month", "return_url": "https://evil.test/x"})),
    )
    .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_return_url"))
    );
    let (s, _) = call(&h.app, "POST", "/v1/billing/checkout", Some(w),
        Some(json!({"plan": "heed", "interval": "month", "return_url": "https://heed.test/account?billing=done"}))).await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = call(
        &h.app,
        "POST",
        "/v1/billing/portal",
        Some(w),
        Some(json!({"return_url": "https://heed.test.evil.test/"})),
    )
    .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_return_url"))
    );
    let (s, _) = call(
        &h.app,
        "POST",
        "/v1/billing/portal",
        Some(w),
        Some(json!({"return_url": "https://heed.test/account?billing=done"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(&h.app, "POST", "/v1/billing/portal", Some(w), None).await;
    assert_eq!(s, StatusCode::OK, "no body: app hand-off");
}

#[tokio::test]
async fn billing_off_entitles_everyone_and_closes_stripe_routes() {
    let Some((db, pool)) = crate::testkit::test_db().await else {
        return;
    };
    let (auth, _composio, llm) = fakes();
    let gateway = Gateway::new(
        Some(llm as Arc<dyn crate::gateway::Llm>),
        None,
        "claude-haiku-5-5".into(),
        "claude-sonnet-5-5".into(),
    );
    let off = |k: &str| match k {
        "NUGGET_BILLING" => Some("off".to_owned()),
        _ => None,
    };
    let plans = Catalog::parse(crate::plans::DEFAULT_TOML, off).unwrap();
    let state = AppState::new(db, auth, gateway, Arc::new(plans))
        .with_limits(crate::ratelimit::Limits::unlimited());
    let app = router(Arc::new(state));
    let u = add_user(&pool).await;
    let (s, me) = call(&app, "GET", "/v1/me", Some(u), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        (me["plan"].as_str(), me["billing"].as_bool()),
        (Some("heed_max"), Some(false))
    );
    assert_eq!(me["subscription"]["status"], "active");
    // Gated routes work with no subscription row at all.
    let (s, v) = call(
        &app,
        "POST",
        "/v1/ask",
        Some(u),
        Some(json!({"question": "anything"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    for (m, uri) in [
        ("POST", "/v1/billing/checkout"),
        ("POST", "/v1/billing/portal"),
    ] {
        let (s, v) = call(
            &app,
            m,
            uri,
            Some(u),
            Some(json!({"plan": "heed", "interval": "month"})),
        )
        .await;
        assert_eq!(
            (s, v["error"].as_str()),
            (StatusCode::NOT_FOUND, Some("billing_disabled")),
            "{uri}"
        );
    }
    let req = Request::post("/v1/webhooks/stripe")
        .body(Body::from("{}"))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    let (_, p) = call(&app, "GET", "/v1/billing/plans", None, None).await;
    assert_eq!(p["billing"], false);
}
