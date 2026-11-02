//! Heed's streaming model endpoint: an **Anthropic-Messages-compatible** API for the desktop.
//!
//! - `POST /llm/v1/messages` takes an Anthropic Messages body (streaming or not, with tools,
//!   images and `cache_control`), authenticated with the user's Supabase access token
//!   (`Authorization: Bearer …`, or `x-api-key: …` so a stock Anthropic client works with the
//!   base URL `{api}/llm`). `model` names a tier, not a model: `heed-fast` (ghost text, triage,
//!   light work) or `heed-smart` (asks, drafting, the agent). The service picks the real model
//!   from the tier and the user's [`entitlements`].
//! - `GET /llm/v1/models` lists the two tiers. The desktop calls it to open (prewarm) its
//!   connection; it also warms this service's own connection to the provider when that has
//!   been idle.
//!
//! **Why Anthropic-compatible rather than OpenAI-compatible:** Anthropic is the primary
//! provider, so the primary path forwards the client's body (after checks) and relays the
//! provider's bytes unchanged: no translation on the latency-critical path, and prompt caching
//! (`cache_control` breakpoints, `max_tokens: 0` prefix warming), tool use and images keep their
//! exact semantics. The desktop's `nugget-infer` already speaks this API, including those
//! caching features. Only the fallback (OpenAI, used when Anthropic is unset or fails before
//! the first byte) is translated ([`openai`]).
//!
//! Every call is metered into `usage_events` (route, tokens, prompt-cache tokens, cost) and
//! checked against the month's quota first. A client that disconnects drops the upstream
//! request at once (the provider stops generating) and the tokens seen so far are metered.
//! Prompt contents are never logged: logs carry the user id, tier, model, sizes, status and
//! timings only.

pub mod entitlements;
mod openai;
mod sse;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::api::AppState;
use crate::auth::{AuthError, AuthUser};
use crate::config::Config;
use crate::db::DbError;
use crate::gateway::price_per_mtok;
pub use entitlements::{Entitlements, entitlements};

/// The `model` value for the fast tier.
pub const FAST: &str = "heed-fast";
/// The `model` value for the smart tier.
pub const SMART: &str = "heed-smart";

/// Anthropic API version sent upstream when the client names none.
const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";
/// Largest upstream error body relayed to the client.
const MAX_ERROR_BODY: usize = 64 * 1024;
/// Largest non-streaming upstream body accepted.
const MAX_JSON_BODY: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmTier {
    Fast,
    Smart,
}

impl LlmTier {
    pub fn parse(model: &str) -> Option<Self> {
        match model {
            FAST => Some(Self::Fast),
            SMART => Some(Self::Smart),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fast => FAST,
            Self::Smart => SMART,
        }
    }
}

/// One upstream provider. `Debug` hides the key.
#[derive(Clone)]
pub struct Upstream {
    pub base: String,
    pub key: String,
    pub fast: String,
    pub smart: String,
}

impl std::fmt::Debug for Upstream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upstream")
            .field("base", &self.base)
            .field("fast", &self.fast)
            .field("smart", &self.smart)
            .finish_non_exhaustive()
    }
}

impl Upstream {
    fn model(&self, tier: LlmTier, ent: &Entitlements) -> &str {
        match tier {
            LlmTier::Smart if ent.smart_model => &self.smart,
            _ => &self.fast,
        }
    }
}

/// Request limits and timeouts.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Largest request body (images included).
    pub max_body_bytes: usize,
    /// Most messages in one request.
    pub max_messages: usize,
    /// Wait for the provider's response headers (time to first byte).
    pub headers_timeout: Duration,
    /// Longest silence between two chunks of a stream (providers send pings).
    pub idle_timeout: Duration,
    /// Longest a whole request may take.
    pub total_timeout: Duration,
    /// How long entitlements and month-to-date usage are reused before re-reading them.
    pub usage_ttl: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_body_bytes: 8 * 1024 * 1024,
            max_messages: 4_000,
            headers_timeout: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(90),
            total_timeout: Duration::from_secs(15 * 60),
            usage_ttl: Duration::from_secs(15),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LlmConfig {
    /// Primary (Anthropic Messages).
    pub anthropic: Option<Upstream>,
    /// Fallback (OpenAI Chat Completions).
    pub openai: Option<Upstream>,
    pub limits: Limits,
}

