//! The provider trait and construction from config.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use nugget_core::egress::EgressSink;
use nugget_core::ids::ProviderId;
use nugget_core::{Error, Result};
use reqwest::header::{HeaderName, HeaderValue};

use crate::adapters::openai_responses::OpenAiResponsesProvider;
use crate::adapters::{anthropic::AnthropicProvider, openai_compatible::OpenAiCompatibleProvider};
use crate::config::{ProviderConfig, Secret, SecretResolver, SystemSecrets};
use crate::http::{Http, HttpSettings};
use crate::types::{ChatRequest, ChatStream, ProviderKind, ProviderMeta};

/// One model behind one API. Implementations are cheap to share (`Arc<dyn Provider>`).
#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> &ProviderId;
    fn meta(&self) -> ProviderMeta;
    /// Start a streaming chat. Transient HTTP failures are retried before the stream starts;
    /// errors after that arrive as `Err` items and end the stream.
    async fn chat(&self, req: ChatRequest) -> Result<ChatStream>;
    /// Embed texts, one vector per input in order. `Error::Unsupported` if the API has none.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

/// Build a provider, resolving its API key from the environment or OS keychain.
pub fn build_provider(
    cfg: &ProviderConfig,
    egress: Arc<dyn EgressSink>,
) -> Result<Arc<dyn Provider>> {
    build_provider_with(cfg, egress, &SystemSecrets)
}

/// [`build_provider`] with an explicit secret resolver.
pub fn build_provider_with(
    cfg: &ProviderConfig,
    egress: Arc<dyn EgressSink>,
    secrets: &dyn SecretResolver,
) -> Result<Arc<dyn Provider>> {
    cfg.validate()?;
    let key = cfg
        .api_key
        .as_ref()
        .map(|k| {
            secrets.resolve(k).map_err(|e| match e {
                Error::NotFound(m) => {
                    Error::NotFound(format!("provider {}: api_key {k}: {m}", cfg.id))
                }
                other => other,
            })
        })
        .transpose()?;
    Ok(match cfg.kind {
        ProviderKind::Anthropic => Arc::new(AnthropicProvider::new(cfg, egress, key)?),
        ProviderKind::OpenaiResponses => Arc::new(OpenAiResponsesProvider::new(cfg, egress, key)?),
        ProviderKind::OpenaiCompatible => {
            Arc::new(OpenAiCompatibleProvider::new(cfg, egress, key)?)
        }
    })
}

/// State every adapter shares.
pub(crate) struct Common {
    pub id: ProviderId,
    pub meta: ProviderMeta,
    pub http: Http,
    pub cfg: ProviderConfig,
}

impl Common {
    pub(crate) fn new(
        cfg: &ProviderConfig,
        egress: Arc<dyn EgressSink>,
        auth: Vec<(HeaderName, HeaderValue)>,
    ) -> Result<Self> {
        let base = cfg.base_url()?;
        let locality = cfg.effective_locality()?;
        let settings = HttpSettings {
            connect_timeout: Duration::from_secs(cfg.connect_timeout_secs.unwrap_or(10)),
            idle_timeout: Duration::from_secs(cfg.idle_timeout_secs.unwrap_or(600)),
            max_retries: cfg.max_retries.unwrap_or(2),
            retry_base: Duration::from_millis(cfg.retry_base_ms.unwrap_or(500)),
            max_retry_delay: Duration::from_secs(60),
            headers: cfg.headers.clone(),
        };
        let http = Http::new(cfg.id.clone(), base, locality, egress, settings, auth)?;
        Ok(Self {
            id: cfg.id.clone(),
            meta: ProviderMeta {
                kind: cfg.kind,
                model: cfg.model.clone(),
                locality,
                context_tokens: cfg.context_tokens,
            },
            http,
            cfg: cfg.clone(),
        })
    }
}

/// The auth header for a resolved key (`value` formats the secret), marked sensitive so it is
/// never logged. No key, no header.
pub(crate) fn auth_header(
    name: &'static str,
    key: Option<Secret>,
    value: impl Fn(&str) -> String,
) -> Result<Vec<(HeaderName, HeaderValue)>> {
    let Some(key) = key else {
        return Ok(Vec::new());
    };
    let mut v = HeaderValue::from_str(&value(key.expose())).map_err(|_| {
        Error::Invalid("api key contains characters not allowed in an HTTP header".into())
    })?;
    v.set_sensitive(true);
    Ok(vec![(HeaderName::from_static(name), v)])
}
