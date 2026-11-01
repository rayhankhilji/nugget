//! The on-device runtime (`nugget-runtime`) in the router, and routing order.
//!
//! Each runtime role gets a [`RuntimeSlot`] in the router from the start (`local-chat`,
//! `local-embedding`, ...): a [`Provider`] that forwards to the provider built from the config
//! [`Runtime::ensure`] returns, starting the server on demand, since local servers come and go.
//!
//! - Until its model is installed a slot fails fast with `Unsupported` (router falls back).
//! - The rerank slot is never a router provider; retrieval reaches it through [`crate::rerank`].
//! - The draft slot (`[draft.local] model`) joins the router without tiers: only the draft
//!   engine asks for it, by id.
//! - A slot starts routable for nothing. An installed chat slot whose server is starting is
//!   routable for chat ([`RuntimeSlot::starting`]) so a chat waits for the model to load instead
//!   of finding no candidate; other slots become routable once their server is first ready.
//! - Interactive chats on slots and on configured non-cloud providers ([`ForegroundAware`])
//!   hold ingest's foreground guard for their stream, so background triage yields to them.
//!
//! **Routing order**, per tier: configured `LocalNetwork` providers and probed `OnDevice` ones
//! (config order), then the runtime slot, then unprobed `OnDevice` providers (a server that is
//! not running would cost every request a failed connection), then `Cloud` providers.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use async_trait::async_trait;
use nugget_core::capability::{Capabilities, Locality, Tier};
use nugget_core::egress::EgressSink;
use nugget_core::ids::{ItemRef, ProviderId};
use nugget_core::ipc::{RuntimeInfo, RuntimeRole, RuntimeState};
use nugget_core::{Error, Result};
use nugget_infer::{
    ChatRequest, ChatStream, Provider, ProviderConfig, ProviderKind, ProviderMeta, Router, TierMap,
    build_provider_with,
};
use nugget_models::{Hardware, Manifest, ModelEntry, recommend};
use nugget_runtime::{Runtime, provider_id};

use crate::config::Config;
use crate::features::draft::local_draft_model;

/// The longest a chat whose server crashed before it streamed anything waits for the restarted
/// server (its backoff plus reloading the model) before it gives up with a plain error.
const CHAT_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_secs(15);

/// Tiers the local chat server always serves, whatever else its model lists.
const CHAT_TIERS: [Tier; 3] = [Tier::Triage, Tier::Light, Tier::Synthesis];

/// The embedding model of a store whose vectors predate the record of their model
/// ([`nugget_store::Store::embedding_model`]): the only embedding model the manifest had then.
pub(crate) const LEGACY_EMBEDDING_MODEL: &str = "nomic-embed-text-v1.5-q8";

/// The manifest model each runtime role should run, per config and hardware.
#[cfg(test)]
pub(crate) fn selected_models(
    cfg: &Config,
    manifest: &Manifest,
    hw: &Hardware,
) -> Vec<(RuntimeRole, ModelEntry)> {
    selected_models_for_index(cfg, manifest, hw, None)
}

/// Record [`LEGACY_EMBEDDING_MODEL`] as the embedding model of a store that has vectors but no
/// recorded model (its vectors predate the record, and v1.5 was the only embedding model the
/// manifest had). Does nothing otherwise. Returns the model recorded, if this call recorded one.
pub(crate) fn pin_legacy_embedding_model(store: &nugget_store::Store) -> Result<Option<String>> {
    if store.embedding_model()?.is_some() || store.embedding_dim()?.is_none() {
        return Ok(None);
    }
    let model = store.adopt_embedding_model(LEGACY_EMBEDDING_MODEL)?;
    tracing::info!(%model, "recorded the embedding model of an index built before models were recorded");
    Ok(Some(model))
}

/// The embedding model `store`'s vectors were built with: the recorded one, or
/// [`LEGACY_EMBEDDING_MODEL`] for vectors from before models were recorded; `None` for a store
/// without vectors (or that cannot be read).
pub(crate) fn index_embedding_model(store: &nugget_store::Store) -> Option<String> {
    match store.embedding_model() {
        Ok(Some(m)) => Some(m),
        Ok(None) => match store.embedding_dim() {
            Ok(Some(_)) => Some(LEGACY_EMBEDDING_MODEL.to_owned()),
            _ => None,
        },
        Err(e) => {
            tracing::warn!(error = %e, "cannot read the index's embedding model");
            None
        }
    }
}

/// The manifest model each runtime role should run, per config, hardware and the embedding
/// model `index_model` the store's vectors were built with ([`index_embedding_model`]).
///
/// Without an explicit embedding model, a store with vectors keeps the model that built them:
/// two models' vectors are not comparable, so switching would orphan every stored vector.
pub(crate) fn selected_models_for_index(
    cfg: &Config,
    manifest: &Manifest,
    hw: &Hardware,
    index_model: Option<&str>,
) -> Vec<(RuntimeRole, ModelEntry)> {
    let pick = |explicit: Option<&String>, fallback: Option<&String>, tiers: &[Tier]| {
        explicit.or(fallback).map_or_else(
            || {
                let built_with = index_model
                    .filter(|_| tiers.contains(&Tier::Embedding))
                    .and_then(|id| manifest.get(id))
                    .filter(|e| e.tiers.contains(&Tier::Embedding));
                built_with
                    .or_else(|| tiers.iter().find_map(|t| recommend(manifest, hw, *t)))
                    .cloned()
            },
            |id| {
                let found = manifest.get(id).cloned();
                if found.is_none() {
                    tracing::warn!(model = %id, "the selected local model is not in the manifest");
                }
                found
            },
        )
    };
    let mut out = Vec::new();
    let chat = pick(
        cfg.runtime.chat_model.as_ref(),
        cfg.models.selected.triage.as_ref(),
        &[Tier::Triage, Tier::Synthesis],
    );
    if let Some(m) = chat {
        if m.tiers.iter().any(|t| t.is_chat()) {
            out.push((RuntimeRole::Chat, m));
        } else {
            tracing::warn!(model = %m.id, "runtime.chat_model serves no chat tier; no local chat");
        }
    }
    let embedding = pick(
        cfg.runtime.embedding_model.as_ref(),
        cfg.models.selected.embedding.as_ref(),
        &[Tier::Embedding],
    );
    if let Some(m) = embedding {
        if m.tiers.contains(&Tier::Embedding) {
            out.push((RuntimeRole::Embedding, m));
        } else {
            tracing::warn!(model = %m.id, "runtime.embedding_model does not embed; no local embeddings");
        }
    }
    // Only when configured: a reranker is opt-in, never recommended.
    if let Some(id) = &cfg.runtime.rerank_model {
        match manifest.get(id) {
            Some(m) if m.tiers.contains(&Tier::Rerank) => {
                out.push((RuntimeRole::Rerank, m.clone()));
            }
            Some(m) => {
                tracing::warn!(model = %m.id, "runtime.rerank_model is not a reranker; no local reranker");
            }
            None => {
                tracing::warn!(model = %id, "runtime.rerank_model is not in the manifest; no local reranker");
            }
        }
    }
    // Only when configured: the Double Tap model is opt-in.
    if let Some(id) = local_draft_model(&cfg.draft) {
        match manifest.get(&id) {
            Some(m) if m.tiers.iter().any(|t| t.is_chat()) && !m.tiers.contains(&Tier::Rerank) => {
                out.push((RuntimeRole::Draft, m.clone()));
            }
            Some(m) => {
                tracing::warn!(model = %m.id, "draft.local.model serves no chat tier; no local Double Tap model");
            }
            None => {
                tracing::warn!(model = %id, "draft.local.model is not in the manifest; no local Double Tap model");
            }
        }
    }
    out
}