impl LlmConfig {
    /// From the service config: Anthropic models from `NUGGET_MODEL_FAST` / `NUGGET_MODEL_SMART`
    /// (as the gateway); OpenAI's from `NUGGET_OPENAI_MODEL_FAST` / `NUGGET_OPENAI_MODEL_SMART`
    /// when Anthropic is also set, else from the same two variables.
    pub fn from_config(cfg: &Config, get: impl Fn(&str) -> Option<String>) -> Self {
        let opt = |k: &str| {
            get(k)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let anthropic = cfg.anthropic_api_key.as_ref().map(|key| Upstream {
            base: cfg.anthropic_base.trim_end_matches('/').to_owned(),
            key: key.clone(),
            fast: cfg
                .model_fast
                .clone()
                .unwrap_or_else(|| "claude-haiku-5-5".into()),
            smart: cfg
                .model_smart
                .clone()
                .unwrap_or_else(|| "claude-sonnet-5-5".into()),
        });
        let (fast_var, smart_var) = if anthropic.is_some() {
            (
                opt("NUGGET_OPENAI_MODEL_FAST"),
                opt("NUGGET_OPENAI_MODEL_SMART"),
            )
        } else {
            (cfg.model_fast.clone(), cfg.model_smart.clone())
        };
        let openai = cfg.openai_api_key.as_ref().map(|key| Upstream {
            base: cfg.openai_base.trim_end_matches('/').to_owned(),
            key: key.clone(),
            fast: fast_var.unwrap_or_else(|| "gpt-5-nano".into()),
            smart: smart_var.unwrap_or_else(|| "gpt-5-mini".into()),
        });
        Self {
            anthropic,
            openai,
            limits: Limits::default(),
        }
    }
}

/// The HTTP client for provider calls: pooled, HTTP/2 with keep-alive pings, no overall
/// timeout (streams are bounded by [`Limits`] instead), no redirects.
pub fn upstream_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(120))
        .tcp_nodelay(true)
        .tcp_keepalive(Duration::from_secs(60))
        .http2_keep_alive_interval(Duration::from_secs(30))
        .http2_keep_alive_timeout(Duration::from_secs(10))
        .http2_keep_alive_while_idle(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

struct CachedUsage {
    at: Instant,
    ent: Entitlements,
    month_cost: i64,
}

pub struct LlmState {
    pub app: Arc<AppState>,
    pub cfg: LlmConfig,
    http: reqwest::Client,
    usage: Mutex<HashMap<Uuid, CachedUsage>>,
    inflight: Mutex<HashMap<Uuid, usize>>,
    last_upstream: Mutex<Option<Instant>>,
}

impl LlmState {
    pub fn new(app: Arc<AppState>, cfg: LlmConfig, http: reqwest::Client) -> Self {
        Self {
            app,
            cfg,
            http,
            usage: Mutex::new(HashMap::new()),
            inflight: Mutex::new(HashMap::new()),
            last_upstream: Mutex::new(None),
        }
    }

    pub fn configured(&self) -> bool {
        self.cfg.anthropic.is_some() || self.cfg.openai.is_some()
    }

    /// Entitlements and month-to-date cost, reused for [`Limits::usage_ttl`] so a keystroke's
    /// request does not wait for the database. Costs metered meanwhile are added locally, so
    /// the quota can only be overshot by requests already in flight.
    async fn usage(&self, user: Uuid) -> Result<(Entitlements, i64), DbError> {
        if let Some(c) = lock(&self.usage).get(&user)
            && c.at.elapsed() < self.cfg.limits.usage_ttl
        {
            return Ok((c.ent, c.month_cost));
        }
        let db = &self.app.db;
        let (ent, month_cost) =
            tokio::try_join!(entitlements(db, user), db.month_cost_micros(user))?;
        let mut cache = lock(&self.usage);
        if cache.len() > 50_000 {
            cache.clear();
        }
        cache.insert(
            user,
            CachedUsage {
                at: Instant::now(),
                ent,
                month_cost,
            },
        );
        Ok((ent, month_cost))
    }

    fn add_cost(&self, user: Uuid, cost: i64) {
        if let Some(c) = lock(&self.usage).get_mut(&user) {
            c.month_cost += cost;
        }
    }

    fn acquire(self: &Arc<Self>, user: Uuid, max: usize) -> Option<Slot> {
        let mut m = lock(&self.inflight);
        let n = m.entry(user).or_insert(0);
        if *n >= max {
            return None;
        }
        *n += 1;
        Some(Slot {
            state: self.clone(),
            user,
        })
    }

    /// Streams this user has open now.
    pub fn in_flight(&self, user: Uuid) -> usize {
        lock(&self.inflight).get(&user).copied().unwrap_or(0)
    }

    fn touch_upstream(&self) -> bool {
        let mut last = lock(&self.last_upstream);
        let idle = last.is_none_or(|t| t.elapsed() > Duration::from_secs(60));
        *last = Some(Instant::now());
        idle
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// One open request of a user; released on drop.
struct Slot {
    state: Arc<LlmState>,
    user: Uuid,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut m = lock(&self.state.inflight);
        if let Some(n) = m.get_mut(&self.user) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&self.user);
            }
        }
    }
}

