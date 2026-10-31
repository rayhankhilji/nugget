//! Tier-based routing with capability-aware fallback.
//!
//! Each tier maps to an ordered list of provider ids. [`Router::route`] returns the first
//! candidate whose probed capabilities satisfy the requirement and which is not cooling down
//! after a failure (falling back to a cooling candidate only if nothing else qualifies).
//! Providers that have not been probed are only eligible when nothing beyond plain streaming
//! chat is required.
//!
//! Latency: [`Router::with_metrics`] installs a [`ChatMetrics`] hook that receives one
//! [`ChatMetric`] per stream opened by [`Router::chat`] (time to first event including fallbacks,
//! time to first token, total, and the provider's [`ChatStats`]). [`Router::prewarm`] warms the
//! provider a request on a tier would use.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use futures::StreamExt;
use nugget_core::capability::{Capabilities, Tier};
use nugget_core::ids::ProviderId;
use nugget_core::{Error, Result};

use crate::config::ProviderConfig;
use crate::probe::{ProbeOptions, ProbeReport, probe_with};
use crate::provider::Provider;
use crate::types::{
    ChatEvent, ChatRequest, ChatStats, ChatStream, StopReason, requires_nothing_probed, satisfies,
};

/// How a routed chat stream ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ChatOutcome {
    Done(StopReason),
    /// The stream yielded an error (its message).
    Error(String),
    /// The consumer dropped the stream before it finished (e.g. a cancelled draft).
    Dropped,
}

/// One routed chat, reported when its stream ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMetric {
    pub provider: ProviderId,
    pub tier: Tier,
    pub feature: String,
    /// Providers that failed before this one served the stream.
    pub fallbacks: u32,
    /// From [`Router::chat`] being called to the first stream event (includes routing,
    /// connection, fallbacks and the provider's time to first event).
    pub first_event: Duration,
    /// From [`Router::chat`] being called to the first text delta or tool call.
    pub first_token: Option<Duration>,
    /// From [`Router::chat`] being called to the end of the stream.
    pub total: Duration,
    pub outcome: ChatOutcome,
    /// The provider's usage and per-request timings ([`ChatEvent::Stats`]), if it sent them.
    pub stats: Option<ChatStats>,
}

/// Receives [`ChatMetric`]s. Called on the task polling the stream: keep it cheap and
/// non-blocking (record into a histogram or channel).
pub trait ChatMetrics: Send + Sync {
    fn record(&self, metric: ChatMetric);
}

impl<F: Fn(ChatMetric) + Send + Sync> ChatMetrics for F {
    fn record(&self, metric: ChatMetric) {
        self(metric)
    }
}

/// Reports a [`ChatMetric`] exactly once: when the stream ends, or when it is dropped.
struct MetricGuard {
    sink: Arc<dyn ChatMetrics>,
    started: Instant,
    metric: Option<ChatMetric>,
}

impl MetricGuard {
    fn observe(&mut self, ev: &Result<ChatEvent>) {
        let Some(m) = self.metric.as_mut() else {
            return;
        };
        match ev {
            Ok(ChatEvent::TextDelta(t)) if !t.is_empty() => {
                m.first_token.get_or_insert_with(|| self.started.elapsed());
            }
            Ok(ChatEvent::ToolCall { .. }) => {
                m.first_token.get_or_insert_with(|| self.started.elapsed());
            }
            Ok(ChatEvent::Stats(s)) => m.stats = Some(s.clone()),
            Ok(ChatEvent::Done { stop_reason, .. }) => {
                self.finish(ChatOutcome::Done(stop_reason.clone()));
            }
            Err(e) => self.finish(ChatOutcome::Error(e.to_string())),
            Ok(_) => {}
        }
    }

    fn finish(&mut self, outcome: ChatOutcome) {
        if let Some(mut m) = self.metric.take() {
            m.total = self.started.elapsed();
            m.outcome = outcome;
            self.sink.record(m);
        }
    }
}

impl Drop for MetricGuard {
    fn drop(&mut self) {
        self.finish(ChatOutcome::Dropped);
    }
}