/// Tiers a role's slot is routed for.
pub(crate) fn slot_tiers(role: RuntimeRole, model: &ModelEntry) -> Vec<Tier> {
    match role {
        RuntimeRole::Embedding => vec![Tier::Embedding],
        // Retrieval calls the reranker directly: the slot never joins the router (see
        // `LocalRuntime::routed`), the tier is only reported by `providers.list`.
        RuntimeRole::Rerank => vec![Tier::Rerank],
        // Never routed by tier: only the draft engine asks for it, by id.
        RuntimeRole::Draft => Vec::new(),
        RuntimeRole::Chat => {
            let mut t: Vec<Tier> = CHAT_TIERS.to_vec();
            for x in &model.tiers {
                if x.is_chat() && !t.contains(x) {
                    t.push(*x);
                }
            }
            t.sort_by_key(|t| *t as u8);
            t
        }
    }
}

/// Capabilities a ready local server is known to have.
fn known_capabilities(role: RuntimeRole, cfg: &ProviderConfig) -> Capabilities {
    match role {
        RuntimeRole::Chat | RuntimeRole::Draft => Capabilities {
            streaming: true,
            json_schema: true,
            context_tokens: cfg.context_tokens,
            ..Capabilities::default()
        },
        RuntimeRole::Embedding => Capabilities {
            embeddings: true,
            ..Capabilities::default()
        },
        RuntimeRole::Rerank => Capabilities::default(),
    }
}

/// Capabilities of an installed slot whose server is starting but not ready yet: those of
/// [`known_capabilities`] that do not depend on the server's config. Only the chat role is
/// routed before its server is ready (see the module docs).
fn starting_capabilities(role: RuntimeRole) -> Option<Capabilities> {
    match role {
        RuntimeRole::Chat => Some(Capabilities {
            streaming: true,
            json_schema: true,
            context_tokens: None,
            ..Capabilities::default()
        }),
        RuntimeRole::Embedding | RuntimeRole::Draft | RuntimeRole::Rerank => None,
    }
}

/// Where a configured provider goes relative to the runtime slots.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    BeforeRuntime,
    AfterRuntime,
    Last,
}

fn place(loc: Locality, probed: bool) -> Place {
    match loc {
        Locality::Cloud => Place::Last,
        Locality::OnDevice if !probed => Place::AfterRuntime,
        _ => Place::BeforeRuntime,
    }
}

