//! Heed cloud: the signed-in user's model endpoint (`{api}/llm`, Anthropic-Messages-compatible,
//! served by `nugget-cloud`).
//!
//! - Two providers, [`CLOUD_FAST`] (model `heed-fast`: triage, light work, ghost text) and
//!   [`CLOUD_SMART`] (`heed-smart`: drafting, asks, the agent). The service maps each tier to a
//!   real model by the user's plan. Both share ONE connection pool, so warming either (for
//!   example when a reply field gets focus) warms the connection the other will use.
//! - The user's Supabase access token is read from a [`CloudSession`] at every request, so a
//!   token refresh never rebuilds the providers or drops the warm connection. Without a valid
//!   token nothing is sent: the request fails as a provider error, the providers report
//!   [`Provider::available`] `false`, and routing falls back to local models. A `401` from the
//!   service clears the token until a fresh one is handed over.
//! - Locality is always [`Locality::Cloud`]: every request is in the egress ledger under the
//!   service's host, and every rule about what may leave the device (sensitive passages,
//!   screenshots, local-only triage) applies to it as to any cloud model.
//! - No retries before the stream starts: the service already falls back between providers, and
//!   a failure here should fall back to a local model at once (ghost text cannot wait).

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use nugget_core::capability::{Capabilities, Locality, Tier};
use nugget_core::egress::EgressSink;
use nugget_core::ids::{ItemRef, ProviderId};
use nugget_core::{Error, Result};
use serde_json::json;

use crate::adapters::anthropic::AnthropicProvider;
use crate::config::{ProviderConfig, Secret};
use crate::locality::{HostResolver, SystemResolver};
use crate::provider::{Prepared, Provider};
use crate::types::{ChatRequest, ChatStream, ProviderMeta};

/// Provider id of the fast tier.
pub const CLOUD_FAST: &str = "heed-cloud-fast";
/// Provider id of the smart tier.
pub const CLOUD_SMART: &str = "heed-cloud-smart";
/// The `model` each tier asks the service for.
pub const FAST_MODEL: &str = "heed-fast";
pub const SMART_MODEL: &str = "heed-smart";

/// Tiers served by the fast provider. Triage of local items stays local regardless: the
/// ingest pipeline only uses non-cloud providers for it.
pub const FAST_TIERS: &[Tier] = &[Tier::Triage, Tier::Light];
/// Tiers served by the smart provider.
pub const SMART_TIERS: &[Tier] = &[Tier::Synthesis, Tier::Agentic];

/// A token is treated as expired this many seconds early, so a request never carries one that
/// lapses on the way.
pub const EXPIRY_MARGIN_SECS: i64 = 15;

/// A bearer token read at each request.
pub trait BearerSource: Send + Sync {
    /// The current token; `None` when signed out or expired (then the request is not sent).
    fn bearer(&self) -> Option<Secret>;
    /// The server refused the token (HTTP 401).
    fn rejected(&self) {}
}

type Clock = Box<dyn Fn() -> i64 + Send + Sync>;

/// The access token handed over by the desktop app, kept in memory only.
pub struct CloudSession {
    token: RwLock<Option<(Secret, i64)>>,
    now: Clock,
}

impl Default for CloudSession {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for CloudSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloudSession")
            .field("expires_at", &self.expires_at())
            .finish_non_exhaustive()
    }
}

impl CloudSession {
    pub fn new() -> Self {
        Self::with_clock(Box::new(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or_default()
        }))
    }

    /// With a clock returning unix seconds (tests).
    pub fn with_clock(now: Clock) -> Self {
        Self {
            token: RwLock::new(None),
            now,
        }
    }

    /// Use `token` until `expires_at` (unix seconds).
    pub fn set(&self, token: Secret, expires_at: i64) {
        *self.token.write().unwrap_or_else(|e| e.into_inner()) = Some((token, expires_at));
    }

    pub fn clear(&self) {
        *self.token.write().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// The token, if there is one that is still valid.
    pub fn token(&self) -> Option<Secret> {
        let now = (self.now)();
        self.token
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|(_, exp)| exp - EXPIRY_MARGIN_SECS > now)
            .map(|(t, _)| t.clone())
    }

    /// When the held token expires (even if it already has).
    pub fn expires_at(&self) -> Option<i64> {
        self.token
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(_, e)| *e)
    }

    pub fn is_active(&self) -> bool {
        self.token().is_some()
    }
}

impl BearerSource for CloudSession {
    fn bearer(&self) -> Option<Secret> {
        self.token()
    }
    fn rejected(&self) {
        tracing::info!("Heed cloud refused the access token; waiting for a fresh one");
        self.clear();
    }
}