pub fn router(state: Arc<LlmState>) -> Router {
    Router::new()
        .route("/llm/v1/messages", post(messages))
        .route("/llm/v1/models", get(models))
        .layer(axum::extract::DefaultBodyLimit::max(
            state.cfg.limits.max_body_bytes,
        ))
        .with_state(state)
}

/// An Anthropic-shaped error: `{"type":"error","error":{"type","message"}}`, plus Heed's own
/// `code` where the client should act on it (`quota_exceeded`, `not_signed_in`).
fn error(status: StatusCode, kind: &str, message: &str, code: Option<&str>) -> Response {
    let mut e = json!({"type": kind, "message": message});
    if let Some(c) = code {
        e["code"] = json!(c);
    }
    (status, Json(json!({"type": "error", "error": e}))).into_response()
}

fn bad_request(message: &str) -> Response {
    error(
        StatusCode::BAD_REQUEST,
        "invalid_request_error",
        message,
        None,
    )
}

async fn authed(s: &LlmState, h: &HeaderMap) -> Result<AuthUser, Response> {
    let value = |k: &str| h.get(k).and_then(|v| v.to_str().ok());
    let token = value("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| value("x-api-key"))
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let Some(token) = token else {
        return Err(error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "sign in to Heed",
            Some("not_signed_in"),
        ));
    };
    match s.app.auth.verify(token).await {
        Ok(u) => Ok(u),
        Err(AuthError::Invalid) => Err(error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "sign in to Heed",
            Some("not_signed_in"),
        )),
        Err(AuthError::Unavailable(e)) => {
            tracing::warn!(%e, "llm: auth service unavailable");
            Err(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "api_error",
                "the sign-in service is unavailable; try again",
                None,
            ))
        }
    }
}

async fn models(State(s): State<Arc<LlmState>>, h: HeaderMap) -> Response {
    if let Err(r) = authed(&s, &h).await {
        return r;
    }
    // The desktop prewarms with this call; if our own provider connection has gone idle, open
    // it too (a bodiless, unbilled request), so the real request finds both hops warm.
    if let Some(a) = s.cfg.anthropic.clone()
        && s.touch_upstream()
    {
        let http = s.http.clone();
        tokio::spawn(async move {
            let r = http
                .get(format!("{}/v1/models?limit=1", a.base))
                .header("x-api-key", &a.key)
                .header("anthropic-version", DEFAULT_ANTHROPIC_VERSION)
                .send()
                .await;
            if let Ok(r) = r {
                let _ = r.bytes().await;
            }
        });
    }
    Json(json!({
        "data": [
            {"type": "model", "id": FAST, "display_name": "Heed fast"},
            {"type": "model", "id": SMART, "display_name": "Heed smart"},
        ],
        "has_more": false,
    }))
    .into_response()
}

/// A checked request, ready to send (with the model still a tier alias).
#[derive(Debug)]
pub(crate) struct Checked {
    pub tier: LlmTier,
    pub stream: bool,
    pub body: Map<String, Value>,
}

/// Fields forwarded upstream. Anything else (server tools, containers, MCP servers, service
/// tiers, betas) is dropped: those are cost and capability knobs the service does not offer.
const ALLOWED_FIELDS: &[&str] = &[
    "model",
    "messages",
    "system",
    "max_tokens",
    "stream",
    "tools",
    "tool_choice",
    "stop_sequences",
    "temperature",
    "top_p",
    "top_k",
    "thinking",
    "output_config",
];

