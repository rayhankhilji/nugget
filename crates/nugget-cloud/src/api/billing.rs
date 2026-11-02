//! Billing routes: plans, Stripe Checkout and portal, the Stripe webhook, the return page.
//!
//! Stripe is the source of truth. Every relevant webhook event makes us fetch the current
//! subscription from Stripe and store it, so duplicate or out-of-order events converge on the
//! same row, and processed event ids are recorded so repeats are acknowledged without work.

use axum::response::Html;
use serde_json::Value;

use super::*;
use crate::db::SubscriptionRow;
use crate::plans::Interval;
use crate::stripe::{CheckoutRequest, verify_signature};

const SIGNATURE_TOLERANCE_SECS: i64 = 300;

/// The entitlements a client may show and gate on.
pub(super) fn entitlements(id: &str, p: &Plan) -> Value {
    json!({
        "plan": id,
        "name": p.name,
        "max_connections": p.max_connections,
        "sync_interval_minutes": p.sync_interval_minutes,
        "history_days": p.history_days,
        "smart_asks_per_month": p.smart_asks_per_month,
        "default_model": p.default_model,
        "devices": p.devices,
        "ghost_text": p.ghost_text,
        "needs_you_inbox": p.needs_you_inbox,
        "daily_brief": p.daily_brief,
        "actions_with_approval": p.actions_with_approval,
        "agent_tasks": p.agent_tasks,
        "computer_use": p.computer_use,
        "priority_support": p.priority_support,
    })
}

pub(super) async fn plans(State(s): State<Arc<AppState>>) -> Json<Value> {
    let plans: Vec<Value> = s
        .plans
        .plans
        .iter()
        .map(|(id, p)| {
            json!({
                "id": id,
                "name": p.name,
                "monthly_gbp": p.monthly_gbp,
                "annual_gbp": p.annual_gbp,
                "trial_days": s.plans.trial_days,
                "entitlements": entitlements(id, p),
            })
        })
        .collect();
    Json(json!({"plans": plans}))
}

fn stripe(s: &AppState) -> ApiResult<&Arc<dyn Stripe>> {
    s.stripe
        .as_ref()
        .ok_or_else(|| StripeError::NotConfigured.into())
}

fn return_url(s: &AppState) -> ApiResult<String> {
    s.public_url
        .as_ref()
        .map(|u| format!("{u}/billing/done"))
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "billing_not_configured",
                "NUGGET_PUBLIC_URL is not set",
            )
        })
}

#[derive(Deserialize)]
pub(super) struct CheckoutBody {
    plan: String,
    interval: Interval,
}

pub(super) async fn checkout(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(b): Json<CheckoutBody>,
) -> ApiResult<Json<Value>> {
    let c = caller(&s, &h).await?;
    if c.subscription.is_some() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "already_subscribed",
            "you already have a plan; change it in Manage billing",
        ));
    }
    if s.plans.get(&b.plan).is_none() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "unknown_plan",
            format!("no plan {:?}", b.plan),
        ));
    }
    let price = s.plans.price_id(&b.plan, b.interval).ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "billing_not_configured",
            "no Stripe price for that plan",
        )
    })?;
    let stripe = stripe(&s)?;
    let url = return_url(&s)?;
    let uid = c.user.id.to_string();
    let customer = match s.db.customer(c.user.id).await? {
        Some(cus) => cus,
        None => {
            let cus = stripe
                .create_customer(c.user.email.as_deref(), &uid)
                .await?;
            s.db.set_customer(c.user.id, &cus).await?;
            cus
        }
    };
    // One trial per user: none if they ever had a subscription.
    let had_one = !s.db.subscriptions(c.user.id).await?.is_empty();
    let checkout_url = stripe
        .checkout(&CheckoutRequest {
            customer: &customer,
            price,
            user_id: &uid,
            trial_days: (!had_one).then_some(s.plans.trial_days),
            success_url: &url,
            cancel_url: &url,
        })
        .await?;
    Ok(Json(json!({"url": checkout_url})))
}

