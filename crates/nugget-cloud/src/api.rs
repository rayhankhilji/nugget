//! The `/v1` HTTP API. Every route except the webhook and health check needs
//! `Authorization: Bearer <Supabase access token>`. Errors are `{error, message}`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::ask;
use crate::auth::{AuthError, AuthProvider, AuthUser};
use crate::composio::{Composio, ComposioError, Toolkit, map_status};
use crate::db::{Db, DbError, SubscriptionRow};
use crate::gateway::{Allowance, Gateway, GatewayError};
use crate::ingest::{self, Ingest, SyncReport};
use crate::plans::{Catalog, ModelTier, Plan};
use crate::stripe::{Stripe, StripeError};
use crate::webhook;

mod billing;

const MAX_QUESTION_CHARS: usize = 2_000;
const CATALOG_TTL: Duration = Duration::from_secs(3600);

pub struct AppState {
    pub db: Db,
    pub auth: Arc<dyn AuthProvider>,
    pub gateway: Gateway,
    pub plans: Arc<Catalog>,
    pub composio: Option<Arc<dyn Composio>>,
    /// Toolkit slug → Composio auth config id.
    pub auth_configs: BTreeMap<String, String>,
    pub webhook_secret: Option<String>,
    pub connect_callback: String,
    pub stripe: Option<Arc<dyn Stripe>>,
    pub stripe_webhook_secret: Option<String>,
    /// Public https base of this service (Stripe return pages).
    pub public_url: Option<String>,
    pub app_scheme: String,
    /// Website origins: CORS and allowed billing return URLs. Empty = CORS off.
    pub site_origins: Vec<String>,
    toolkits: Mutex<Option<(Instant, Vec<Toolkit>)>>,
}

impl AppState {
    /// A state with the required parts; set the optional public fields before sharing it.
    pub fn new(db: Db, auth: Arc<dyn AuthProvider>, gateway: Gateway, plans: Arc<Catalog>) -> Self {
        Self {
            db,
            auth,
            gateway,
            plans,
            composio: None,
            auth_configs: BTreeMap::new(),
            webhook_secret: None,
            connect_callback: "heed://connections/callback".into(),
            stripe: None,
            stripe_webhook_secret: None,
            public_url: None,
            app_scheme: "nugget".into(),
            site_origins: Vec::new(),
            toolkits: Mutex::new(None),
        }
    }

    pub fn ingest(&self) -> Option<Ingest> {
        Some(Ingest {
            db: self.db.clone(),
            composio: self.composio.clone()?,
            gateway: self.gateway.clone(),
            catalog: self.plans.clone(),
        })
    }

    fn composio(&self) -> Result<&Arc<dyn Composio>, ApiError> {
        self.composio
            .as_ref()
            .ok_or_else(|| ComposioError::NotConfigured.into())
    }
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error": self.code, "message": self.message})),
        )
            .into_response()
    }
}

impl From<DbError> for ApiError {
    fn from(e: DbError) -> Self {
        tracing::error!(%e, "database error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "database error",
        )
    }
}

impl From<GatewayError> for ApiError {
    fn from(e: GatewayError) -> Self {
        match e {
            GatewayError::QuotaExceeded => Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                "quota_exceeded",
                e.to_string(),
            ),
            GatewayError::NotConfigured => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "llm_not_configured",
                e.to_string(),
            ),
            GatewayError::Provider(m) => {
                tracing::warn!(%m, "provider error");
                Self::new(
                    StatusCode::BAD_GATEWAY,
                    "provider_error",
                    "the AI provider failed; try again",
                )
            }
            GatewayError::Db(e) => e.into(),
        }
    }
}

impl From<ComposioError> for ApiError {
    fn from(e: ComposioError) -> Self {
        match e {
            ComposioError::NotConfigured => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "composio_not_configured",
                e.to_string(),
            ),
            _ => Self::new(StatusCode::BAD_GATEWAY, "composio_error", e.to_string()),
        }
    }
}