/// Validate a request body and keep only what may go upstream. Error messages name fields,
/// never their content.
pub(crate) fn check(body: Value, ent: &Entitlements, limits: &Limits) -> Result<Checked, String> {
    let Value::Object(mut obj) = body else {
        return Err("the body must be a JSON object".into());
    };
    obj.retain(|k, _| ALLOWED_FIELDS.contains(&k.as_str()));
    let tier = obj
        .get("model")
        .and_then(Value::as_str)
        .and_then(LlmTier::parse)
        .ok_or_else(|| format!("model must be \"{FAST}\" or \"{SMART}\""))?;
    let stream = match obj.get("stream") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("stream must be a boolean".into()),
    };
    let messages = obj
        .get("messages")
        .and_then(Value::as_array)
        .filter(|m| !m.is_empty())
        .ok_or("messages must be a non-empty array")?;
    if messages.len() > limits.max_messages {
        return Err(format!("at most {} messages", limits.max_messages));
    }
    for m in messages {
        let role_ok = matches!(m["role"].as_str(), Some("user" | "assistant"));
        let content_ok = m["content"].is_string() || m["content"].is_array();
        if !role_ok || !content_ok {
            return Err("each message needs a role (user or assistant) and content".into());
        }
    }
    match obj.get("system") {
        None | Some(Value::String(_) | Value::Array(_)) => {}
        Some(_) => return Err("system must be a string or an array of blocks".into()),
    }
    if let Some(tools) = obj.get("tools") {
        let tools = tools.as_array().ok_or("tools must be an array")?;
        for t in tools {
            let custom = t["type"].is_null() || t["type"] == "custom";
            if !custom || !t["name"].is_string() || !t["input_schema"].is_object() {
                return Err("only custom tools (name and input_schema) are supported".into());
            }
        }
    }
    let requested = obj
        .get("max_tokens")
        .and_then(Value::as_u64)
        .ok_or("max_tokens must be a non-negative integer")?;
    if requested == 0 && stream {
        return Err("max_tokens: 0 is only for non-streaming cache warming".into());
    }
    let cap = match tier {
        LlmTier::Fast => ent.max_output_fast,
        LlmTier::Smart => ent.max_output_smart,
    };
    obj.insert("max_tokens".into(), json!(requested.min(u64::from(cap))));
    ensure_cache_breakpoint(&mut obj);
    Ok(Checked {
        tier,
        stream,
        body: obj,
    })
}

/// Requests that mark nothing cacheable get a breakpoint on the system prompt, so the stable
/// prefix (rules, voice) is cached across a user's keystrokes. Prompts under the provider's
/// minimum cacheable length are simply not cached, at no extra cost.
fn ensure_cache_breakpoint(obj: &mut Map<String, Value>) {
    fn has_cc(v: &Value) -> bool {
        match v {
            Value::Object(m) => m.contains_key("cache_control") || m.values().any(has_cc),
            Value::Array(a) => a.iter().any(has_cc),
            _ => false,
        }
    }
    if obj.values().any(has_cc) {
        return;
    }
    let cc = json!({"type": "ephemeral"});
    match obj.get_mut("system") {
        Some(Value::String(s)) if !s.trim().is_empty() => {
            let text = std::mem::take(s);
            obj.insert(
                "system".into(),
                json!([{"type": "text", "text": text, "cache_control": cc}]),
            );
        }
        Some(Value::Array(blocks)) => {
            if let Some(last) = blocks.iter_mut().rev().find(|b| b["type"] == "text") {
                last["cache_control"] = cc;
            }
        }
        _ => {}
    }
}

/// Usage of one request, metered when the request ends (or the client goes away).
struct Meter {
    state: Arc<LlmState>,
    user: Uuid,
    tier: LlmTier,
    model: String,
    route: &'static str,
    started: Instant,
    first_byte: Option<Duration>,
    input: u32,
    cache_read: u32,
    cache_write: u32,
    output: Option<u32>,
    output_chars: usize,
    status: &'static str,
    done: bool,
}

impl Meter {
    fn new(
        state: &Arc<LlmState>,
        user: Uuid,
        tier: LlmTier,
        model: &str,
        route: &'static str,
    ) -> Self {
        Self {
            state: state.clone(),
            user,
            tier,
            model: model.to_owned(),
            route,
            started: Instant::now(),
            first_byte: None,
            input: 0,
            cache_read: 0,
            cache_write: 0,
            output: None,
            output_chars: 0,
            status: "ok",
            done: false,
        }
    }