/// Ordered provider ids per tier.
pub type TierMap = HashMap<Tier, Vec<ProviderId>>;

/// Build a tier map from configs: each provider joins the tiers it lists, in config order.
pub fn tier_map_from_configs(configs: &[ProviderConfig]) -> TierMap {
    let mut map = TierMap::new();
    for c in configs {
        for t in &c.tiers {
            let ids = map.entry(*t).or_default();
            if !ids.contains(&c.id) {
                ids.push(c.id.clone());
            }
        }
    }
    map
}

/// How long a provider is deprioritised after a failure.
pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(30);

pub struct Router {
    providers: HashMap<ProviderId, Arc<dyn Provider>>,
    order: Vec<ProviderId>,
    tiers: TierMap,
    caps: RwLock<HashMap<ProviderId, Capabilities>>,
    failed_at: Mutex<HashMap<ProviderId, Instant>>,
    cooldown: Duration,
    metrics: RwLock<Option<Arc<dyn ChatMetrics>>>,
}

impl Router {
    /// Tier entries naming unknown providers are ignored (with a warning).
    pub fn new(providers: Vec<Arc<dyn Provider>>, tier_map: TierMap) -> Self {
        let order: Vec<ProviderId> = providers.iter().map(|p| p.id().clone()).collect();
        let providers: HashMap<_, _> = providers.into_iter().map(|p| (p.id().clone(), p)).collect();
        for (tier, ids) in &tier_map {
            for id in ids {
                if !providers.contains_key(id) {
                    tracing::warn!(?tier, provider = %id, "tier references unknown provider");
                }
            }
        }
        Self {
            providers,
            order,
            tiers: tier_map,
            caps: RwLock::new(HashMap::new()),
            failed_at: Mutex::new(HashMap::new()),
            cooldown: DEFAULT_COOLDOWN,
            metrics: RwLock::new(None),
        }
    }

    pub fn with_cooldown(mut self, cooldown: Duration) -> Self {
        self.cooldown = cooldown;
        self
    }

    /// Report a [`ChatMetric`] for every stream [`Router::chat`] opens.
    pub fn with_metrics(self, metrics: Arc<dyn ChatMetrics>) -> Self {
        self.set_metrics(Some(metrics));
        self
    }

    /// Install (or remove) the metrics hook on a shared router.
    pub fn set_metrics(&self, metrics: Option<Arc<dyn ChatMetrics>>) {
        *self.metrics.write().unwrap_or_else(|e| e.into_inner()) = metrics;
    }

    /// Warm the provider that [`Router::chat`] would pick first for `tier` and `required`
    /// (plus whatever `prefix` itself requires) before the full request is ready, e.g. on a
    /// trigger press. With `prefix`, its stable part may be sent to warm the prompt cache (see
    /// [`Provider::prewarm_prefix`] for what leaves the machine); without, only the connection
    /// is warmed ([`Provider::prewarm`], no content). Returns the provider warmed.
    ///
    /// Routing can still differ by the time the real request is sent (a failure cooldown, or
    /// final context that needs more capabilities); the warm-up is then simply unused.
    pub async fn prewarm(
        &self,
        tier: Tier,
        required: &Capabilities,
        prefix: Option<&ChatRequest>,
    ) -> Result<Arc<dyn Provider>> {
        let mut need = required.clone();
        if let Some(p) = prefix {
            merge_required(&mut need, &p.required_capabilities());
        }
        let p = self.route(tier, &need)?;
        match prefix {
            Some(req) => p.prewarm_prefix(req).await?,
            None => p.prewarm().await?,
        }
        Ok(p)
    }