impl From<StripeError> for ApiError {
    fn from(e: StripeError) -> Self {
        match e {
            StripeError::NotConfigured => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "billing_not_configured",
                e.to_string(),
            ),
            StripeError::Http(m) => {
                tracing::warn!(%m, "stripe error");
                Self::new(
                    StatusCode::BAD_GATEWAY,
                    "billing_error",
                    "the payment provider failed; try again",
                )
            }
        }
    }
}

impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::Invalid => {
                Self::new(StatusCode::UNAUTHORIZED, "not_signed_in", e.to_string())
            }
            AuthError::Unavailable(_) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth_unavailable",
                e.to_string(),
            ),
        }
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// A signed-in caller and their live subscription, if any.
struct Caller {
    user: AuthUser,
    subscription: Option<SubscriptionRow>,
    plan: Option<Plan>,
}

impl Caller {
    /// The plan, or 402 when there is no live subscription.
    fn entitled(&self) -> ApiResult<&Plan> {
        self.plan.as_ref().ok_or_else(|| {
            ApiError::new(
                StatusCode::PAYMENT_REQUIRED,
                "subscription_required",
                "start a Heed trial or subscription to use this",
            )
        })
    }
}

/// Signs the caller in, loads their subscription and enforces the plan's device limit.
async fn caller(s: &AppState, h: &HeaderMap) -> ApiResult<Caller> {
    let user = user(s, h).await?;
    let ent = crate::plans::entitlement(&s.db, &s.plans, user.id).await?;
    let (subscription, plan) = match ent {
        Some(e) => (Some(e.subscription), Some(e.plan)),
        None => (None, None),
    };
    // Only clients that identify themselves (the desktop app) count as devices; the website
    // and API tools send no id and never use up a seat. A fair-use limit, not DRM.
    let device = h
        .get("x-heed-device")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|d| {
            (8..=64).contains(&d.len()) && d.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    if let (Some(p), Some(device)) = (&plan, device)
        && !s.db.register_device(user.id, device, p.devices).await?
    {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "device_limit_reached",
            format!(
                "{} allows {} device(s); remove one in account settings or upgrade",
                p.name, p.devices
            ),
        ));
    }
    Ok(Caller {
        user,
        subscription,
        plan,
    })
}

async fn user(state: &AppState, headers: &HeaderMap) -> ApiResult<AuthUser> {
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or(AuthError::Invalid)?;
    Ok(state.auth.verify(token.trim()).await?)
}

pub fn router(state: Arc<AppState>) -> Router {
    let cors = cors_layer(&state.site_origins);
    let router = routes(state);
    match cors {
        Some(c) => router.layer(c),
        None => router,
    }
}

/// CORS for the website: listed origins only, bearer auth (no cookies), preflight handled.
fn cors_layer(origins: &[String]) -> Option<tower_http::cors::CorsLayer> {
    use axum::http::{HeaderValue, Method, header};
    let list: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();
    (!list.is_empty()).then(|| {
        tower_http::cors::CorsLayer::new()
            .allow_origin(list)
            .allow_methods([Method::GET, Method::POST, Method::DELETE])
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
            .max_age(Duration::from_secs(3600))
    })
}

fn routes(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/me", get(me).delete(delete_me))
        .route("/v1/connections", get(list_connections).post(connect))
        .route("/v1/connections/{id}", delete(disconnect))
        .route("/v1/sync", post(sync))
        .route("/v1/ask", post(ask_route))
        .route("/v1/export", get(export))
        .route("/v1/webhooks/composio", post(composio_webhook))
        .route("/v1/billing/plans", get(billing::plans))
        .route("/v1/billing/checkout", post(billing::checkout))
        .route("/v1/billing/portal", post(billing::portal))
        .route(
            "/v1/webhooks/stripe",
            // Stripe events can exceed a small default body limit.
            post(billing::stripe_webhook).layer(axum::extract::DefaultBodyLimit::max(256 * 1024)),
        )
        .route("/billing/done", get(billing::done))
        .with_state(state)
}