    /// Read usage from one Anthropic stream event (or a whole non-streaming message).
    fn anthropic_usage(&mut self, usage: &Value) {
        let n = |k: &str| usage[k].as_u64().map(|v| v.min(u64::from(u32::MAX)) as u32);
        if let Some(v) = n("input_tokens") {
            self.input = v;
        }
        if let Some(v) = n("cache_read_input_tokens") {
            self.cache_read = v;
        }
        if let Some(v) = n("cache_creation_input_tokens") {
            self.cache_write = v;
        }
        if let Some(v) = n("output_tokens") {
            self.output = Some(v);
        }
    }

    fn anthropic_event(&mut self, ev: &sse::Event) {
        let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
            return;
        };
        match v["type"].as_str() {
            Some("message_start") => {
                // Its `output_tokens` is a placeholder; the real count comes with message_delta.
                let output = self.output;
                self.anthropic_usage(&v["message"]["usage"]);
                self.output = output;
            }
            Some("message_delta") => self.anthropic_usage(&v["usage"]),
            Some("content_block_delta") => {
                let d = &v["delta"];
                for k in ["text", "partial_json", "thinking"] {
                    self.output_chars += d[k].as_str().map_or(0, str::len);
                }
            }
            _ => {}
        }
    }

    /// Record the row (once), in the background.
    fn finish(&mut self) {
        if self.done {
            return;
        }
        self.done = true;
        // Output tokens are only reported at the end; a stream cut short is estimated from
        // what was relayed, erring high (about 3 characters per token).
        let output = self
            .output
            .unwrap_or_else(|| self.output_chars.div_ceil(3) as u32);
        let (i, o) = price_per_mtok(&self.model);
        let cost = (f64::from(self.input) * i
            + f64::from(self.cache_write) * i * 1.25
            + f64::from(self.cache_read) * i * 0.1
            + f64::from(output) * o)
            .ceil() as i64;
        tracing::info!(
            user = %self.user,
            tier = self.tier.as_str(),
            model = %self.model,
            route = self.route,
            status = self.status,
            input = self.input,
            cache_read = self.cache_read,
            cache_write = self.cache_write,
            output,
            cost_micros = cost,
            first_byte_ms = self.first_byte.map(|d| d.as_millis() as u64),
            total_ms = self.started.elapsed().as_millis() as u64,
            "llm request"
        );
        if self.input == 0 && output == 0 && self.cache_read == 0 && self.cache_write == 0 {
            return;
        }
        self.state.add_cost(self.user, cost);
        let row = UsageRow {
            user: self.user,
            model: self.model.clone(),
            route: format!("{}:{}", self.tier.as_str(), self.route),
            input: self.input,
            output,
            cache_read: self.cache_read,
            cache_write: self.cache_write,
            cost,
        };
        let db = self.state.app.db.clone();
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        rt.spawn(async move {
            if let Err(e) = record(&db, &row).await {
                tracing::error!(%e, "llm: could not meter a request");
            }
        });
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        if !self.done {
            self.status = "client_gone";
            self.finish();
        }
    }
}

struct UsageRow {
    user: Uuid,
    model: String,
    route: String,
    input: u32,
    output: u32,
    cache_read: u32,
    cache_write: u32,
    cost: i64,
}

async fn record(db: &crate::db::Db, r: &UsageRow) -> Result<(), DbError> {
    let int = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
    let (i, o, cr, cw) = (
        int(r.input),
        int(r.output),
        int(r.cache_read),
        int(r.cache_write),
    );
    db.scoped(Some(r.user), async |tx| {
        tx.execute(
            "insert into public.usage_events
               (user_id, kind, model, input_tokens, output_tokens, cost_micros, route,
                cache_read_tokens, cache_write_tokens)
             values ($1, 'llm', $2, $3, $4, $5, $6, $7, $8)",
            &[&r.user, &r.model, &i, &o, &r.cost, &r.route, &cr, &cw],
        )
        .await?;
        Ok(())
    })
    .await
}

/// Why the primary upstream did not answer.
enum Failed {
    /// Worth trying the fallback: network error, timeout, overload, 5xx, our own rate limit.
    Retryable(Response),
    /// The request itself was refused (4xx): the fallback would refuse it too.
    Final(Response),
}

