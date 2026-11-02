//! The `/v1` HTTP API. Every route except the webhook and health check needs
//! `Authorization: Bearer <Supabase access token>`. Errors are `{error, message}`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::ask;
use crate::auth::{AuthError, AuthProvider, AuthUser};
use crate::composio::{Composio, ComposioError, Toolkit, map_status};
use crate::db::{Db, DbError};
use crate::gateway::{Gateway, GatewayError, Plan};
use crate::ingest::{self, Ingest, SyncReport};
use crate::ops::{self, METRICS};
use crate::ratelimit::{Limits, RateLimiter};
use crate::webhook::{self, ReplayGuard};

const MAX_QUESTION_CHARS: usize = 2_000;
const CATALOG_TTL: Duration = Duration::from_secs(3600);
/// Largest request body for the API (questions are 2,000 characters).
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Largest Composio webhook body.
pub const MAX_WEBHOOK_BYTES: usize = 256 * 1024;

/// The rate limiters behind [`Limits`].
pub struct Limiters {
    pub ip: RateLimiter<std::net::IpAddr>,
    pub user: RateLimiter<Uuid>,
    pub ask: RateLimiter<Uuid>,
    pub sync: RateLimiter<Uuid>,
    /// `GET /v1/export` builds the whole corpus in memory: once a minute per user.
    pub export: RateLimiter<Uuid>,
}

impl Limiters {
    pub fn new(l: Limits) -> Self {
        Self {
            ip: RateLimiter::per_minute(l.ip_per_min),
            user: RateLimiter::per_minute(l.user_per_min),
            ask: RateLimiter::per_minute(l.ask_per_min),
            sync: RateLimiter::every(l.sync_every_secs),
            export: RateLimiter::every(if l.user_per_min == 0 { 0 } else { 60 }),
        }
    }
}

pub struct AppState {
    pub db: Db,
    pub auth: Arc<dyn AuthProvider>,
    pub composio: Option<Arc<dyn Composio>>,
    pub gateway: Gateway,
    /// Toolkit slug → Composio auth config id.
    pub auth_configs: BTreeMap<String, String>,
    pub webhook_secret: Option<String>,
    pub connect_callback: String,
    catalog: Mutex<Option<(Instant, Vec<Toolkit>)>>,
    pub limits: Limiters,
    /// Header with the real client IP set by the edge proxy; `None` uses the TCP peer.
    pub client_ip_header: Option<String>,
    replay: ReplayGuard,
}

impl AppState {
    pub fn new(
        db: Db,
        auth: Arc<dyn AuthProvider>,
        composio: Option<Arc<dyn Composio>>,
        gateway: Gateway,
        auth_configs: BTreeMap<String, String>,
        webhook_secret: Option<String>,
        connect_callback: String,
    ) -> Self {
        Self {
            db,
            auth,
            composio,
            gateway,
            auth_configs,
            webhook_secret,
            connect_callback,
            catalog: Mutex::new(None),
            limits: Limiters::new(Limits::default()),
            client_ip_header: Some("fly-client-ip".into()),
            replay: ReplayGuard::default(),
        }
    }

    /// Replaces the default rate limits.
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = Limiters::new(limits);
        self
    }

    /// Which header carries the client IP (`None`: the TCP peer address).
    pub fn with_client_ip_header(mut self, header: Option<String>) -> Self {
        self.client_ip_header = header;
        self
    }

    pub fn ingest(&self) -> Option<Ingest> {
        Some(Ingest {
            db: self.db.clone(),
            composio: self.composio.clone()?,
            gateway: self.gateway.clone(),
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
    retry_after: Option<Duration>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retry_after: None,
        }
    }

    /// 429 with `Retry-After`.
    pub fn rate_limited(scope: &'static str, wait: Duration) -> Self {
        METRICS.rate_limited(scope);
        Self {
            retry_after: Some(wait),
            ..Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "too many requests; slow down and try again shortly",
            )
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut res = (
            self.status,
            Json(json!({"error": self.code, "message": self.message})),
        )
            .into_response();
        if let Some(w) = self.retry_after {
            let secs = w.as_secs() + u64::from(w.subsec_nanos() > 0);
            res.headers_mut().insert(
                "retry-after",
                secs.max(1).to_string().parse().expect("digits"),
            );
        }
        res
    }
}

impl From<DbError> for ApiError {
    fn from(e: DbError) -> Self {
        if e.is_unavailable() {
            tracing::warn!(%e, "database unavailable");
            return Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "busy",
                "the service is busy; try again shortly",
            );
        }
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
            // The upstream message stays in our logs: it can describe Composio's internals.
            _ => {
                tracing::warn!(%e, "composio error");
                Self::new(
                    StatusCode::BAD_GATEWAY,
                    "composio_error",
                    "the connected-apps service failed; try again",
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
            AuthError::Unavailable(m) => {
                tracing::warn!(%m, "auth unavailable");
                Self::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "auth_unavailable",
                    "sign-in service unavailable; try again",
                )
            }
        }
    }
}