async fn me(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let c = caller(&s, &h).await?;
    s.db.touch_profile(c.user.id).await?;
    let cost = s.db.month_cost_micros(c.user.id).await?;
    let smart =
        s.db.month_model_calls(c.user.id, s.gateway.model_for(ModelTier::Smart))
            .await?;
    Ok(Json(json!({
        "user_id": c.user.id,
        "email": c.user.email,
        "plan": c.subscription.as_ref().map(|x| &x.plan),
        "embeddings": s.gateway.has_embeddings(),
        "subscription": c.subscription,
        "entitlements": c.subscription.as_ref().zip(c.plan.as_ref()).map(|(x, p)| billing::entitlements(&x.plan, p)),
        "usage": {"month_cost_micros": cost, "smart_asks_used": smart},
    })))
}

async fn delete_me(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<StatusCode> {
    let u = user(&s, &h).await?;
    // Revoke Google access at Composio first; if that fails, keep the account so the user can retry.
    if let Some(c) = &s.composio {
        for acc in c.accounts(&u.id.to_string()).await? {
            c.delete_account(&acc.id).await?;
        }
    }
    // Then Stripe: deleting the customer cancels any subscription at once (no further charges).
    // Invoices stay in Stripe for HMRC; our billing rows go with the user. Abort on failure.
    if let Some(customer) = s.db.customer(u.id).await? {
        let stripe = s.stripe.as_ref().ok_or(StripeError::NotConfigured)?;
        stripe.delete_customer(&customer).await?;
    }
    s.auth.delete_user(u.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct Available {
    slug: String,
    name: String,
    logo: Option<String>,
    connectable: bool,
}

impl AppState {
    /// Composio's catalogue, cached; falls back to what we support if Composio is unreachable.
    async fn catalog(&self) -> Vec<Toolkit> {
        if let Some((at, list)) = self
            .toolkits
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            && at.elapsed() < CATALOG_TTL
        {
            return list.clone();
        }
        let fetched = match &self.composio {
            Some(c) => c.toolkits().await.ok(),
            None => None,
        };
        match fetched {
            Some(list) => {
                *self.toolkits.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some((Instant::now(), list.clone()));
                list
            }
            None => ingest::SUPPORTED
                .iter()
                .map(|s| Toolkit {
                    slug: (*s).into(),
                    name: (*s).into(),
                    logo: None,
                })
                .collect(),
        }
    }

    fn connectable(&self, slug: &str) -> bool {
        ingest::SUPPORTED.contains(&slug) && self.auth_configs.contains_key(slug)
    }
}

async fn list_connections(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let u = user(&s, &h).await?;
    // Pick up accounts whose OAuth finished since the last look.
    if let Some(c) = &s.composio {
        match c.accounts(&u.id.to_string()).await {
            Ok(accounts) => {
                for a in accounts
                    .iter()
                    .filter(|a| ingest::SUPPORTED.contains(&a.toolkit.as_str()))
                {
                    s.db.upsert_connection(u.id, &a.toolkit, &a.id, map_status(&a.status))
                        .await?;
                }
            }
            Err(e) => tracing::warn!(%e, "could not refresh connected accounts"),
        }
    }
    let connections = s.db.connections(u.id).await?;
    let mut available: Vec<Available> = s
        .catalog()
        .await
        .into_iter()
        .map(|t| Available {
            connectable: s.connectable(&t.slug),
            slug: t.slug,
            name: t.name,
            logo: t.logo,
        })
        .collect();
    available.sort_by(|a, b| b.connectable.cmp(&a.connectable).then(a.name.cmp(&b.name)));
    available.truncate(60);
    Ok(Json(
        json!({"connections": connections, "available": available}),
    ))
}

#[derive(Deserialize)]
struct ConnectBody {
    toolkit: String,
}

async fn connect(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(b): Json<ConnectBody>,
) -> ApiResult<Json<Value>> {
    let c = caller(&s, &h).await?;
    let plan = c.entitled()?;
    let u = &c.user;
    let slug = b.toolkit.to_ascii_lowercase();
    if let Some(max) = plan.max_connections {
        let live =
            s.db.connections(u.id)
                .await?
                .iter()
                .filter(|x| x.status != "failed" && x.toolkit != slug)
                .count();
        if live >= max as usize {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "connection_limit_reached",
                format!("{} allows {max} connected apps", plan.name),
            ));
        }
    }
    if !s.connectable(&slug) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "unsupported_toolkit",
            format!("{slug} cannot be connected yet"),
        ));
    }
    let link = s
        .composio()?
        .create_link(
            &u.id.to_string(),
            &s.auth_configs[&slug],
            &s.connect_callback,
        )
        .await?;
    s.db.upsert_connection(u.id, &slug, &link.connected_account_id, "initiated")
        .await?;
    Ok(Json(json!({"redirect_url": link.redirect_url})))
}