async fn messages(
    State(s): State<Arc<LlmState>>,
    h: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let user = match authed(&s, &h).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let body = match body {
        Ok(b) => b,
        Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                &format!(
                    "the request is larger than {} bytes",
                    s.cfg.limits.max_body_bytes
                ),
                None,
            );
        }
        Err(_) => return bad_request("could not read the request body"),
    };
    let Ok(value) = serde_json::from_slice::<Value>(&body) else {
        return bad_request("the body is not valid JSON");
    };
    drop(body);
    if !s.configured() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "api_error",
            "no model provider is configured",
            Some("llm_not_configured"),
        );
    }
    let (ent, month_cost) = match s.usage(user.id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(%e, "llm: reading usage");
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "api_error",
                "internal error",
                None,
            );
        }
    };
    if month_cost >= ent.quota_micros {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "monthly AI quota used up",
            Some("quota_exceeded"),
        );
    }
    let checked = match check(value, &ent, &s.cfg.limits) {
        Ok(c) => c,
        Err(m) => return bad_request(&m),
    };
    let Some(slot) = s.acquire(user.id, ent.max_concurrent) else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "too many requests at once",
            Some("too_many_concurrent"),
        );
    };
    let version = h
        .get("anthropic-version")
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.len() == 10 && v.bytes().all(|b| b.is_ascii_digit() || b == b'-'))
        .unwrap_or(DEFAULT_ANTHROPIC_VERSION)
        .to_owned();
    let deadline = Instant::now() + s.cfg.limits.total_timeout;

    if let Some(a) = s.cfg.anthropic.clone() {
        s.touch_upstream();
        let failure =
            match anthropic(&s, &a, user.id, &ent, &checked, &version, slot, deadline).await {
                Ok(r) | Err(Failed::Final(r)) => return r,
                Err(Failed::Retryable(r)) => r,
            };
        // The slot went with the failed attempt; the fallback takes a new one.
        let (Some(o), Some(slot)) = (s.cfg.openai.clone(), s.acquire(user.id, ent.max_concurrent))
        else {
            return failure;
        };
        tracing::warn!(user = %user.id, tier = checked.tier.as_str(), "llm: falling back to OpenAI");
        return openai_route(&s, &o, user.id, &ent, &checked, slot, deadline)
            .await
            .unwrap_or_else(|r| r);
    }
    let o = s.cfg.openai.clone().expect("configured() checked");
    openai_route(&s, &o, user.id, &ent, &checked, slot, deadline)
        .await
        .unwrap_or_else(|r| r)
}

fn upstream_error() -> Response {
    error(
        StatusCode::BAD_GATEWAY,
        "api_error",
        "the AI provider failed; try again",
        None,
    )
}

fn timeout_error() -> Response {
    error(
        StatusCode::GATEWAY_TIMEOUT,
        "timeout_error",
        "the AI provider did not answer in time",
        None,
    )
}

/// Map a provider's error status for our client. Our own key being refused is our problem,
/// never reported as the user's (`401` would sign them out); 429/529/5xx are retryable.
async fn provider_failure(resp: reqwest::Response, route: &str) -> Failed {
    let status = resp.status();
    tracing::warn!(route, status = status.as_u16(), "llm: provider refused");
    match status.as_u16() {
        401 | 403 => Failed::Retryable(upstream_error()),
        429 => Failed::Retryable(error(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "the AI provider is busy; try again",
            None,
        )),
        529 => Failed::Retryable(error(
            StatusCode::from_u16(529).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
            "overloaded_error",
            "the AI provider is overloaded; try again",
            None,
        )),
        s if s >= 500 => Failed::Retryable(upstream_error()),
        s => {
            // The request was refused: relay the provider's error (it describes the request,
            // which is the user's own) without logging it.
            let body = read_capped(resp, MAX_ERROR_BODY).await.unwrap_or_default();
            let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let message = v
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("the AI provider refused the request");
            let kind = v
                .pointer("/error/type")
                .and_then(Value::as_str)
                .unwrap_or("invalid_request_error");
            Failed::Final(error(
                StatusCode::from_u16(s).unwrap_or(StatusCode::BAD_REQUEST),
                kind,
                message,
                None,
            ))
        }
    }
}

async fn read_capped(resp: reqwest::Response, max: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut body = resp.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.ok()?;
        if out.len() + chunk.len() > max {
            return None;
        }
        out.extend_from_slice(&chunk);
    }
    Some(out)
}

/// Send with the time-to-first-byte limit; network errors and timeouts are retryable.
async fn send(
    req: reqwest::RequestBuilder,
    limits: &Limits,
    route: &str,
) -> Result<reqwest::Response, Failed> {
    match tokio::time::timeout(limits.headers_timeout, req.send()).await {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(e)) => {
            tracing::warn!(route, error = %e.without_url(), "llm: provider unreachable");
            Err(Failed::Retryable(upstream_error()))
        }
        Err(_) => {
            tracing::warn!(route, "llm: provider timed out before answering");
            Err(Failed::Retryable(timeout_error()))
        }
    }
}