/// The routing order described in the module docs. `configured` are the built user providers
/// in config order with their tiers, locality and whether a probe of them has succeeded (a
/// stored probe result); `local` the runtime slots with their tiers.
pub(crate) fn tier_map(
    configured: &[(ProviderId, Vec<Tier>, Locality, bool)],
    local: &[(ProviderId, Vec<Tier>)],
) -> TierMap {
    let mut map = TierMap::new();
    for tier in Tier::ROUTED {
        let user = |at: Place| {
            configured
                .iter()
                .filter(move |(_, tiers, loc, probed)| {
                    tiers.contains(&tier) && place(*loc, *probed) == at
                })
                .map(|(id, ..)| id.clone())
        };
        let mut ids: Vec<ProviderId> = Vec::new();
        let runtime = local
            .iter()
            .filter(|(_, tiers)| tiers.contains(&tier))
            .map(|(id, _)| id.clone());
        for id in user(Place::BeforeRuntime)
            .chain(runtime)
            .chain(user(Place::AfterRuntime))
            .chain(user(Place::Last))
        {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if !ids.is_empty() {
            map.insert(tier, ids);
        }
    }
    map
}

struct Active {
    cfg: ProviderConfig,
    provider: Arc<dyn Provider>,
}

/// Hold a background chat (not interactive, not triage) while an interactive chat on the same
/// slot is in flight or just ended: the proactive judge until the slot is free (nothing times
/// it out, and it is preempted anyway, see [`preempt_on_foreground`]); other background work
/// (knowledge) for at most `max_wait`, so its caller's timeout is never reached because of it.
pub(crate) async fn yield_to_interactive(
    fg: &nugget_ingest::Foreground,
    feature: &str,
    max_wait: std::time::Duration,
) {
    if nugget_ingest::is_interactive(feature)
        || feature == nugget_ingest::FEATURE_TRIAGE
        || !fg.busy()
    {
        return;
    }
    if feature == nugget_proactive::judge::FEATURE_JUDGE {
        fg.idle().await;
    } else {
        let _ = tokio::time::timeout(max_wait, fg.idle()).await;
    }
}

/// Longest knowledge extraction waits for interactive chats on the same slot before it is sent
/// anyway (under the 120 s timeout knowledge gives a request before it marks the provider as
/// failing).
const BACKGROUND_MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(90);

/// End a proactive judge stream as soon as an interactive chat starts: dropping the inner
/// stream closes the connection, which cancels the request on the server, so the Ask gets the
/// slot instead of queueing behind up to `max_tokens` of judge output. The judge sees an error
/// and the proactive pass ranks without it (it does not mark the provider as failing).
/// Other features are returned unchanged.
pub(crate) fn preempt_on_foreground(
    stream: ChatStream,
    fg: Option<Arc<nugget_ingest::Foreground>>,
    feature: &str,
) -> ChatStream {
    let Some(fg) = fg.filter(|_| feature == nugget_proactive::judge::FEATURE_JUDGE) else {
        return stream;
    };
    use futures::StreamExt;
    Box::pin(futures::stream::unfold(
        Some((stream, fg)),
        |state| async move {
            let (mut stream, fg) = state?;
            tokio::select! {
                biased;
                _ = fg.started() => Some((
                    Err(Error::Provider(
                        "proactive judge preempted by an interactive request".into(),
                    )),
                    None,
                )),
                ev = stream.next() => ev.map(|ev| (ev, Some((stream, fg)))),
            }
        },
    ))
}

/// Called with a slot's role each time its server becomes ready on a new config (first start,
/// restart on another port, new model). The daemon uses it to re-embed text-only items.
pub(crate) type ReadyHook = Arc<dyn Fn(RuntimeRole) + Send + Sync>;

/// One runtime role in the router (see the module docs).
pub(crate) struct RuntimeSlot {
    id: ProviderId,
    role: RuntimeRole,
    /// Changes only when the index switches embedding model ([`RuntimeSlot::switch_model`]).
    model: Mutex<ModelEntry>,
    tiers: Vec<Tier>,
    runtime: Runtime,
    egress: Arc<dyn EgressSink>,
    installed: AtomicBool,
    active: Mutex<Option<Active>>,
    router: OnceLock<Weak<Router>>,
    on_ready: OnceLock<ReadyHook>,
    /// Interactive chats hold a guard on it so background triage yields.
    foreground: OnceLock<Arc<nugget_ingest::Foreground>>,
}

impl RuntimeSlot {
    pub fn model(&self) -> ModelEntry {
        self.model
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Serve `model` from now on: the running server is stopped and started again on it. Until
    /// it is ready the slot reports the model its server still runs (see `meta`), so callers that
    /// check the model (retrieval, ingest) never mix two models' vectors.
    pub async fn switch_model(&self, model: ModelEntry) -> Result<Arc<dyn Provider>> {
        *self.model.lock().unwrap_or_else(PoisonError::into_inner) = model;
        self.runtime.stop(self.role).await;
        self.refresh().await
    }

    pub fn id(&self) -> &ProviderId {
        &self.id
    }

    pub fn role(&self) -> RuntimeRole {
        self.role
    }

    /// Tiers the slot serves (for the rerank slot, which is not routed, only `rerank`).
    pub fn tiers(&self) -> &[Tier] {
        &self.tiers
    }

    pub fn is_installed(&self) -> bool {
        self.installed.load(Ordering::SeqCst)
    }

    pub fn set_installed(&self, yes: bool) {
        self.installed.store(yes, Ordering::SeqCst);
    }

    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// The config of this role's server if it is installed and ready right now (counts as
    /// activity for the idle timer). Never starts or waits for anything.
    pub fn ready_config(&self) -> Option<ProviderConfig> {
        if !self.is_installed() {
            return None;
        }
        let d = self.runtime.details(self.role)?;
        if d.info.state != RuntimeState::Ready {
            return None;
        }
        let cfg = d.config?;
        self.runtime.touch(self.role);
        Some(cfg)
    }

    /// The role's server state (`None` if it never started).
    pub fn state(&self) -> Option<RuntimeState> {
        self.runtime.details(self.role).map(|d| d.info.state)
    }

    fn not_installed(&self) -> Error {
        Error::Unsupported(format!(
            "local model `{}` is not installed",
            self.model().id
        ))
    }

    /// Start (or reuse) the server and return the provider in front of it.
    pub async fn ready(&self) -> Result<Arc<dyn Provider>> {
        if !self.is_installed() {
            return Err(self.not_installed());
        }
        if let Some(d) = self.runtime.details(self.role)
            && d.info.state == RuntimeState::Ready
            && let Some(cfg) = d.config
        {
            self.runtime.touch(self.role);
            return self.activate(cfg);
        }
        self.refresh().await
    }

    /// Run [`Runtime::ensure`] even while a server is up (it restarts the server on a new
    /// model after `models.install`), then route the result.
    pub async fn refresh(&self) -> Result<Arc<dyn Provider>> {
        if !self.is_installed() {
            return Err(self.not_installed());
        }
        let cfg = self
            .runtime
            .ensure(self.role, &self.model())
            .await
            .map_err(|e| unavailable(self.role, e))?;
        self.activate(cfg)
    }

    /// The daemon is starting this installed slot's server (in the background): until it is
    /// first ready, publish the role's config-independent capabilities so requests route to
    /// the slot and wait for the server (chat only; see the module docs). No-op once active.
    pub fn starting(&self) {
        let Some(caps) = starting_capabilities(self.role) else {
            return;
        };
        let active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        if active.is_some() || !self.is_installed() {
            return;
        }
        if let Some(router) = self.router.get().and_then(Weak::upgrade) {
            router.set_capabilities(&self.id, caps);
        }
    }

    /// The background start [`RuntimeSlot::starting`] announced failed: unless the server was
    /// ready before, the slot is routable for nothing again so requests skip it. A launched
    /// server that failed (state `failed`) stays routable: the runtime then fails requests at
    /// once with a plain sentence, which the user sees when there is no other candidate.
    pub fn start_failed(&self) {
        if starting_capabilities(self.role).is_none() || self.state() == Some(RuntimeState::Failed)
        {
            return;
        }
        let active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        if active.is_some() {
            return;
        }
        if let Some(router) = self.router.get().and_then(Weak::upgrade) {
            router.set_capabilities(&self.id, Capabilities::default());
        }
    }

    /// Use `cfg` (from `Runtime::ensure`): build its provider unless unchanged, and publish the
    /// role's capabilities to the router.
    pub fn activate(&self, cfg: ProviderConfig) -> Result<Arc<dyn Provider>> {
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(a) = active.as_ref()
            && a.cfg == cfg
        {
            return Ok(a.provider.clone());
        }
        let provider = build_provider_with(&cfg, self.egress.clone(), &self.runtime.secrets())
            .map_err(|e| unavailable(self.role, e))?;
        if self.role != RuntimeRole::Rerank {
            if let Some(router) = self.router.get().and_then(Weak::upgrade) {
                router.set_capabilities(&self.id, known_capabilities(self.role, &cfg));
            }
            tracing::info!(provider = %self.id, model = %cfg.model, "local runtime routed");
        }
        *active = Some(Active {
            cfg,
            provider: provider.clone(),
        });
        drop(active);
        if let Some(hook) = self.on_ready.get() {
            hook(self.role);
        }
        Ok(provider)
    }

    /// Start a chat and wait for its first event: an error before anything streamed is
    /// returned as the error (so the caller can retry), anything else is put back in front.
    async fn chat_started(&self, req: ChatRequest) -> Result<ChatStream> {
        use futures::StreamExt;
        let mut stream = self.ready().await?.chat(req).await?;
        match stream.next().await {
            Some(Err(e)) => Err(e),
            Some(Ok(first)) => Ok(Box::pin(
                futures::stream::once(async { Ok(first) }).chain(stream),
            )),
            None => Ok(Box::pin(futures::stream::empty())),
        }
    }

    /// Whether the role's server failed since a request began on the server with
    /// `pid_before`: it is no longer ready, or another process serves now (restarted).
    async fn server_failed(&self, pid_before: Option<u32>) -> bool {
        for attempt in 0..2 {
            let failed = match self.runtime.details(self.role) {
                Some(d) => d.info.state != RuntimeState::Ready || d.pid != pid_before,
                None => false,
            };
            if failed {
                return true;
            }
            // The connection can close a moment before the exit can be collected.
            if attempt == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }
        }
        false
    }

    /// How long a crashed server is worth waiting for: the supervisor's backoff plus the
    /// model's expected load time, within [`CHAT_RETRY_BUDGET`] (no point waiting when the
    /// reload alone takes longer: the user gets the plain error at once instead).
    fn retry_wait(&self) -> std::time::Duration {
        use std::time::Duration;
        let Some(d) = self.runtime.details(self.role) else {
            return Duration::ZERO;
        };
        let backoff = Duration::from_millis(d.info.retry_in_ms.unwrap_or(0));
        let load = d.expected_load.unwrap_or(CHAT_RETRY_BUDGET);
        // The supervisor may not have scheduled its restart yet (no `retry_in_ms`): allow for
        // its first backoff at least.
        let need = (backoff + load + Duration::from_secs(1)).max(Duration::from_secs(4));
        if need > CHAT_RETRY_BUDGET {
            Duration::ZERO
        } else {
            need
        }
    }

    /// The plain error for a server that failed and is not back yet (from the runtime's
    /// status, which says whether it crashed, hung or is loading).
    fn down_error(&self) -> Error {
        let d = self.runtime.details(self.role);
        let msg = d
            .and_then(|d| d.info.error)
            .unwrap_or_else(|| "The on-device chat model is restarting.".into());
        // Say what to do once: messages that already name the next step keep theirs.
        if msg.contains("Open Status") || msg.contains("Try again") {
            Error::Provider(msg)
        } else {
            Error::Provider(format!("{msg} Try again in a moment."))
        }
    }

    /// The ready provider of an embedding slot.
    async fn embedder(&self) -> Result<Arc<dyn Provider>> {
        if self.role != RuntimeRole::Embedding {
            return Err(Error::Unsupported(format!("{} does not embed", self.id)));
        }
        self.ready().await
    }

    fn context_tokens(&self) -> Option<u32> {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(|a| a.cfg.context_tokens)
    }
}

#[async_trait]
impl Provider for RuntimeSlot {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    /// `model` is what the server runs (it lags [`RuntimeSlot::switch_model`] until restarted).
    fn meta(&self) -> ProviderMeta {
        let running = self
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|a| a.cfg.model.clone());
        ProviderMeta {
            kind: ProviderKind::OpenaiCompatible,
            model: running.unwrap_or_else(|| self.model().id),
            locality: Locality::OnDevice,
            context_tokens: self.context_tokens(),
        }
    }

    /// Until its model is installed the slot fails fast (`Unsupported`).
    fn available(&self) -> bool {
        self.is_installed()
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatStream> {
        if !matches!(self.role, RuntimeRole::Chat | RuntimeRole::Draft) {
            return Err(Error::Unsupported(format!("{} does not chat", self.id)));
        }
        // Interactive chats hold the foreground for their stream so background triage yields;
        // other background work waits for interactive chats so it never takes the server's one
        // slot just ahead of an Ask.
        let fg = self.foreground.get().cloned();
        if let Some(fg) = &fg {
            yield_to_interactive(fg, &req.feature, BACKGROUND_MAX_WAIT).await;
        }
        let guard = interactive_guard(&self.foreground, &req);
        let feature = req.feature.clone();
        // In flight until the stream ends: the hang watchdog checks the server often meanwhile.
        let busy = self.runtime.request(self.role);
        let before = self.runtime.details(self.role);
        let pid_before = before.as_ref().and_then(|d| d.pid);
        let hangs_before = before.map_or(0, |d| d.hangs);
        let stream = match self.chat_started(req.clone()).await {
            Ok(s) => s,
            // The server hung and the watchdog killed it: say so at once. No automatic resend:
            // the same request might hang it again, and the answer is due within seconds.
            Err(e)
                if self
                    .runtime
                    .details(self.role)
                    .is_some_and(|d| d.hangs != hangs_before) =>
            {
                tracing::warn!(provider = %self.id, error = %e, "local server hung during a chat");
                return Err(self.down_error());
            }
            // The server crashed (and is restarting) before anything streamed: retry once if
            // it is back within a moment, else say plainly what happened.
            Err(e) if pid_before.is_some() && self.server_failed(pid_before).await => {
                tracing::warn!(provider = %self.id, error = %e,
                    "local server failed before answering; retrying once");
                match self.runtime.wait_ready(self.role, self.retry_wait()).await {
                    Some(_) => self.chat_started(req).await?,
                    None => return Err(self.down_error()),
                }
            }
            Err(e) => return Err(e),
        };
        // In flight for the watchdog, and the guard held, until the stream ends.
        let stream: ChatStream = Box::pin(futures::StreamExt::map(stream, move |ev| {
            let _held = &busy;
            ev
        }));
        Ok(preempt_on_foreground(
            hold_guard(stream, guard),
            fg,
            &feature,
        ))
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embedder().await?.embed(texts).await
    }

    async fn embed_query(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embedder().await?.embed_query(texts).await
    }

    async fn embed_items(&self, texts: &[String], items: &[ItemRef]) -> Result<Vec<Vec<f32>>> {
        self.embedder().await?.embed_items(texts, items).await
    }

    async fn embed_probe(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embedder().await?.embed_probe(texts).await
    }

    /// The served model's task prefixes: from its config once running, else from its id (the
    /// runtime serves a model under its manifest id).
    fn embed_prefixes(&self) -> (String, String) {
        match self
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            Some(a) => nugget_infer::embed_prefixes(&a.cfg),
            None => nugget_infer::model_embed_prefixes(&self.model().id),
        }
    }

    /// Start the server if it stopped (Double Tap trigger), then warm the ready provider.
    async fn prewarm(&self) -> Result<()> {
        self.ready().await?.prewarm().await
    }

    async fn prewarm_prefix(&self, req: &ChatRequest) -> Result<()> {
        self.ready().await?.prewarm_prefix(req).await
    }
}