async fn disconnect(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let u = user(&s, &h).await?;
    let Some(account) =
        s.db.connections(u.id)
            .await?
            .into_iter()
            .find(|c| c.id == id)
    else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such connection",
        ));
    };
    s.composio()?
        .delete_account(&account.composio_account_id)
        .await?;
    s.db.delete_connection(u.id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn sync(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let c = caller(&s, &h).await?;
    c.entitled()?;
    let u = c.user;
    let ingest = s.ingest().ok_or(ComposioError::NotConfigured)?;
    s.db.mark_due(u.id, None).await?;
    let mut synced: Vec<SyncReport> = Vec::new();
    for c in
        s.db.connections(u.id)
            .await?
            .into_iter()
            .filter(|c| c.status == "active")
    {
        synced.push(ingest.sync(u.id, &c).await);
    }
    Ok(Json(json!({"synced": synced})))
}

#[derive(Deserialize)]
struct AskBody {
    question: String,
    conversation_id: Option<Uuid>,
    model: Option<ModelTier>,
}

async fn ask_route(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(b): Json<AskBody>,
) -> ApiResult<Json<ask::AskResponse>> {
    let c = caller(&s, &h).await?;
    let plan = c.entitled()?;
    let q = b.question.trim();
    if q.is_empty() || q.chars().count() > MAX_QUESTION_CHARS {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_question",
            "ask a question of 1 to 2000 characters",
        ));
    }
    s.db.touch_profile(c.user.id).await?;
    let allowance = Allowance::from(plan);
    Ok(Json(
        ask::ask(
            &s.db,
            &s.gateway,
            c.user.id,
            allowance,
            b.model,
            q,
            b.conversation_id,
        )
        .await?,
    ))
}

async fn export(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let u = user(&s, &h).await?;
    Ok(Json(s.db.export(u.id).await?))
}

async fn composio_webhook(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let Some(secret) = &s.webhook_secret else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "webhooks are not enabled",
        ));
    };
    let hv = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).unwrap_or("");
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    if !webhook::verify(
        secret,
        hv("webhook-id"),
        hv("webhook-timestamp"),
        &body,
        hv("webhook-signature"),
        now,
    ) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "bad_signature",
            "signature check failed",
        ));
    }
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    // The payload is only a nudge: mark that connection due; the sync fetches the data itself.
    if let Some(acc) = v
        .pointer("/metadata/connected_account_id")
        .and_then(Value::as_str)
        && let Some((user, conn)) = s.db.connection_owner(acc).await?
    {
        s.db.mark_due(user, Some(conn)).await?;
    }
    Ok(StatusCode::OK)
}

#[cfg(test)]
mod tests;