type ApiResult<T> = Result<T, ApiError>;

async fn user(state: &AppState, headers: &HeaderMap) -> ApiResult<AuthUser> {
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or(AuthError::Invalid)?;
    let u = state.auth.verify(token.trim()).await?;
    state
        .limits
        .user
        .check(&u.id)
        .map_err(|w| ApiError::rate_limited("user", w))?;
    Ok(u)
}

/// The client address: the edge proxy's header when configured and present, else the peer.
fn client_ip(state: &AppState, req: &Request) -> Option<std::net::IpAddr> {
    let from_header = state.client_ip_header.as_ref().and_then(|h| {
        req.headers()
            .get(h.as_str())?
            .to_str()
            .ok()?
            .split(',')
            .next()?
            .trim()
            .parse()
            .ok()
    });
    from_header.or_else(|| {
        req.extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|c| c.0.ip())
    })
}

/// Per-IP limit on everything but the health checks.
async fn ip_limit(State(s): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    if !matches!(req.uri().path(), "/healthz" | "/readyz")
        && let Some(ip) = client_ip(&s, &req)
        && let Err(w) = s.limits.ip.check(&ip)
    {
        return ApiError::rate_limited("ip", w).into_response();
    }
    next.run(req).await
}

async fn readyz(State(s): State<Arc<AppState>>) -> ApiResult<&'static str> {
    match tokio::time::timeout(Duration::from_secs(2), s.db.ping()).await {
        Ok(Ok(())) => Ok("ready"),
        _ => Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "not_ready",
            "database unreachable",
        )),
    }
}

/// `/metrics` for the internal listener only (never mount it on the public router).
pub fn metrics_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            "/metrics",
            get(|State(s): State<Arc<AppState>>| async move {
                (
                    [("content-type", "text/plain; version=0.0.4")],
                    METRICS.render(Some(s.db.pool_status())),
                )
            }),
        )
        .with_state(state)
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(readyz))
        .route("/v1/me", get(me).delete(delete_me))
        .route("/v1/connections", get(list_connections).post(connect))
        .route("/v1/connections/{id}", delete(disconnect))
        .route("/v1/sync", post(sync))
        .route("/v1/ask", post(ask_route))
        .route("/v1/export", get(export))
        .route(
            "/v1/webhooks/composio",
            post(composio_webhook).layer(DefaultBodyLimit::max(MAX_WEBHOOK_BYTES)),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), ip_limit))
        .layer(middleware::from_fn(ops::deadline))
        .layer(middleware::from_fn(ops::observe))
        .with_state(state)
}

async fn me(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let u = user(&s, &h).await?;
    let plan = Plan::parse(&s.db.touch_profile(u.id).await?);
    let cost = s.db.month_cost_micros(u.id).await?;
    Ok(Json(json!({
        "user_id": u.id,
        "email": u.email,
        "plan": if plan == Plan::Pro { "pro" } else { "free" },
        "embeddings": s.gateway.has_embeddings(),
        "usage": {"month_cost_micros": cost, "quota_micros": plan.quota_micros()},
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
            .catalog
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
                *self.catalog.lock().unwrap_or_else(|p| p.into_inner()) =
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
    let u = user(&s, &h).await?;
    let slug = b.toolkit.to_ascii_lowercase();
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
    let u = user(&s, &h).await?;
    let ingest = s.ingest().ok_or(ComposioError::NotConfigured)?;
    s.limits
        .sync
        .check(&u.id)
        .map_err(|w| ApiError::rate_limited("sync", w))?;
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
}

async fn ask_route(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Json(b): Json<AskBody>,
) -> ApiResult<Json<ask::AskResponse>> {
    let u = user(&s, &h).await?;
    let q = b.question.trim();
    if q.is_empty() || q.chars().count() > MAX_QUESTION_CHARS {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_question",
            "ask a question of 1 to 2000 characters",
        ));
    }
    s.limits
        .ask
        .check(&u.id)
        .map_err(|w| ApiError::rate_limited("ask", w))?;
    let plan = Plan::parse(&s.db.touch_profile(u.id).await?);
    Ok(Json(
        ask::ask(&s.db, &s.gateway, u.id, plan, q, b.conversation_id).await?,
    ))
}

async fn export(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let u = user(&s, &h).await?;
    s.limits
        .export
        .check(&u.id)
        .map_err(|w| ApiError::rate_limited("export", w))?;
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
    // A replayed delivery (same id, timestamp still in tolerance) is acknowledged, not acted on.
    if !s.replay.first_seen(hv("webhook-id"), now) {
        return Ok(StatusCode::OK);
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