    /// Record probed (or otherwise known) capabilities.
    pub fn set_capabilities(&self, id: &ProviderId, caps: Capabilities) {
        self.caps
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), caps);
    }

    pub fn capabilities(&self, id: &ProviderId) -> Option<Capabilities> {
        self.caps
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    pub fn provider(&self, id: &ProviderId) -> Option<Arc<dyn Provider>> {
        self.providers.get(id).cloned()
    }

    /// All providers, in construction order.
    pub fn providers(&self) -> Vec<Arc<dyn Provider>> {
        self.order
            .iter()
            .filter_map(|id| self.providers.get(id).cloned())
            .collect()
    }

    pub fn tiers(&self) -> &TierMap {
        &self.tiers
    }

    /// Tiers a provider is assigned to.
    pub fn tiers_of(&self, id: &ProviderId) -> Vec<Tier> {
        let mut t: Vec<Tier> = self
            .tiers
            .iter()
            .filter(|(_, ids)| ids.contains(id))
            .map(|(t, _)| *t)
            .collect();
        t.sort_by_key(|t| *t as u8);
        t
    }

    /// Probe one provider and remember the result.
    pub async fn probe(&self, id: &ProviderId, opts: &ProbeOptions) -> Result<ProbeReport> {
        let p = self
            .provider(id)
            .ok_or_else(|| Error::NotFound(format!("provider {id}")))?;
        let report = probe_with(p.as_ref(), opts).await;
        self.set_capabilities(id, report.capabilities.clone());
        Ok(report)
    }

    /// Mark a provider as failing; it is tried last until the cooldown passes.
    pub fn report_failure(&self, id: &ProviderId) {
        self.failed_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), Instant::now());
    }

    pub fn report_success(&self, id: &ProviderId) {
        self.failed_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }

    fn cooling(&self, id: &ProviderId) -> bool {
        self.failed_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .is_some_and(|t| t.elapsed() < self.cooldown)
    }

    fn eligible(&self, id: &ProviderId, required: &Capabilities) -> bool {
        match self.capabilities(id) {
            Some(have) => satisfies(&have, required),
            None => requires_nothing_probed(required),
        }
    }

    /// Every provider for `tier` that satisfies `required`, healthy ones first, each group in
    /// configured order.
    pub fn candidates(&self, tier: Tier, required: &Capabilities) -> Vec<Arc<dyn Provider>> {
        let ids = self.tiers.get(&tier).map(Vec::as_slice).unwrap_or_default();
        let (healthy, cooling): (Vec<&ProviderId>, Vec<&ProviderId>) = ids
            .iter()
            .filter(|id| self.providers.contains_key(*id) && self.eligible(id, required))
            .partition(|id| !self.cooling(id));
        healthy
            .into_iter()
            .chain(cooling)
            .filter_map(|id| self.providers.get(id).cloned())
            .collect()
    }

    /// The best provider for `tier` that satisfies `required`.
    pub fn route(&self, tier: Tier, required: &Capabilities) -> Result<Arc<dyn Provider>> {
        if let Some(p) = self.candidates(tier, required).into_iter().next() {
            return Ok(p);
        }
        Err(self.no_candidate(tier, required))
    }

    fn no_candidate(&self, tier: Tier, required: &Capabilities) -> Error {
        let configured = self
            .tiers
            .get(&tier)
            .is_some_and(|ids| ids.iter().any(|id| self.providers.contains_key(id)));
        if configured {
            Error::Unsupported(format!(
                "no provider for tier {tier:?} has the required capabilities {required:?} (probe providers first)"
            ))
        } else {
            Error::NotFound(format!("no provider configured for tier {tier:?}"))
        }
    }

    /// Chat on `tier`, falling back to the next candidate if a provider fails before producing
    /// its first event. Requirements implied by the request (images, tools, JSON schema) are
    /// added to `required`. Returns the provider that is serving the stream.
    pub async fn chat(
        &self,
        tier: Tier,
        required: &Capabilities,
        req: ChatRequest,
    ) -> Result<(Arc<dyn Provider>, ChatStream)> {
        let started = Instant::now();
        let mut need = req.required_capabilities();
        merge_required(&mut need, required);
        need.context_tokens = required.context_tokens;
        let candidates = self.candidates(tier, &need);
        if candidates.is_empty() {
            return Err(self.no_candidate(tier, &need));
        }
        let mut last_err = None;
        let mut fallbacks = 0u32;
        for p in candidates {
            match self.try_chat(p.as_ref(), req.clone()).await {
                Ok(stream) => {
                    self.report_success(p.id());
                    let stream =
                        self.metered(stream, p.id(), tier, &req.feature, fallbacks, started);
                    return Ok((p, stream));
                }
                Err(e) if is_fallback_error(&e) => {
                    tracing::warn!(provider = %p.id(), error = %e, ?tier, "provider failed; trying next");
                    self.report_failure(p.id());
                    last_err = Some(e);
                    fallbacks += 1;
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or_else(|| self.no_candidate(tier, &need)))
    }

    /// Wrap `stream` so the metrics hook (if any) sees how it ends.
    fn metered(
        &self,
        stream: ChatStream,
        provider: &ProviderId,
        tier: Tier,
        feature: &str,
        fallbacks: u32,
        started: Instant,
    ) -> ChatStream {
        let Some(sink) = self
            .metrics
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return stream;
        };
        let mut guard = MetricGuard {
            sink,
            started,
            metric: Some(ChatMetric {
                provider: provider.clone(),
                tier,
                feature: feature.to_string(),
                fallbacks,
                // The first event has already arrived (see `try_chat`).
                first_event: started.elapsed(),
                first_token: None,
                total: Duration::ZERO,
                outcome: ChatOutcome::Dropped,
                stats: None,
            }),
        };
        let mut stream = stream;
        Box::pin(async_stream::stream! {
            while let Some(ev) = stream.next().await {
                guard.observe(&ev);
                yield ev;
            }
            guard.finish(ChatOutcome::Error("stream ended before completion".into()));
        })
    }

    /// Start a chat and wait for its first event, so early failures can fall back.
    async fn try_chat(&self, p: &dyn Provider, req: ChatRequest) -> Result<ChatStream> {
        let mut stream = p.chat(req).await?;
        match stream.next().await {
            Some(Ok(first)) => Ok(Box::pin(
                futures::stream::once(async { Ok(first) }).chain(stream),
            )),
            Some(Err(e)) => Err(e),
            None => Err(Error::Provider(format!("{}: empty stream", p.id()))),
        }
    }

    /// Embed documents with the `Embedding` tier, falling back across candidates.
    pub async fn embed(&self, texts: &[String]) -> Result<(ProviderId, Vec<Vec<f32>>)> {
        self.embed_as(texts, false).await
    }

    /// Embed search queries ([`Provider::embed_query`]) like [`Router::embed`].
    pub async fn embed_query(&self, texts: &[String]) -> Result<(ProviderId, Vec<Vec<f32>>)> {
        self.embed_as(texts, true).await
    }

    async fn embed_as(&self, texts: &[String], query: bool) -> Result<(ProviderId, Vec<Vec<f32>>)> {
        let required = Capabilities {
            embeddings: true,
            ..Default::default()
        };
        let candidates = self.candidates(Tier::Embedding, &required);
        if candidates.is_empty() {
            return Err(self.no_candidate(Tier::Embedding, &required));
        }
        let mut last_err = None;
        for p in candidates {
            let res = if query {
                p.embed_query(texts).await
            } else {
                p.embed(texts).await
            };
            match res {
                Ok(v) => {
                    self.report_success(p.id());
                    return Ok((p.id().clone(), v));
                }
                Err(e) if is_fallback_error(&e) => {
                    tracing::warn!(provider = %p.id(), error = %e, "embedding provider failed; trying next");
                    self.report_failure(p.id());
                    last_err = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or_else(|| self.no_candidate(Tier::Embedding, &required)))
    }
}

fn merge_required(need: &mut Capabilities, extra: &Capabilities) {
    need.streaming |= extra.streaming;
    need.vision |= extra.vision;
    need.tools |= extra.tools;
    need.json_schema |= extra.json_schema;
    need.embeddings |= extra.embeddings;
}

/// Failures worth trying another provider for: transient ones (`Network`, `RateLimited`) and
/// provider-side ones. Request errors (`Invalid`) and cancellation are not: another provider
/// would reject the same request or the caller gave up.
fn is_fallback_error(e: &Error) -> bool {
    matches!(
        e,
        Error::Network(_)
            | Error::RateLimited { .. }
            | Error::Provider(_)
            | Error::Io(_)
            | Error::Unsupported(_)
            | Error::Other(_)
    )
}
