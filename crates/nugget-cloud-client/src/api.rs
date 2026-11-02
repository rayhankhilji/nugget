//! Typed client for the cloud service's `/v1/*` endpoints. Every call carries the session's
//! access token; errors come back as `{error: "<code>", message}`.

use std::sync::Arc;

use reqwest::Method;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::session::Session;
use crate::{CloudError, Result};

mod intel;
pub use intel::{
    Brief, Card, CardAction, CardEvidence, CardOutcome, Cards, DismissReason, FacetCount, Facets,
    SearchDates, SearchRequest, SearchResponse, SearchResult, Timeline, TimelineBucket,
};

/// Sent on every `/v1` call so the cloud can enforce the plan's device limit.
pub const DEVICE_HEADER: &str = "X-Heed-Device";

/// This month's metering. Older servers sent `quota_micros`; newer ones `smart_asks_used`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Usage {
    pub month_cost_micros: i64,
    pub quota_micros: Option<i64>,
    pub smart_asks_used: u32,
}

/// What a plan includes. `None` limits are unlimited.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entitlements {
    pub plan: String,
    pub name: String,
    pub max_connections: Option<u32>,
    pub sync_interval_minutes: u32,
    pub history_days: Option<u32>,
    pub smart_asks_per_month: u32,
    /// `fast` or `smart`.
    pub default_model: String,
    pub devices: u32,
    pub ghost_text: bool,
    pub needs_you_inbox: bool,
    pub daily_brief: bool,
    pub actions_with_approval: bool,
    pub agent_tasks: bool,
    pub computer_use: bool,
    pub priority_support: bool,
}