pub(super) async fn portal(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let c = caller(&s, &h).await?;
    let Some(customer) = s.db.customer(c.user.id).await? else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "no_customer",
            "start a trial first",
        ));
    };
    let url = stripe(&s)?.portal(&customer, &return_url(&s)?).await?;
    Ok(Json(json!({"url": url})))
}

/// Where Stripe sends the browser back: hands over to the app.
pub(super) async fn done(State(s): State<Arc<AppState>>) -> Html<String> {
    let link = format!("{}://billing/done", s.app_scheme);
    Html(format!(
        "<!doctype html><meta charset=utf-8><title>Heed</title>\
         <meta http-equiv=refresh content=\"0;url={link}\">\
         <p>All set. <a href=\"{link}\">Return to Heed</a>; you can close this tab.</p>"
    ))
}

/// The subscription id an event is about, if it is one we act on.
pub fn subscription_of(event_type: &str, obj: &Value) -> Option<String> {
    let s = |v: &Value| v.as_str().map(str::to_owned);
    match event_type {
        "checkout.session.completed" => s(&obj["subscription"]),
        "customer.subscription.created"
        | "customer.subscription.updated"
        | "customer.subscription.deleted" => s(&obj["id"]),
        // `invoice.subscription` moved under `parent.subscription_details` in newer API versions.
        "invoice.payment_failed" | "invoice.paid" => s(&obj["subscription"])
            .or_else(|| s(&obj["parent"]["subscription_details"]["subscription"])),
        _ => None,
    }
}

pub(super) async fn stripe_webhook(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let Some(secret) = &s.stripe_webhook_secret else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "Stripe webhooks are not enabled",
        ));
    };
    let sig = h
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    if !verify_signature(secret, sig, &body, now, SIGNATURE_TOLERANCE_SECS) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_signature",
            "signature check failed",
        ));
    }
    let event: Value = serde_json::from_slice(&body)
        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "bad_event", "not JSON"))?;
    let (Some(id), Some(kind)) = (event["id"].as_str(), event["type"].as_str()) else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_event",
            "no id or type",
        ));
    };
    if s.db.stripe_event_seen(id).await? {
        return Ok(StatusCode::OK);
    }
    let obj = &event["data"]["object"];
    if let Some(sub_id) = subscription_of(kind, obj) {
        let fallback_user = obj["client_reference_id"].as_str();
        apply_subscription(&s, &sub_id, fallback_user).await?;
    }
    s.db.mark_stripe_event(id, kind).await?;
    Ok(StatusCode::OK)
}

/// Fetches the subscription from Stripe and stores it for its owner. Errors make the webhook
/// fail, so Stripe retries later (e.g. after a missing price id is configured).
async fn apply_subscription(
    s: &AppState,
    sub_id: &str,
    fallback_user: Option<&str>,
) -> ApiResult<()> {
    let sub = stripe(s)?.subscription(sub_id).await?;
    let owner = match s.db.customer_owner(&sub.customer).await? {
        Some(u) => u,
        None => {
            // Customer unknown (created outside Checkout from this app): trust only our own
            // metadata, then remember the customer.
            let u: Uuid = sub
                .user_id
                .as_deref()
                .or(fallback_user)
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| {
                    ApiError::new(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "unknown_customer",
                        "no owner for customer",
                    )
                })?;
            s.db.set_customer(u, &sub.customer).await?;
            u
        }
    };
    let Some((plan, interval)) = s.plans.by_price(&sub.price_id) else {
        tracing::error!(price = %sub.price_id, "subscription on a price not in plans.toml / env");
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "unknown_price",
            "price not configured",
        ));
    };
    let row = SubscriptionRow {
        id: sub.id.clone(),
        plan: plan.to_owned(),
        interval: match interval {
            Interval::Month => "month",
            Interval::Year => "year",
        }
        .to_owned(),
        status: sub.status.clone(),
        trial_end: sub.trial_end,
        current_period_end: sub.current_period_end,
        cancel_at_period_end: sub.cancel_at_period_end,
    };
    s.db.upsert_subscription(owner, &row, &sub.price_id).await?;
    tracing::info!(plan, status = %sub.status, "subscription stored");
    Ok(())
}