/// A [`nugget_ingest::ForegroundGuard`] for `req` if it is interactive and `fg` is set.
fn interactive_guard(
    fg: &OnceLock<Arc<nugget_ingest::Foreground>>,
    req: &ChatRequest,
) -> Option<nugget_ingest::ForegroundGuard> {
    nugget_ingest::is_interactive(&req.feature)
        .then(|| fg.get().map(|f| f.begin()))
        .flatten()
}

/// Keep `guard` alive for as long as `stream` lives.
fn hold_guard(stream: ChatStream, guard: Option<nugget_ingest::ForegroundGuard>) -> ChatStream {
    match guard {
        Some(g) => Box::pin(futures::StreamExt::map(stream, move |ev| {
            let _held = &g;
            ev
        })),
        None => stream,
    }
}

/// Where the daemon publishes ingest's [`nugget_ingest::Foreground`] once ingest started (the
/// router, and so every provider, is built before it).
pub(crate) type ForegroundCell = Arc<OnceLock<Arc<nugget_ingest::Foreground>>>;

/// A configured on-device or LAN provider whose interactive chats, like a [`RuntimeSlot`]'s, hold
/// a foreground guard for the life of their stream. Everything else is forwarded unchanged.
pub(crate) struct ForegroundAware {
    inner: Arc<dyn Provider>,
    foreground: ForegroundCell,
}