/// The Stripe subscription as the cloud mirrors it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Subscription {
    pub plan: String,
    /// Stripe's status: `trialing`, `active`, `past_due`, `canceled`, ...
    pub status: String,
    /// `month` or `year`.
    pub interval: String,
    pub trial_end: Option<String>,
    pub current_period_end: Option<String>,
    pub cancel_at_period_end: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    pub user_id: String,
    #[serde(default)]
    pub email: Option<String>,
    /// `heed`, `heed_max`, or `None` without a subscription.
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub embeddings: bool,
    #[serde(default)]
    pub subscription: Option<Subscription>,
    #[serde(default)]
    pub entitlements: Option<Entitlements>,
    #[serde(default)]
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanOffer {
    pub id: String,
    pub name: String,
    pub monthly_gbp: u32,
    pub annual_gbp: u32,
    #[serde(default)]
    pub trial_days: u32,
    #[serde(default)]
    pub entitlements: Entitlements,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plans {
    pub plans: Vec<PlanOffer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Interval {
    Month,
    Year,
}

impl Interval {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "month" => Ok(Self::Month),
            "year" => Ok(Self::Year),
            _ => Err(CloudError::Invalid(
                "choose monthly or annual billing".into(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelTier {
    Fast,
    Smart,
}

/// A Stripe page (Checkout or the customer portal) to open in the browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BillingLink {
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,
    pub toolkit: String,
    pub status: String,
    pub last_synced_at: Option<String>,
    pub last_error: Option<String>,
    /// A sync is running in the cloud now.
    #[serde(default)]
    pub syncing: bool,
    /// The first import (backfill) has finished.
    #[serde(default)]
    pub backfill_done: bool,
    #[serde(default)]
    pub items_synced: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableApp {
    pub slug: String,
    pub name: String,
    pub logo: Option<String>,
    pub connectable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connections {
    pub connections: Vec<Connection>,
    pub available: Vec<AvailableApp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectStart {
    pub redirect_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncResult {
    pub toolkit: String,
    pub added: u64,
    pub updated: u64,
    /// Items removed because they were deleted upstream (servers before CL4 omit it).
    #[serde(default)]
    pub deleted: u64,
    pub error: Option<String>,
    /// Another sync of this connection was already running.
    #[serde(default)]
    pub in_progress: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    pub synced: Vec<SyncResult>,
    /// Work continues in the background (202); poll the connections for progress.
    #[serde(default)]
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskRequest {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    /// Ask for a tier; the plan's default when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelTier>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    pub n: u32,
    pub item_id: String,
    pub title: Option<String>,
    pub source: String,
    pub url: Option<String>,
    pub occurred_at: Option<String>,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// An answer with `[n]` markers that refer to `citations`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub conversation_id: String,
    pub answer: String,
    pub citations: Vec<Citation>,
    pub usage: AskUsage,
    /// `fast` or `smart`: the tier that answered.
    #[serde(default)]
    pub model_tier: Option<String>,
    /// A smart request answered by the fast tier (allowance or fair-use budget used up).
    #[serde(default)]
    pub downgraded: bool,
}

/// An app in the catalogue (`GET /v1/apps`): what it syncs, which actions it can take and
/// whether the user has connected it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfo {
    pub slug: String,
    pub name: String,
    /// A `data:` URI (the desktop CSP allows no remote images).
    pub logo: Option<String>,
    /// The remote logo (for the website); servers before the data: change omit it.
    #[serde(default)]
    pub logo_url: Option<String>,
    /// `mail`, `calendar`, `chat`, `docs` or `files`.
    pub category: String,
    pub syncs: bool,
    /// Action kinds (`send_mail`, ...) as the server names them.
    #[serde(default)]
    pub actions: Vec<String>,
    /// `poll` or `trigger`.
    pub sync_mode: String,
    pub connectable: bool,
    pub connected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AppList {
    apps: Vec<AppInfo>,
}

/// What an action does once approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    SendMail,
    ReplyMail,
    CreateEvent,
    UpdateEvent,
    PostMessage,
    CreatePage,
}

impl ActionKind {
    pub const ALL: [Self; 6] = [
        Self::SendMail,
        Self::ReplyMail,
        Self::CreateEvent,
        Self::UpdateEvent,
        Self::PostMessage,
        Self::CreatePage,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SendMail => "send_mail",
            Self::ReplyMail => "reply_mail",
            Self::CreateEvent => "create_event",
            Self::UpdateEvent => "update_event",
            Self::PostMessage => "post_message",
            Self::CreatePage => "create_page",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| CloudError::Invalid("not a known action".into()))
    }
}

/// Where an action is in its life. Statuses this client doesn't know read as `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Pending,
    Executing,
    Succeeded,
    Failed,
    Cancelled,
    Expired,
    Denied,
    #[serde(other)]
    Unknown,
}

impl ActionStatus {
    /// The statuses a list may be filtered by.
    pub const KNOWN: [Self; 7] = [
        Self::Pending,
        Self::Executing,
        Self::Succeeded,
        Self::Failed,
        Self::Cancelled,
        Self::Expired,
        Self::Denied,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Executing => "executing",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
            Self::Denied => "denied",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Self::KNOWN
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| CloudError::Invalid("not a known action status".into()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewField {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewDiff {
    pub field: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// What the action would send, for the user to read before approving.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionPreview {
    #[serde(default)]
    pub fields: Vec<PreviewField>,
    #[serde(default)]
    pub diff: Vec<PreviewDiff>,
}

/// A proposed (and maybe approved, executed) write to a connected app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub kind: ActionKind,
    pub toolkit: String,
    pub status: ActionStatus,
    /// e.g. `send`.
    pub risk: String,
    /// The policy's verdict: `confirm` or `deny`.
    pub decision: String,
    pub reason: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub preview: ActionPreview,
    /// Hex SHA-256 of what will be executed; an approval must echo it.
    pub digest: String,
    pub idempotency_key: String,
    pub created_at: String,
    pub expires_at: String,
    pub decided_at: Option<String>,
    pub executed_at: Option<String>,
    pub result: Option<Value>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ActionList {
    actions: Vec<Action>,
}

/// `send_mail` input.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendMail {
    pub to: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cc: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
}

/// `reply_mail` input.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyMail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Vec<String>>,
    pub body: String,
}

/// `create_event` input; times are RFC 3339.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateEvent {
    pub title: String,
    pub start: String,
    pub end: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attendees: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `update_event` input: only the fields given change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateEvent {
    pub event_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attendees: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `post_message` input.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostMessage {
    pub channel: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_ts: Option<String>,
}

/// `create_page` input.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePage {
    pub parent_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
}

/// A typed action input; [`ProposeAction::typed`] turns it into a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionInput {
    SendMail(SendMail),
    ReplyMail(ReplyMail),
    CreateEvent(CreateEvent),
    UpdateEvent(UpdateEvent),
    PostMessage(PostMessage),
    CreatePage(CreatePage),
}

impl ActionInput {
    pub fn kind(&self) -> ActionKind {
        match self {
            Self::SendMail(_) => ActionKind::SendMail,
            Self::ReplyMail(_) => ActionKind::ReplyMail,
            Self::CreateEvent(_) => ActionKind::CreateEvent,
            Self::UpdateEvent(_) => ActionKind::UpdateEvent,
            Self::PostMessage(_) => ActionKind::PostMessage,
            Self::CreatePage(_) => ActionKind::CreatePage,
        }
    }

    pub fn to_value(&self) -> Value {
        let v = match self {
            Self::SendMail(i) => serde_json::to_value(i),
            Self::ReplyMail(i) => serde_json::to_value(i),
            Self::CreateEvent(i) => serde_json::to_value(i),
            Self::UpdateEvent(i) => serde_json::to_value(i),
            Self::PostMessage(i) => serde_json::to_value(i),
            Self::CreatePage(i) => serde_json::to_value(i),
        };
        // Plain structs of strings always serialize.
        v.unwrap_or(Value::Null)
    }
}

/// `POST /v1/actions` body. `input` is the kind's object (see [`ActionInput`] for the shapes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposeAction {
    pub kind: ActionKind,
    pub toolkit: String,
    pub input: Value,
    pub idempotency_key: String,
}

impl ProposeAction {
    pub fn typed(
        toolkit: impl Into<String>,
        input: &ActionInput,
        idempotency_key: impl Into<String>,
    ) -> Self {
        Self {
            kind: input.kind(),
            toolkit: toolkit.into(),
            input: input.to_value(),
            idempotency_key: idempotency_key.into(),
        }
    }
}

#[derive(Clone)]
pub struct Api {
    session: Arc<Session>,
}

impl Api {
    pub fn new(session: Arc<Session>) -> Self {
        Self { session }
    }

    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<reqwest::Response> {
        let token = self.session.access_token().await?;
        let url = format!("{}/v1/{path}", self.session.config().api_base);
        let mut req = self
            .session
            .http()
            .request(method, url)
            .bearer_auth(token)
            .header(DEVICE_HEADER, self.session.device_id().await);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp = req.send().await.map_err(CloudError::network)?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        let text = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_owned);
        let code = text("error").unwrap_or_else(|| format!("http_{}", status.as_u16()));
        let message = text("message").unwrap_or_else(|| status.to_string());
        if status == reqwest::StatusCode::UNAUTHORIZED {
            // Revoked or expired early: the next call refreshes (or reports signed out).
            self.session.invalidate_access().await;
            return Err(CloudError::NotSignedIn);
        }
        match (status.as_u16(), code.as_str()) {
            (429, "quota_exceeded") => return Err(CloudError::QuotaExceeded(message)),
            (402, _) => return Err(CloudError::SubscriptionRequired(message)),
            (403, "device_limit_reached") => return Err(CloudError::DeviceLimitReached(message)),
            (403, "connection_limit_reached") => {
                return Err(CloudError::ConnectionLimitReached(message));
            }
            (409, "no_customer") => return Err(CloudError::NoCustomer(message)),
            _ => {}
        }
        Err(CloudError::Api {
            status: status.as_u16(),
            code,
            message,
        })
    }

    async fn json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<T> {
        self.send(method, path, body)
            .await?
            .json()
            .await
            .map_err(|e| CloudError::Protocol(format!("{path}: {}", e.without_url())))
    }

    /// `GET /v1/me`.
    pub async fn me(&self) -> Result<Me> {
        self.json(Method::GET, "me", None).await
    }

    /// `GET /v1/connections`: the user's connected accounts and the apps they may connect.
    pub async fn connections(&self) -> Result<Connections> {
        self.json(Method::GET, "connections", None).await
    }

    /// `POST /v1/connections`: the Composio consent page to open in the browser.
    pub async fn connect(&self, toolkit: &str) -> Result<ConnectStart> {
        self.json(
            Method::POST,
            "connections",
            Some(json!({ "toolkit": toolkit })),
        )
        .await
    }

    /// `DELETE /v1/connections/{id}`.
    pub async fn disconnect(&self, id: &str) -> Result<()> {
        let id = path_segment(id)?;
        self.send(Method::DELETE, &format!("connections/{id}"), None)
            .await
            .map(drop)
    }

    /// `POST /v1/sync`: sync every connection now.
    pub async fn sync(&self) -> Result<SyncReport> {
        self.json(Method::POST, "sync", Some(json!({}))).await
    }

    /// `POST /v1/ask`.
    pub async fn ask(&self, request: &AskRequest) -> Result<Answer> {
        let body = serde_json::to_value(request).map_err(|e| CloudError::Invalid(e.to_string()))?;
        self.json(Method::POST, "ask", Some(body)).await
    }

    /// `GET /v1/apps`: the app catalogue with what each can sync and do.
    pub async fn apps(&self) -> Result<Vec<AppInfo>> {
        Ok(self.json::<AppList>(Method::GET, "apps", None).await?.apps)
    }

    /// `POST /v1/actions`: propose an action; it waits for approval (or is denied by policy).
    pub async fn propose_action(&self, request: &ProposeAction) -> Result<Action> {
        let body = serde_json::to_value(request).map_err(|e| CloudError::Invalid(e.to_string()))?;
        self.json(Method::POST, "actions", Some(body)).await
    }

    /// `GET /v1/actions[?status=...]`: the user's actions, optionally of one status.
    pub async fn actions(&self, status: Option<&str>) -> Result<Vec<Action>> {
        let path = match status {
            Some(s) => format!("actions?status={}", ActionStatus::parse(s)?.as_str()),
            None => "actions".to_owned(),
        };
        Ok(self
            .json::<ActionList>(Method::GET, &path, None)
            .await?
            .actions)
    }

    /// `GET /v1/actions/{id}`.
    pub async fn action(&self, id: &str) -> Result<Action> {
        let id = path_segment(id)?;
        self.json(Method::GET, &format!("actions/{id}"), None).await
    }

    /// `GET /v1/actions/{id}/preview`: a dry run of what would be sent; no side effects.
    pub async fn action_preview(&self, id: &str) -> Result<Action> {
        let id = path_segment(id)?;
        self.json(Method::GET, &format!("actions/{id}/preview"), None)
            .await
    }

    /// `POST /v1/actions/{id}/approve`: `digest` must be the one the user was shown.
    pub async fn approve_action(&self, id: &str, digest: &str) -> Result<Action> {
        let id = path_segment(id)?;
        self.json(
            Method::POST,
            &format!("actions/{id}/approve"),
            Some(json!({ "digest": digest })),
        )
        .await
    }

    /// `POST /v1/actions/{id}/cancel`.
    pub async fn cancel_action(&self, id: &str) -> Result<Action> {
        let id = path_segment(id)?;
        self.json(
            Method::POST,
            &format!("actions/{id}/cancel"),
            Some(json!({})),
        )
        .await
    }

    /// `GET /v1/billing/plans`: the plans on offer.
    pub async fn plans(&self) -> Result<Plans> {
        self.json(Method::GET, "billing/plans", None).await
    }

    /// `POST /v1/billing/checkout`: a Stripe Checkout page that starts a trial of `plan`.
    pub async fn checkout(&self, plan: &str, interval: Interval) -> Result<BillingLink> {
        let ok = !plan.is_empty()
            && plan.len() <= 32
            && plan
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !ok {
            return Err(CloudError::Invalid("not a known plan".into()));
        }
        let body = json!({ "plan": plan, "interval": interval });
        self.json(Method::POST, "billing/checkout", Some(body))
            .await
    }

    /// `POST /v1/billing/portal`: the Stripe customer portal (change plan, card, cancel).
    pub async fn portal(&self) -> Result<BillingLink> {
        self.json(Method::POST, "billing/portal", Some(json!({})))
            .await
    }

    /// `GET /v1/export`: all of the user's data as JSON.
    pub async fn export(&self) -> Result<Value> {
        self.json(Method::GET, "export", None).await
    }

    /// `DELETE /v1/me`: delete the account; the local session is forgotten too.
    pub async fn delete_account(&self) -> Result<()> {
        self.send(Method::DELETE, "me", None).await?;
        self.session.forget().await;
        Ok(())
    }
}

/// Ids go into a path: only characters that need no escaping.
fn path_segment(id: &str) -> Result<&str> {
    let ok = !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        && id != "."
        && id != "..";
    if ok {
        Ok(id)
    } else {
        Err(CloudError::Invalid("not a valid id".into()))
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::auth::tests::token_body;
    use crate::session::tests::{FakeClock, session};
    use crate::session::{MemoryStore, StoredSession};
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub(super) async fn signed_in(server: &MockServer) -> Api {
        Mock::given(method("POST"))
            .and(path("/auth/v1/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_body("AT", "rt2", 10_000)))
            .mount(server)
            .await;
        let store = Arc::new(MemoryStore::with(StoredSession {
            refresh_token: "rt".into(),
            email: None,
        }));
        let clock = Arc::new(FakeClock::default());
        clock.set(1_000);
        Api::new(Arc::new(session(&server.uri(), store, clock)))
    }

    fn err(status: u16, code: &str, message: &str) -> ResponseTemplate {
        ResponseTemplate::new(status).set_body_json(json!({ "error": code, "message": message }))
    }

    #[tokio::test]
    async fn calls_carry_the_bearer_token_and_parse() {
        let server = MockServer::start().await;
        let api = signed_in(&server).await;
        Mock::given(method("POST"))
            .and(path("/v1/ask"))
            .and(header("authorization", "Bearer AT"))
            .and(body_json(json!({ "question": "When is my flight?" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "conversation_id": "c1",
                "answer": "Tuesday at 9 [1].",
                "citations": [{ "n": 1, "item_id": "i1", "title": "Booking", "source": "gmail",
                                "url": null, "occurred_at": "2026-10-30T09:00:00Z", "snippet": "Flight BA1" }],
                "usage": { "input_tokens": 10, "output_tokens": 5 }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/connections"))
            .and(header("authorization", "Bearer AT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "connections": [{ "id": "k1", "toolkit": "gmail", "status": "ACTIVE",
                                  "last_synced_at": null, "last_error": null }],
                "available": [{ "slug": "gmail", "name": "Gmail", "logo": null, "connectable": true },
                              { "slug": "slack", "name": "Slack", "logo": null, "connectable": false }]
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/connections"))
            .and(body_json(json!({ "toolkit": "googlecalendar" })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "redirect_url": "https://connect.example/x" })),
            )
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/v1/connections/k1"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/sync"))
            .and(body_json(json!({})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "synced": [{ "toolkit": "gmail", "added": 3, "updated": 1, "error": null }]
            })))
            .mount(&server)
            .await;

        let a = api
            .ask(&AskRequest {
                question: "When is my flight?".into(),
                conversation_id: None,
                model: None,
            })
            .await
            .unwrap();
        assert_eq!(a.citations[0].n, 1);
        assert_eq!(a.usage.output_tokens, 5);
        // An older server: no tier fields.
        assert_eq!((a.model_tier.as_deref(), a.downgraded), (None, false));
        let c = api.connections().await.unwrap();
        assert_eq!(c.connections[0].toolkit, "gmail");
        assert!(!c.available[1].connectable);
        assert_eq!(
            api.connect("googlecalendar").await.unwrap().redirect_url,
            "https://connect.example/x"
        );
        api.disconnect("k1").await.unwrap();
        assert!(matches!(
            api.disconnect("../me").await,
            Err(CloudError::Invalid(_))
        ));
        assert_eq!(api.sync().await.unwrap().synced[0].added, 3);
    }

    #[tokio::test]
    async fn errors_are_mapped() {
        let server = MockServer::start().await;
        let api = signed_in(&server).await;
        Mock::given(method("GET"))
            .and(path("/v1/me"))
            .respond_with(err(401, "unauthorized", "bad token"))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/ask"))
            .respond_with(err(
                429,
                "quota_exceeded",
                "You've used this month's AI allowance.",
            ))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/sync"))
            .respond_with(err(429, "rate_limited", "slow down"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/export"))
            .respond_with(ResponseTemplate::new(500).set_body_string("oops"))
            .mount(&server)
            .await;
        assert!(matches!(api.me().await, Err(CloudError::NotSignedIn)));
        let q = api
            .ask(&AskRequest {
                question: "q".into(),
                conversation_id: Some("c1".into()),
                model: None,
            })
            .await
            .unwrap_err();
        assert!(
            matches!(&q, CloudError::QuotaExceeded(m) if m.contains("allowance")),
            "{q}"
        );
        assert_eq!(q.code(), "quota_exceeded");
        match api.sync().await {
            Err(CloudError::Api {
                status: 429, code, ..
            }) => assert_eq!(code, "rate_limited"),
            other => panic!("{other:?}"),
        }
        match api.export().await {
            Err(e @ CloudError::Api { status: 500, .. }) => assert_eq!(e.code(), "http_500"),
            other => panic!("{other:?}"),
        }
    }

    fn action_body(id: &str, status: &str) -> Value {
        json!({
            "id": id, "kind": "send_mail", "toolkit": "gmail", "status": status,
            "risk": "send", "decision": "confirm", "reason": null,
            "summary": "Email ana@example.com: Lunch",
            "preview": {
                "fields": [{ "label": "To", "value": "ana@example.com" }],
                "diff": [{ "field": "title", "before": null, "after": "Lunch" }]
            },
            "digest": "ab".repeat(32), "idempotency_key": "k-1",
            "created_at": "2026-11-02T10:00:00Z", "expires_at": "2026-11-02T10:15:00Z",
            "decided_at": null, "executed_at": null,
            "result": if status == "succeeded" { json!({ "message_id": "m1" }) } else { Value::Null },
            "error": null
        })
    }

    #[tokio::test]
    async fn apps_and_actions() {
        let server = MockServer::start().await;
        let api = signed_in(&server).await;
        let id = "6f1c0b8e-2b7d-4a52-9a43-0e0f6a2c1d11";
        Mock::given(method("GET"))
            .and(path("/v1/apps"))
            .and(header("authorization", "Bearer AT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "apps": [{
                "slug": "gmail", "name": "Gmail", "logo": null, "category": "mail",
                "syncs": true, "actions": ["send_mail", "reply_mail"], "sync_mode": "poll",
                "connectable": true, "connected": false
            }] })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/actions"))
            .and(body_json(json!({
                "kind": "send_mail", "toolkit": "gmail",
                "input": { "to": ["ana@example.com"], "subject": "Lunch", "body": "Noon?" },
                "idempotency_key": "k-1"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(action_body(id, "pending")))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/actions"))
            .and(query_param("status", "pending"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "actions": [action_body(id, "pending")] })),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/actions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "actions": [
                action_body(id, "pending"), action_body("a2", "brand_new_status")
            ] })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v1/actions/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(action_body(id, "succeeded")))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/v1/actions/{id}/preview")))
            .respond_with(ResponseTemplate::new(200).set_body_json(action_body(id, "pending")))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/v1/actions/{id}/approve")))
            .and(body_json(json!({ "digest": "ab".repeat(32) })))
            .respond_with(ResponseTemplate::new(200).set_body_json(action_body(id, "executing")))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("/v1/actions/{id}/cancel")))
            .and(body_json(json!({})))
            .respond_with(ResponseTemplate::new(200).set_body_json(action_body(id, "cancelled")))
            .expect(1)
            .mount(&server)
            .await;

        let apps = api.apps().await.unwrap();
        assert_eq!(apps[0].category, "mail");
        assert_eq!(apps[0].actions, ["send_mail", "reply_mail"]);
        assert!(!apps[0].connected);

        let input = ActionInput::SendMail(SendMail {
            to: vec!["ana@example.com".into()],
            subject: "Lunch".into(),
            body: "Noon?".into(),
            ..Default::default()
        });
        let a = api
            .propose_action(&ProposeAction::typed("gmail", &input, "k-1"))
            .await
            .unwrap();
        assert_eq!(a.kind, ActionKind::SendMail);
        assert_eq!(a.status, ActionStatus::Pending);
        assert_eq!(a.preview.fields[0].value, "ana@example.com");
        assert_eq!(a.preview.diff[0].after.as_deref(), Some("Lunch"));
        assert_eq!(a.digest.len(), 64);

        let pending = api.actions(Some("pending")).await.unwrap();
        assert_eq!(pending.len(), 1);
        let all = api.actions(None).await.unwrap();
        assert_eq!(all[1].status, ActionStatus::Unknown);
        assert!(matches!(
            api.actions(Some("pending&x=1")).await,
            Err(CloudError::Invalid(_))
        ));

        let done = api.action(id).await.unwrap();
        assert_eq!(done.status, ActionStatus::Succeeded);
        assert_eq!(done.result, Some(json!({ "message_id": "m1" })));
        assert_eq!(api.action_preview(id).await.unwrap().summary, a.summary);
        assert_eq!(
            api.approve_action(id, &"ab".repeat(32))
                .await
                .unwrap()
                .status,
            ActionStatus::Executing
        );
        assert_eq!(
            api.cancel_action(id).await.unwrap().status,
            ActionStatus::Cancelled
        );
        for bad in ["", "..", "a/b", "x?y"] {
            assert!(matches!(api.action(bad).await, Err(CloudError::Invalid(_))));
            assert!(matches!(
                api.approve_action(bad, "d").await,
                Err(CloudError::Invalid(_))
            ));
        }
    }

    #[tokio::test]
    async fn action_errors_keep_the_server_code() {
        let server = MockServer::start().await;
        let api = signed_in(&server).await;
        Mock::given(method("POST"))
            .and(path("/v1/actions/a1/approve"))
            .respond_with(err(412, "digest_mismatch", "The action changed."))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/actions"))
            .respond_with(err(409, "idempotency_conflict", "Key reused."))
            .mount(&server)
            .await;
        let e = api.approve_action("a1", "00").await.unwrap_err();
        assert!(matches!(&e, CloudError::Api { status: 412, .. }), "{e}");
        assert_eq!(e.code(), "digest_mismatch");
        let req = ProposeAction::typed(
            "slack",
            &ActionInput::PostMessage(PostMessage {
                channel: "C1".into(),
                text: "hi".into(),
                thread_ts: None,
            }),
            "k",
        );
        assert_eq!(req.kind, ActionKind::PostMessage);
        assert_eq!(req.input, json!({ "channel": "C1", "text": "hi" }));
        assert_eq!(
            api.propose_action(&req).await.unwrap_err().code(),
            "idempotency_conflict"
        );
    }

    #[test]
    fn kinds_and_statuses_round_trip() {
        for k in ActionKind::ALL {
            assert_eq!(ActionKind::parse(k.as_str()).unwrap(), k);
            assert_eq!(serde_json::to_value(k).unwrap(), json!(k.as_str()));
        }
        for s in ActionStatus::KNOWN {
            assert_eq!(ActionStatus::parse(s.as_str()).unwrap(), s);
            assert_eq!(serde_json::to_value(s).unwrap(), json!(s.as_str()));
        }
        assert!(ActionKind::parse("delete_everything").is_err());
        assert!(ActionStatus::parse("unknown").is_err());
    }

    #[tokio::test]
    async fn me_plans_and_billing() {
        let server = MockServer::start().await;
        let api = signed_in(&server).await;
        let device = api.session().device_id().await;
        Mock::given(method("GET"))
            .and(path("/v1/me"))
            .and(header(DEVICE_HEADER, device.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "user_id": "u1", "email": "me@example.com", "plan": "heed", "embeddings": true,
                "subscription": { "plan": "heed", "status": "trialing", "interval": "month",
                                  "trial_end": "2026-11-16T00:00:00Z", "current_period_end": null,
                                  "cancel_at_period_end": false, "future": 1 },
                "entitlements": { "plan": "heed", "name": "Heed", "max_connections": 10,
                                  "sync_interval_minutes": 30, "history_days": null,
                                  "smart_asks_per_month": 300, "default_model": "fast", "devices": 1,
                                  "ghost_text": true, "needs_you_inbox": true, "daily_brief": true,
                                  "actions_with_approval": true, "agent_tasks": false,
                                  "computer_use": false, "priority_support": false },
                "usage": { "month_cost_micros": 1200, "smart_asks_used": 7 }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/billing/plans"))
            .and(header(DEVICE_HEADER, device.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "plans": [
                { "id": "heed", "name": "Heed", "monthly_gbp": 39, "annual_gbp": 390, "trial_days": 14,
                  "entitlements": { "plan": "heed", "name": "Heed", "smart_asks_per_month": 300 } },
                { "id": "heed_max", "name": "Heed Max", "monthly_gbp": 99, "annual_gbp": 990, "trial_days": 14,
                  "entitlements": { "plan": "heed_max", "name": "Heed Max", "max_connections": null } }
            ]})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/billing/checkout"))
            .and(body_json(json!({ "plan": "heed_max", "interval": "year" })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "url": "https://checkout.stripe.com/c/x" })),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/billing/portal"))
            .and(body_json(json!({})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "url": "https://billing.stripe.com/p/x" })),
            )
            .mount(&server)
            .await;

        let me = api.me().await.unwrap();
        assert_eq!(me.plan.as_deref(), Some("heed"));
        let sub = me.subscription.unwrap();
        assert_eq!(
            (sub.status.as_str(), sub.trial_end.as_deref()),
            ("trialing", Some("2026-11-16T00:00:00Z"))
        );
        let ent = me.entitlements.unwrap();
        assert_eq!((ent.smart_asks_per_month, ent.history_days), (300, None));
        assert_eq!(me.usage.smart_asks_used, 7);
        let plans = api.plans().await.unwrap().plans;
        assert_eq!(plans[1].annual_gbp, 990);
        assert_eq!(plans[1].entitlements.max_connections, None);
        assert_eq!(
            api.checkout("heed_max", Interval::Year).await.unwrap().url,
            "https://checkout.stripe.com/c/x"
        );
        assert!(matches!(
            api.checkout("../x", Interval::Month).await,
            Err(CloudError::Invalid(_))
        ));
        assert_eq!(
            api.portal().await.unwrap().url,
            "https://billing.stripe.com/p/x"
        );
        assert!(Interval::parse("week").is_err());
    }

    #[tokio::test]
    async fn me_without_a_subscription_and_old_fields() {
        let server = MockServer::start().await;
        let api = signed_in(&server).await;
        Mock::given(method("GET"))
            .and(path("/v1/me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "user_id": "u1", "email": null, "plan": null, "embeddings": false,
                "subscription": null, "entitlements": null,
                "usage": { "month_cost_micros": 0, "quota_micros": 500 }
            })))
            .mount(&server)
            .await;
        let me = api.me().await.unwrap();
        assert_eq!(
            (me.plan, me.subscription, me.entitlements),
            (None, None, None)
        );
        assert_eq!(me.usage.quota_micros, Some(500));
    }

    #[tokio::test]
    async fn ask_sends_the_tier_and_reads_a_downgrade() {
        let server = MockServer::start().await;
        let api = signed_in(&server).await;
        Mock::given(method("POST"))
            .and(path("/v1/ask"))
            .and(body_json(json!({ "question": "q", "model": "smart" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "conversation_id": "c1", "answer": "a", "citations": [],
                "usage": { "input_tokens": 1, "output_tokens": 1 },
                "model_tier": "fast", "downgraded": true
            })))
            .mount(&server)
            .await;
        let a = api
            .ask(&AskRequest {
                question: "q".into(),
                conversation_id: None,
                model: Some(ModelTier::Smart),
            })
            .await
            .unwrap();
        assert_eq!(
            (a.model_tier.as_deref(), a.downgraded),
            (Some("fast"), true)
        );
    }

    #[tokio::test]
    async fn billing_errors_have_their_own_codes() {
        let server = MockServer::start().await;
        let api = signed_in(&server).await;
        Mock::given(method("POST"))
            .and(path("/v1/sync"))
            .respond_with(err(402, "subscription_required", "Start a trial to sync."))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/connections"))
            .respond_with(err(403, "device_limit_reached", "Too many devices."))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/connections"))
            .respond_with(err(403, "connection_limit_reached", "Plan limit."))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/billing/portal"))
            .respond_with(err(409, "no_customer", "Subscribe first."))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/export"))
            .respond_with(err(403, "forbidden", "no"))
            .mount(&server)
            .await;
        let e = api.sync().await.unwrap_err();
        assert!(matches!(&e, CloudError::SubscriptionRequired(m) if m == "Start a trial to sync."));
        assert_eq!(e.code(), "subscription_required");
        let e = api.connections().await.unwrap_err();
        assert!(matches!(e, CloudError::DeviceLimitReached(_)));
        assert_eq!(e.code(), "device_limit_reached");
        let e = api.connect("gmail").await.unwrap_err();
        assert_eq!(e.code(), "connection_limit_reached");
        let e = api.portal().await.unwrap_err();
        assert!(matches!(e, CloudError::NoCustomer(_)));
        assert_eq!(e.code(), "no_customer");
        let e = api.export().await.unwrap_err();
        assert!(matches!(e, CloudError::Api { status: 403, .. }));
    }

    #[tokio::test]
    async fn signed_out_calls_never_reach_the_server() {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let api = Api::new(Arc::new(session(
            &server.uri(),
            Default::default(),
            Default::default(),
        )));
        assert!(matches!(
            api.connections().await,
            Err(CloudError::NotSignedIn)
        ));
    }
}