/// What the endpoint can do (it fronts current Claude models), so the router need not probe it.
pub fn cloud_capabilities() -> Capabilities {
    Capabilities {
        streaming: true,
        vision: true,
        tools: true,
        json_schema: true,
        embeddings: false,
        context_tokens: Some(200_000),
    }
}

/// The two cloud providers.
pub struct CloudProviders {
    pub fast: Arc<dyn Provider>,
    pub smart: Arc<dyn Provider>,
}

/// The `{api}/llm` root for a service base URL: `https`, or `http` to loopback (development and
/// tests). No credentials, query or fragment.
pub fn llm_base_url(api_base: &str) -> Result<String> {
    let url = url::Url::parse(api_base.trim())
        .map_err(|e| Error::Invalid(format!("cloud API base URL: {e}")))?;
    let loopback = match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    let scheme_ok = match url.scheme() {
        "https" => url.host().is_some(),
        "http" => loopback,
        _ => false,
    };
    if !scheme_ok
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Invalid(
            "cloud API base URL must be https (http only to this machine)".into(),
        ));
    }
    Ok(format!("{}/llm", url.as_str().trim_end_matches('/')))
}

fn config(id: &str, model: &str, base: &str, tiers: &[Tier]) -> Result<ProviderConfig> {
    let cfg: ProviderConfig = serde_json::from_value(json!({
        "id": id,
        "kind": "anthropic",
        "base_url": base,
        "model": model,
        "tiers": tiers,
        "locality": Locality::Cloud,
        "context_tokens": 200_000,
        "max_retries": 0,
        "connect_timeout_secs": 5,
        "idle_timeout_secs": 120,
    }))
    .map_err(|e| Error::Other(format!("cloud provider config: {e}")))?;
    cfg.validate()?;
    Ok(cfg)
}

/// Build both providers for the service at `api_base`. Resolves the host once (blocking), like
/// [`crate::build_provider`].
pub fn build_cloud_providers(
    api_base: &str,
    session: Arc<CloudSession>,
    egress: Arc<dyn EgressSink>,
) -> Result<CloudProviders> {
    build_with(api_base, session, egress, &SystemResolver)
}

pub(crate) fn build_with(
    api_base: &str,
    session: Arc<CloudSession>,
    egress: Arc<dyn EgressSink>,
    resolver: &dyn HostResolver,
) -> Result<CloudProviders> {
    let base = llm_base_url(api_base)?;
    let fast_cfg = config(CLOUD_FAST, FAST_MODEL, &base, FAST_TIERS)?;
    let smart_cfg = config(CLOUD_SMART, SMART_MODEL, &base, SMART_TIERS)?;
    let (locality, resolved) = fast_cfg.resolve_network(resolver)?;
    if locality != Locality::Cloud {
        return Err(Error::Integrity("the cloud endpoint must be Cloud".into()));
    }
    let prepared = Prepared {
        locality,
        resolved,
        headers: Vec::new(),
        bearer: Some(session.clone() as Arc<dyn BearerSource>),
    };
    let fast = AnthropicProvider::new(&fast_cfg, egress, None, prepared)?;
    let smart = fast.sibling(&smart_cfg);
    Ok(CloudProviders {
        fast: Arc::new(CloudProvider {
            inner: fast,
            session: session.clone(),
        }),
        smart: Arc::new(CloudProvider {
            inner: smart,
            session,
        }),
    })
}

/// An Anthropic adapter that is only available while the session holds a valid token.
struct CloudProvider {
    inner: AnthropicProvider,
    session: Arc<CloudSession>,
}

#[async_trait]
impl Provider for CloudProvider {
    fn id(&self) -> &ProviderId {
        self.inner.id()
    }
    fn meta(&self) -> ProviderMeta {
        self.inner.meta()
    }
    fn available(&self) -> bool {
        self.session.is_active()
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatStream> {
        self.inner.chat(req).await
    }
    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Err(Error::Unsupported(
            "Heed cloud does not serve embeddings to the desktop".into(),
        ))
    }
    async fn embed_query(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed(texts).await
    }
    async fn embed_probe(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed(texts).await
    }
    async fn embed_items(&self, texts: &[String], _items: &[ItemRef]) -> Result<Vec<Vec<f32>>> {
        self.embed(texts).await
    }
    fn embed_prefixes(&self) -> (String, String) {
        (String::new(), String::new())
    }
    async fn prewarm(&self) -> Result<()> {
        if !self.available() {
            return Ok(());
        }
        self.inner.prewarm().await
    }
    async fn prewarm_prefix(&self, req: &ChatRequest) -> Result<()> {
        if !self.available() {
            return Ok(());
        }
        self.inner.prewarm_prefix(req).await
    }
}

#[cfg(test)]
mod tests;