#[async_trait]
impl Provider for ForegroundAware {
    fn id(&self) -> &ProviderId {
        self.inner.id()
    }

    fn meta(&self) -> ProviderMeta {
        self.inner.meta()
    }

    fn available(&self) -> bool {
        self.inner.available()
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatStream> {
        let guard = interactive_guard(&self.foreground, &req);
        let stream = self.inner.chat(req).await?;
        Ok(hold_guard(stream, guard))
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.inner.embed(texts).await
    }

    async fn embed_query(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.inner.embed_query(texts).await
    }

    async fn embed_items(&self, texts: &[String], items: &[ItemRef]) -> Result<Vec<Vec<f32>>> {
        self.inner.embed_items(texts, items).await
    }

    async fn embed_probe(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.inner.embed_probe(texts).await
    }

    fn embed_prefixes(&self) -> (String, String) {
        self.inner.embed_prefixes()
    }

    async fn prewarm(&self) -> Result<()> {
        self.inner.prewarm().await
    }

    async fn prewarm_prefix(&self, req: &ChatRequest) -> Result<()> {
        self.inner.prewarm_prefix(req).await
    }
}

/// Wrap the configured providers that are not cloud in [`ForegroundAware`], all sharing `cell`.
/// Cloud providers are returned as they are.
pub(crate) fn guard_configured(
    providers: Vec<Arc<dyn Provider>>,
    cell: &ForegroundCell,
) -> Vec<Arc<dyn Provider>> {
    providers
        .into_iter()
        .map(|p| {
            if p.meta().locality == Locality::Cloud {
                p
            } else {
                Arc::new(ForegroundAware {
                    inner: p,
                    foreground: cell.clone(),
                }) as Arc<dyn Provider>
            }
        })
        .collect()
}

/// A failure to start or reach a role's server, as an error the router falls back on
/// (`Network`, `Provider` or `Unsupported`), whatever the runtime reported.
fn unavailable(role: RuntimeRole, e: Error) -> Error {
    match e {
        e @ (Error::Network(_) | Error::Provider(_)) => e,
        other => Error::Unsupported(format!(
            "local {} runtime unavailable: {other}",
            role_name(role)
        )),
    }
}

fn role_name(role: RuntimeRole) -> &'static str {
    match role {
        RuntimeRole::Chat => "chat",
        RuntimeRole::Embedding => "embedding",
        RuntimeRole::Rerank => "rerank",
        RuntimeRole::Draft => "draft",
    }
}

/// The runtime and its slots.
pub(crate) struct LocalRuntime {
    pub runtime: Runtime,
    pub slots: Vec<Arc<RuntimeSlot>>,
}

impl LocalRuntime {
    /// Slots for the selected models. `installed` lists installed model ids.
    pub fn new(
        runtime: Runtime,
        selected: Vec<(RuntimeRole, ModelEntry)>,
        installed: &dyn Fn(&str) -> bool,
        egress: Arc<dyn EgressSink>,
    ) -> Self {
        let slots = selected
            .into_iter()
            .map(|(role, model)| {
                Arc::new(RuntimeSlot {
                    id: provider_id(role),
                    role,
                    tiers: slot_tiers(role, &model),
                    installed: AtomicBool::new(installed(&model.id)),
                    model: Mutex::new(model),
                    runtime: runtime.clone(),
                    egress: egress.clone(),
                    active: Mutex::new(None),
                    router: OnceLock::new(),
                    on_ready: OnceLock::new(),
                    foreground: OnceLock::new(),
                })
            })
            .collect();
        Self { runtime, slots }
    }

    /// Let slots publish capabilities to `router` (call once it is built). Until a slot's
    /// server is started ([`RuntimeSlot::starting`]) or first ready it is routable for nothing.
    pub fn attach(&self, router: &Arc<Router>) {
        for s in self.routed() {
            let _ = s.router.set(Arc::downgrade(router));
            if s.active
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_none()
            {
                router.set_capabilities(&s.id, Capabilities::default());
            }
        }
    }

    /// Mark interactive chats on `fg` (see `nugget_ingest::Foreground`). Call once.
    pub fn set_foreground(&self, fg: Arc<nugget_ingest::Foreground>) {
        for s in &self.slots {
            let _ = s.foreground.set(fg.clone());
        }
    }

    /// Call `hook` whenever a slot's server becomes ready on a new config (set once).
    pub fn on_ready(&self, hook: ReadyHook) {
        for s in &self.slots {
            let _ = s.on_ready.set(hook.clone());
        }
    }

    pub fn slot(&self, id: &ProviderId) -> Option<&Arc<RuntimeSlot>> {
        self.slots.iter().find(|s| &s.id == id)
    }

    /// The reranker's slot, when `[runtime] rerank_model` selected one.
    pub fn rerank_slot(&self) -> Option<&Arc<RuntimeSlot>> {
        self.slots.iter().find(|s| s.role == RuntimeRole::Rerank)
    }

    /// Slots that are router providers (all but the reranker; the draft slot without tiers).
    fn routed(&self) -> impl Iterator<Item = &Arc<RuntimeSlot>> {
        self.slots.iter().filter(|s| s.role != RuntimeRole::Rerank)
    }

    /// `runtime.status`: [`Runtime::status`], plus selected roles that have not started yet
    /// (`not_installed` until their model is installed, then `stopped`).
    pub fn status(&self) -> Vec<RuntimeInfo> {
        let mut out = self.runtime.status();
        for s in &self.slots {
            if out.iter().any(|i| i.role == s.role) {
                continue;
            }
            out.push(RuntimeInfo {
                model_id: s.model().id,
                role: s.role,
                state: if s.is_installed() {
                    RuntimeState::Stopped
                } else {
                    RuntimeState::NotInstalled
                },
                provider: s.id.clone(),
                error: None,
                state_ms: None,
                expected_ms: None,
                retry_in_ms: None,
                gave_up: false,
                detail: None,
            });
        }
        out
    }