fn sse_response(
    stream: impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
) -> Response {
    let mut r = Response::new(Body::from_stream(stream));
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert("x-accel-buffering", HeaderValue::from_static("no"));
    r
}

/// A final SSE `error` event, so a stream cut short by a timeout is not mistaken for a
/// complete one.
fn sse_error(kind: &str, message: &str) -> Bytes {
    Bytes::from(sse::encode(
        "error",
        &json!({"type": "error", "error": {"type": kind, "message": message}}),
    ))
}

/// What a relayed stream needs to keep until it ends.
struct Relay<S> {
    upstream: S,
    meter: Meter,
    _slot: Slot,
    deadline: Instant,
    idle: Duration,
    done: bool,
}

impl<S> Relay<S>
where
    S: futures::Stream<Item = reqwest::Result<Bytes>> + Unpin,
{
    /// The next upstream chunk, or why there is none (`Err` carries a final SSE error event).
    async fn next_chunk(&mut self) -> Option<Result<Bytes, Bytes>> {
        let wait = self
            .idle
            .min(self.deadline.saturating_duration_since(Instant::now()));
        match tokio::time::timeout(wait, self.upstream.next()).await {
            Ok(Some(Ok(b))) => Some(Ok(b)),
            Ok(Some(Err(e))) => {
                tracing::warn!(error = %e.without_url(), "llm: provider stream failed");
                self.meter.status = "upstream_error";
                Some(Err(sse_error(
                    "api_error",
                    "the AI provider failed mid-answer",
                )))
            }
            Ok(None) => None,
            Err(_) => {
                self.meter.status = "timeout";
                Some(Err(sse_error(
                    "timeout_error",
                    "the AI provider stopped answering",
                )))
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn anthropic(
    s: &Arc<LlmState>,
    a: &Upstream,
    user: Uuid,
    ent: &Entitlements,
    checked: &Checked,
    version: &str,
    slot: Slot,
    deadline: Instant,
) -> Result<Response, Failed> {
    let model = a.model(checked.tier, ent).to_owned();
    let mut body = checked.body.clone();
    body.insert("model".into(), json!(model));
    body.insert("metadata".into(), json!({"user_id": user.to_string()}));
    let mut meter = Meter::new(s, user, checked.tier, &model, "anthropic");
    let req = s
        .http
        .post(format!("{}/v1/messages", a.base))
        .header("x-api-key", &a.key)
        .header("anthropic-version", version)
        .json(&body);
    let resp = send(req, &s.cfg.limits, "anthropic").await?;
    meter.first_byte = Some(meter.started.elapsed());
    if !resp.status().is_success() {
        meter.done = true;
        return Err(provider_failure(resp, "anthropic").await);
    }
    if !checked.stream {
        let Some(bytes) = read_capped(resp, MAX_JSON_BODY).await else {
            meter.status = "upstream_error";
            return Ok(upstream_error());
        };
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        meter.anthropic_usage(&v["usage"]);
        meter.finish();
        drop(slot);
        return Ok((StatusCode::OK, Json(v)).into_response());
    }
    let relay = Relay {
        upstream: resp.bytes_stream(),
        meter,
        _slot: slot,
        deadline,
        idle: s.cfg.limits.idle_timeout,
        done: false,
    };
    let parser = sse::Parser::default();
    let stream = futures::stream::unfold((relay, parser), |(mut r, mut p)| async move {
        if r.done {
            return None;
        }
        match r.next_chunk().await {
            Some(Ok(bytes)) => {
                for ev in p.feed(&bytes) {
                    r.meter.anthropic_event(&ev);
                }
                Some((Ok(bytes), (r, p)))
            }
            Some(Err(last)) => {
                r.done = true;
                r.meter.finish();
                Some((Ok(last), (r, p)))
            }
            None => {
                for ev in p.finish() {
                    r.meter.anthropic_event(&ev);
                }
                r.meter.finish();
                None
            }
        }
    });
    Ok(sse_response(stream))
}

/// The OpenAI fallback: translate the request, and the answer back to Anthropic's format.
async fn openai_route(
    s: &Arc<LlmState>,
    o: &Upstream,
    user: Uuid,
    ent: &Entitlements,
    checked: &Checked,
    slot: Slot,
    deadline: Instant,
) -> Result<Response, Response> {
    let model = o.model(checked.tier, ent).to_owned();
    if checked.body.get("max_tokens").and_then(Value::as_u64) == Some(0) {
        // Cache warming has no OpenAI equivalent (its caching is automatic): nothing to do.
        return Ok(Json(json!({
            "id": "msg_warm", "type": "message", "role": "assistant", "model": checked.tier.as_str(),
            "content": [], "stop_reason": "max_tokens", "stop_sequence": null,
            "usage": {"input_tokens": 0, "output_tokens": 0},
        }))
        .into_response());
    }
    // Requests of one user and tier share a prefix (rules, voice): route them to one cache.
    let cache_key = format!("{}:{}", user.simple(), checked.tier.as_str());
    let body = openai::to_openai_request(
        &Value::Object(checked.body.clone()),
        &model,
        checked.stream,
        Some(&cache_key),
    )
    .map_err(|m| bad_request(&m))?;
    let mut meter = Meter::new(s, user, checked.tier, &model, "openai");
    let req = s
        .http
        .post(format!("{}/v1/chat/completions", o.base))
        .bearer_auth(&o.key)
        .json(&body);
    let resp = match send(req, &s.cfg.limits, "openai").await {
        Ok(r) => r,
        Err(Failed::Retryable(r) | Failed::Final(r)) => {
            meter.done = true;
            return Err(r);
        }
    };
    meter.first_byte = Some(meter.started.elapsed());
    if !resp.status().is_success() {
        meter.done = true;
        return Err(match provider_failure(resp, "openai").await {
            Failed::Retryable(r) | Failed::Final(r) => r,
        });
    }
    let alias = checked.tier.as_str();
    if !checked.stream {
        let Some(bytes) = read_capped(resp, MAX_JSON_BODY).await else {
            meter.status = "upstream_error";
            return Err(upstream_error());
        };
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let out = openai::from_openai_response(&v, alias).map_err(|_| upstream_error())?;
        let n = |p: &str| v.pointer(p).and_then(Value::as_u64).unwrap_or(0) as u32;
        let cached = n("/usage/prompt_tokens_details/cached_tokens");
        meter.input = n("/usage/prompt_tokens").saturating_sub(cached);
        meter.cache_read = cached;
        meter.output = Some(n("/usage/completion_tokens"));
        meter.finish();
        drop(slot);
        return Ok((StatusCode::OK, Json(out)).into_response());
    }
    let relay = Relay {
        upstream: resp.bytes_stream(),
        meter,
        _slot: slot,
        deadline,
        idle: s.cfg.limits.idle_timeout,
        done: false,
    };
    let state = (
        relay,
        sse::Parser::default(),
        openai::StreamTranslator::new(alias),
    );
    let stream = futures::stream::unfold(state, |(mut r, mut p, mut t)| async move {
        if r.done {
            return None;
        }
        // Keep reading until a chunk yields output (or the stream ends).
        loop {
            let (events, end) = match r.next_chunk().await {
                Some(Ok(bytes)) => (p.feed(&bytes), None),
                Some(Err(last)) => (Vec::new(), Some(Some(last))),
                None => (p.finish(), Some(None)),
            };
            let mut out = Vec::new();
            for ev in events {
                if ev.data == "[DONE]" {
                    continue;
                }
                match t.on_data(&ev.data) {
                    Ok(evs) => {
                        for (name, data) in evs {
                            out.extend(sse::encode(name, &data));
                        }
                    }
                    Err(_) => {
                        r.meter.status = "upstream_error";
                        out.extend_from_slice(&sse_error(
                            "api_error",
                            "the AI provider failed mid-answer",
                        ));
                        r.done = true;
                        break;
                    }
                }
            }
            let usage = t.usage();
            r.meter.input = usage.0; // already excludes cached tokens
            r.meter.cache_read = usage.2;
            if usage.1 > 0 {
                r.meter.output = Some(usage.1);
            }
            r.meter.output_chars = t.output_chars();
            if let Some(last) = end {
                match last {
                    Some(err) => out.extend_from_slice(&err),
                    None => {
                        for (name, data) in t.finish() {
                            out.extend(sse::encode(name, &data));
                        }
                    }
                }
                r.done = true;
            }
            if r.done {
                r.meter.finish();
                return Some((Ok(Bytes::from(out)), (r, p, t)));
            }
            if !out.is_empty() {
                return Some((Ok(Bytes::from(out)), (r, p, t)));
            }
        }
    });
    Ok(sse_response(stream))
}