    /// Error to show for a slot in `providers.list`, if any.
    pub fn slot_error(&self, slot: &RuntimeSlot) -> Option<String> {
        self.runtime
            .details(slot.role)
            .filter(|d| d.info.state == RuntimeState::Failed)
            .map(|d| d.info.error.unwrap_or_else(|| "failed to start".into()))
    }
}

/// Providers by id, for the router's construction order.
pub(crate) fn as_providers(local: &LocalRuntime) -> Vec<Arc<dyn Provider>> {
    local
        .routed()
        .map(|s| s.clone() as Arc<dyn Provider>)
        .collect()
}

/// `(id, tiers)` of every slot, for [`tier_map`].
pub(crate) fn slot_routes(local: Option<&LocalRuntime>) -> Vec<(ProviderId, Vec<Tier>)> {
    local
        .map(|l| {
            l.routed()
                .map(|s| (s.id.clone(), s.tiers.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// Locality per built provider id.
pub(crate) fn localities(providers: &[Arc<dyn Provider>]) -> HashMap<ProviderId, Locality> {
    providers
        .iter()
        .map(|p| (p.id().clone(), p.meta().locality))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use nugget_models::GpuKind;
    use std::time::Duration;

    fn pid(s: &str) -> ProviderId {
        ProviderId::new(s)
    }

    fn manifest() -> Manifest {
        nugget_models::default_manifest().unwrap()
    }

    fn hw(gb: u64) -> Hardware {
        Hardware {
            total_ram: gb << 30,
            available_ram: gb << 29,
            cpu_cores: 8,
            gpu: GpuKind::Unknown,
        }
    }

    fn find(m: &Manifest, f: impl Fn(&ModelEntry) -> bool) -> ModelEntry {
        m.models.iter().find(|e| f(e)).unwrap().clone()
    }

    fn reranker(m: &Manifest) -> ModelEntry {
        find(m, |e| e.tiers.contains(&Tier::Rerank))
    }

    fn chat_model(m: &Manifest) -> ModelEntry {
        find(m, |e| {
            e.tiers.iter().any(|t| t.is_chat()) && !e.tiers.contains(&Tier::Rerank)
        })
    }

    fn selected(cfg: &str, m: &Manifest) -> Vec<(RuntimeRole, ModelEntry)> {
        selected_models(&Config::parse(cfg).unwrap(), m, &hw(64))
    }

    fn of_role(sel: &[(RuntimeRole, ModelEntry)], role: RuntimeRole) -> Vec<String> {
        sel.iter()
            .filter(|(r, _)| *r == role)
            .map(|(_, e)| e.id.clone())
            .collect()
    }

    fn provider_cfg(v: serde_json::Value) -> ProviderConfig {
        serde_json::from_value(v).unwrap()
    }

    struct NoEgress;
    impl EgressSink for NoEgress {
        fn record(&self, _event: nugget_core::egress::EgressEvent) {}
    }

    /// A runtime with `roles` selected and installed, attached to its router.
    fn attached(
        tmp: &tempfile::TempDir,
        roles: Vec<(RuntimeRole, ModelEntry)>,
    ) -> (LocalRuntime, Arc<Router>) {
        let paths = nugget_core::paths::Paths::rooted(tmp.path().to_path_buf());
        let egress: Arc<dyn EgressSink> = Arc::new(NoEgress);
        let models = Arc::new(nugget_models::ModelStore::open(&paths, egress.clone()).unwrap());
        let runtime = Runtime::new(&paths, models, egress.clone()).unwrap();
        let local = LocalRuntime::new(runtime, roles, &|_| true, egress);
        let router = Arc::new(Router::new(
            as_providers(&local),
            tier_map(&[], &slot_routes(Some(&local))),
        ));
        local.attach(&router);
        (local, router)
    }

    #[tokio::test(start_paused = true)]
    async fn background_chats_yield_to_interactive_ones() {
        let fg = Arc::new(nugget_ingest::Foreground::new(Duration::from_millis(500)));
        let max = Duration::from_secs(30);
        // An Ask in flight: the judge waits until it ends plus the grace period, however long.
        let ask = fg.begin();
        let fg2 = fg.clone();
        let judge = tokio::spawn(async move {
            yield_to_interactive(&fg2, nugget_proactive::judge::FEATURE_JUDGE, max).await;
            tokio::time::Instant::now()
        });
        tokio::time::sleep(max * 3).await;
        assert!(!judge.is_finished());
        let ended = tokio::time::Instant::now();
        drop(ask);
        assert!(judge.await.unwrap() - ended >= Duration::from_millis(500));
        // Interactive chats and triage never wait; other background chats at most `max`.
        let _ask = fg.begin();
        let t = tokio::time::Instant::now();
        yield_to_interactive(&fg, "ask", max).await;
        yield_to_interactive(&fg, nugget_ingest::FEATURE_TRIAGE, max).await;
        assert_eq!(t.elapsed(), Duration::ZERO);
        yield_to_interactive(&fg, "knowledge", max).await;
        assert_eq!(t.elapsed(), max);

        // A judge stream in flight ends when an Ask starts; other features are left alone.
        drop(_ask);
        fg.idle().await;
        let (tx, rx) = futures::channel::mpsc::unbounded::<Result<nugget_infer::ChatEvent>>();
        let mut judge = preempt_on_foreground(
            Box::pin(rx),
            Some(fg.clone()),
            nugget_proactive::judge::FEATURE_JUDGE,
        );
        let (tx2, rx2) = futures::channel::mpsc::unbounded::<Result<nugget_infer::ChatEvent>>();
        let mut other = preempt_on_foreground(Box::pin(rx2), Some(fg.clone()), "knowledge");
        tx.unbounded_send(Ok(nugget_infer::ChatEvent::TextDelta("{".into())))
            .unwrap();
        assert!(matches!(judge.next().await, Some(Ok(_))));
        let _ask = fg.begin();
        assert!(matches!(judge.next().await, Some(Err(_))));
        assert!(judge.next().await.is_none());
        tx2.unbounded_send(Ok(nugget_infer::ChatEvent::TextDelta("x".into())))
            .unwrap();
        assert!(matches!(other.next().await, Some(Ok(_))));
    }

    #[test]
    fn routing_order_puts_local_first_unprobed_after_the_runtime_and_cloud_last() {
        let configured = vec![
            (pid("cloud"), Tier::ROUTED.to_vec(), Locality::Cloud, true),
            (pid("example"), vec![Tier::Light], Locality::OnDevice, false),
            (pid("probed"), vec![Tier::Light], Locality::OnDevice, true),
            (pid("lan"), vec![Tier::Light], Locality::LocalNetwork, false),
        ];
        let local = vec![
            (pid("local-chat"), vec![Tier::Light]),
            (pid("local-embedding"), vec![Tier::Embedding]),
        ];
        let map = tier_map(&configured, &local);
        let ids = ["probed", "lan", "local-chat", "example", "cloud"].map(pid);
        assert_eq!(map[&Tier::Light], ids);
        assert_eq!(
            map[&Tier::Embedding],
            [pid("local-embedding"), pid("cloud")]
        );
        // Without the runtime, the unprobed provider still serves.
        assert_eq!(
            tier_map(&configured[1..], &[])[&Tier::Light],
            ["probed", "lan", "example"].map(pid)
        );
    }

    #[test]
    fn selection_prefers_config_then_selected_then_recommendation() {
        let m = manifest();
        let chat_rec = recommend(&m, &hw(64), Tier::Triage).unwrap().id.clone();
        let emb_rec = recommend(&m, &hw(64), Tier::Embedding).unwrap().id.clone();
        let sel = selected("", &m);
        assert_eq!(of_role(&sel, RuntimeRole::Chat), [chat_rec]);
        assert_eq!(
            of_role(&sel, RuntimeRole::Embedding),
            std::slice::from_ref(&emb_rec)
        );
        assert_eq!(sel.len(), 2, "no reranker or draft model unless configured");
        // An embedding-only "chat" model is refused; unknown ids select nothing.
        let sel = selected(&format!("[runtime]\nchat_model = \"{emb_rec}\"\n"), &m);
        assert!(of_role(&sel, RuntimeRole::Chat).is_empty());
        let cfg = "[models.selected]\nembedding = \"nope\"\n[runtime]\nchat_model = \"nope\"\n";
        assert!(selected(cfg, &m).is_empty());
    }

    #[test]
    fn rerank_and_draft_models_are_opt_in_and_never_chat_or_embed() {
        let m = manifest();
        let rerank = reranker(&m);
        let chat = chat_model(&m);
        let emb = recommend(&m, &hw(64), Tier::Embedding).unwrap().clone();
        // A reranker is never recommended nor accepted for chat or embeddings.
        for gb in [4, 16, 1024] {
            for t in Tier::ROUTED {
                let rec = recommend(&m, &hw(gb), t).map(|e| e.id.as_str());
                assert_ne!(rec, Some(rerank.id.as_str()));
            }
        }
        for cfg in [
            format!("[runtime]\nchat_model = \"{}\"\n", rerank.id),
            format!("[models.selected]\nembedding = \"{}\"\n", rerank.id),
        ] {
            assert!(
                selected(&cfg, &m).iter().all(|(_, e)| e.id != rerank.id),
                "{cfg}"
            );
        }
        // Configured: only a manifest reranker serves the rerank role.
        let rr = |id: &str| {
            of_role(
                &selected(&format!("[runtime]\nrerank_model = \"{id}\"\n"), &m),
                RuntimeRole::Rerank,
            )
        };
        assert_eq!(rr(&rerank.id), std::slice::from_ref(&rerank.id));
        assert!(rr(&emb.id).is_empty() && rr("nope").is_empty());
        // Draft: only a chat model, beside the usual roles; a [draft] table it refuses selects none.
        let draft = |cfg: String| of_role(&selected(&cfg, &m), RuntimeRole::Draft);
        let cfg = format!("[draft.local]\nmodel = \"{}\"\n", chat.id);
        assert_eq!(draft(cfg.clone()), std::slice::from_ref(&chat.id));
        assert_eq!(selected(&cfg, &m).len(), 3);
        for id in [emb.id.as_str(), rerank.id.as_str(), "nope"] {
            assert!(
                draft(format!("[draft.local]\nmodel = \"{id}\"\n")).is_empty(),
                "{id}"
            );
        }
        assert!(
            draft(format!(
                "[draft.local]\nmodel = \"{}\"\nnope = 1\n",
                chat.id
            ))
            .is_empty()
        );
        // Tiers and capabilities per role.
        assert_eq!(slot_tiers(RuntimeRole::Rerank, &rerank), [Tier::Rerank]);
        assert!(slot_tiers(RuntimeRole::Draft, &chat).is_empty());
        let t = slot_tiers(RuntimeRole::Chat, &chat);
        assert!(CHAT_TIERS.iter().all(|c| t.contains(c)) && t.iter().all(|t| t.is_chat()));
        let cfg = provider_cfg(serde_json::json!({
            "id": "local-draft", "kind": "openai_compatible",
            "base_url": "http://127.0.0.1:9/v1", "model": "m", "context_tokens": 8192,
        }));
        assert_eq!(
            known_capabilities(RuntimeRole::Draft, &cfg),
            known_capabilities(RuntimeRole::Chat, &cfg)
        );
        assert_eq!(
            known_capabilities(RuntimeRole::Rerank, &cfg),
            Capabilities::default()
        );
    }

    /// EM-1: a store with vectors keeps the embedding model that built them.
    #[test]
    fn a_store_with_vectors_keeps_the_embedding_model_that_built_them() {
        let mut m = manifest();
        let old = recommend(&m, &hw(64), Tier::Embedding).unwrap().clone();
        let mut newer = old.clone();
        newer.id = "newer-embedder".into();
        newer.size = old.size * 4;
        m.models.push(newer);
        let embedder = |cfg: &str, index: Option<&str>| {
            let sel = selected_models_for_index(&Config::parse(cfg).unwrap(), &m, &hw(64), index);
            of_role(&sel, RuntimeRole::Embedding).pop()
        };
        assert_eq!(embedder("", None).as_deref(), Some("newer-embedder"));
        assert_eq!(embedder("", Some(&old.id)), Some(old.id.clone()));
        // An index model the manifest lacks (a cloud embedder): the recommendation.
        assert_eq!(
            embedder("", Some("text-embedding-3-small")).as_deref(),
            Some("newer-embedder")
        );
        // Explicit config wins over the index.
        let cfg = "[runtime]\nembedding_model = \"newer-embedder\"\n";
        assert_eq!(
            embedder(cfg, Some(&old.id)).as_deref(),
            Some("newer-embedder")
        );
        // Legacy stores (vectors, no record) are pinned to the original model at start.
        let store = nugget_store::Store::open_in_memory().unwrap();
        assert_eq!(index_embedding_model(&store), None);
        assert_eq!(pin_legacy_embedding_model(&store).unwrap(), None);
        store
            .upsert_item_with_chunks(
                &nugget_store::NewItem::new("src", "a", "h").with_text("a"),
                &[nugget_store::NewChunk::embedded("a", vec![1.0, 0.0])],
            )
            .unwrap();
        assert_eq!(
            index_embedding_model(&store).as_deref(),
            Some(LEGACY_EMBEDDING_MODEL)
        );
        assert_eq!(
            pin_legacy_embedding_model(&store).unwrap().as_deref(),
            Some(LEGACY_EMBEDDING_MODEL)
        );
        assert_eq!(pin_legacy_embedding_model(&store).unwrap(), None);
        assert_eq!(
            store.adopt_embedding_model("newer-embedder").unwrap(),
            LEGACY_EMBEDDING_MODEL
        );
        assert!(m.get(LEGACY_EMBEDDING_MODEL).is_some());
    }

    /// The rerank slot is reported but never a router provider; the draft slot is a router
    /// provider found by id that no tier routes to.
    #[tokio::test]
    async fn rerank_and_draft_slots_stay_out_of_tier_routing() {
        let tmp = tempfile::tempdir().unwrap();
        let m = manifest();
        let chat = chat_model(&m);
        let (local, router) = attached(
            &tmp,
            vec![
                (RuntimeRole::Chat, chat.clone()),
                (RuntimeRole::Draft, chat.clone()),
                (RuntimeRole::Rerank, reranker(&m)),
            ],
        );
        let rerank = local.rerank_slot().unwrap();
        assert!(router.provider(rerank.id()).is_none());
        assert!(router.tiers().get(&Tier::Rerank).is_none());
        let st = local.status();
        assert_eq!(st.len(), 3);
        assert!(st.iter().all(|i| i.state == RuntimeState::Stopped));
        assert!(rerank.ready_config().is_none());
        assert!(matches!(
            rerank.embed(&["x".into()]).await,
            Err(Error::Unsupported(_))
        ));

        let id = provider_id(RuntimeRole::Draft);
        assert!(router.provider(&id).is_some());
        assert!(router.tiers_of(&id).is_empty());
        assert!(router.tiers()[&Tier::Synthesis].contains(&provider_id(RuntimeRole::Chat)));
        local
            .slot(&id)
            .unwrap()
            .activate(provider_cfg(serde_json::json!({
                "id": "local-draft", "kind": "openai_compatible",
                "base_url": "http://127.0.0.1:9/v1", "model": chat.id,
                "api_key": nugget_runtime::api_key_ref(RuntimeRole::Draft).to_string(),
                "locality": "on_device",
            })))
            .unwrap();
        assert!(router.capabilities(&id).unwrap().streaming);
        for tier in Tier::ROUTED {
            let any = Capabilities::default();
            assert!(router.candidates(tier, &any).iter().all(|p| p.id() != &id));
        }
    }

    /// A starting installed chat slot is routable for chat; a failed start is a fallback error
    /// and makes it unroutable again.
    #[tokio::test]
    async fn a_starting_chat_slot_is_routable_for_chat_until_its_start_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let (local, router) = attached(&tmp, vec![(RuntimeRole::Chat, chat_model(&manifest()))]);
        let chat = local.slot(&provider_id(RuntimeRole::Chat)).unwrap();
        let plain = Capabilities {
            streaming: true,
            ..Default::default()
        };
        assert!(router.candidates(Tier::Synthesis, &plain).is_empty());
        chat.starting();
        assert_eq!(router.candidates(Tier::Synthesis, &plain).len(), 1);
        let caps = router.capabilities(chat.id()).unwrap();
        assert!(caps.json_schema && caps.context_tokens.is_none());
        // No runtime build or model file here: the start fails with a fallback error.
        let Err(err) = chat.chat(ChatRequest::default()).await else {
            panic!("no server can start here")
        };
        assert!(
            matches!(
                err,
                Error::Unsupported(_) | Error::Network(_) | Error::Provider(_)
            ),
            "{err:?}"
        );
        chat.start_failed();
        assert!(router.candidates(Tier::Synthesis, &plain).is_empty());
        // Not installed: never routed early.
        chat.set_installed(false);
        chat.starting();
        assert!(router.candidates(Tier::Synthesis, &plain).is_empty());
    }

    /// A configured provider whose chats stream one delta and stay open.
    struct Configured(ProviderId, Locality);

    #[async_trait]
    impl Provider for Configured {
        fn id(&self) -> &ProviderId {
            &self.0
        }
        fn meta(&self) -> ProviderMeta {
            ProviderMeta {
                kind: ProviderKind::OpenaiCompatible,
                model: "m".into(),
                locality: self.1,
                context_tokens: None,
            }
        }
        fn available(&self) -> bool {
            true
        }
        async fn chat(&self, _req: ChatRequest) -> Result<ChatStream> {
            let first =
                futures::stream::iter([Ok(nugget_infer::ChatEvent::TextDelta("hi".into()))]);
            Ok(Box::pin(first.chain(futures::stream::pending())))
        }
        async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(vec![vec![1.0]])
        }
    }

    fn feature_req(feature: &str) -> ChatRequest {
        ChatRequest {
            feature: feature.into(),
            ..ChatRequest::default()
        }
    }

    /// JJ2: interactive chats on configured on-device and LAN providers hold the foreground for
    /// the life of their stream; background chats and cloud providers never do.
    #[tokio::test]
    async fn configured_local_providers_hold_the_foreground_for_interactive_chats() {
        let cell = ForegroundCell::default();
        let wrapped = guard_configured(
            [
                ("ollama", Locality::OnDevice),
                ("lan", Locality::LocalNetwork),
                ("cloud", Locality::Cloud),
            ]
            .into_iter()
            .map(|(id, l)| Arc::new(Configured(pid(id), l)) as Arc<dyn Provider>)
            .collect(),
            &cell,
        );
        // Before ingest publishes its foreground, chats work and hold nothing.
        let mut s = wrapped[0].chat(feature_req("ask")).await.unwrap();
        assert!(s.next().await.is_some());
        drop(s);
        let fg = Arc::new(nugget_ingest::Foreground::new(Duration::from_millis(50)));
        cell.set(fg.clone()).unwrap();
        for p in &wrapped[..2] {
            let mut s = p.chat(feature_req("ask")).await.unwrap();
            assert!(s.next().await.is_some());
            assert_eq!(fg.in_flight(), 1, "held while the stream lives");
            drop(s);
            assert_eq!(fg.in_flight(), 0);
            tokio::time::timeout(Duration::from_secs(10), fg.idle())
                .await
                .unwrap();
            for f in [
                nugget_ingest::FEATURE_TRIAGE,
                nugget_knowledge::llm::FEATURE_KNOWLEDGE,
            ] {
                let _s = p.chat(feature_req(f)).await.unwrap();
                assert!(!fg.busy(), "{f} on {}", p.id());
            }
            assert_eq!(p.embed(&["x".into()]).await.unwrap(), vec![vec![1.0]]);
        }
        let _s = wrapped[2].chat(feature_req("ask")).await.unwrap();
        assert!(!fg.busy());
    }
}
