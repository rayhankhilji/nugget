//! The on-device runtime (`nugget-runtime`) in the router, and routing order.
//!
//! The router's provider set is fixed when it is built, but local servers come and go: a model
//! is installed after start, a server stops when idle, or restarts on another port. Each role
//! the runtime may serve therefore gets a [`RuntimeSlot`] in the router from the start, under
//! the runtime's provider id (`local-chat`, `local-embedding`). A slot is a [`Provider`] that
//! forwards to the provider built from the config [`Runtime::ensure`] returns (with
//! `build_provider_with(cfg, egress, &runtime.secrets())`), starting the server on demand.
//!
//! - Until its model is installed a slot fails fast with `Unsupported`, so the router falls
//!   back to the next candidate.
//! - The `rerank` role (`local-rerank`, only with `[runtime] rerank_model` naming a manifest
//!   entry with the `rerank` tier) is a slot too, so it is installed, started and reported like
//!   the others, but it never joins the router: `Tier::Rerank` is not a routed tier and the slot
//!   is not a router provider. Retrieval reaches it through [`crate::rerank`]. Rerankers are
//!   never selected for chat or embeddings.
//! - The `draft` role (`local-draft`, only with `[draft.local] model` naming a manifest chat
//!   model) is the small model local Double Tap drafts with, in its own server. Its slot joins
//!   the router as a provider **without tiers**, so Chat, agents and other features never route
//!   to it; the draft engine asks for it by id ([`nugget_draft::LocalBudget::provider`]). Once
//!   ready it has the chat role's capabilities, and like every slot it starts at boot when its
//!   model is installed, restarts on demand and stops when idle.
//! - A slot starts out routable for nothing (empty capabilities). The chat slot becomes
//!   routable as soon as the daemon starts its installed model's server (at
//!   boot, and after `models.install`; [`RuntimeSlot::starting`]): it then has the chat
//!   capabilities that do not depend on the server's config (streaming, JSON schemas), so a
//!   chat sent while the model still loads (seconds to minutes) routes to it and
//!   [`RuntimeSlot::chat`] waits for the server instead of the router finding no candidate. If
//!   that start fails before the server was ever ready, the slot is routable for nothing again
//!   ([`RuntimeSlot::start_failed`]); a request that picked it meanwhile gets a fallback error
//!   (`Unsupported`, `Network` or `Provider`), so the router tries the next candidate.
//!   Embedding, draft and rerank slots stay routable for nothing until their server first
//!   became ready (ingest re-embeds text-only items on readiness, the draft and rerank slots
//!   are reached by id).
//! - Once ready, the role's known capabilities are set (chat streams and follows JSON schemas
//!   and knows its context size, embedding embeds) and stay set: after an idle stop the next
//!   request starts the server again.
//! - Interactive chats (not triage or other background work) on a slot, and on every configured
//!   `OnDevice` or `LocalNetwork` provider ([`ForegroundAware`]), hold ingest's foreground guard
//!   for the life of their stream, so background triage yields to them. Cloud providers are
//!   not wrapped.
//!
//! **Routing order**, per tier: configured `LocalNetwork` providers and `OnDevice` ones with a
//! stored successful probe (config order), then the runtime slot, then unprobed `OnDevice`
//! providers (an example server that is not running would otherwise cost every request a failed
//! connection; a successful probe moves it ahead from the next start), then configured `Cloud`
//! providers. Ingest (triage, embeddings) and
//! retrieval (query embeddings) additionally refuse cloud providers unless allowed in config.

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
/// Without an explicit embedding model in the config, a store that already has vectors keeps
/// the manifest model that built them, even when the manifest now recommends another: two
/// models' vectors are not comparable (even at the same dimension), so switching would leave
/// every stored vector unusable until the whole store is embedded again. Only a store without
/// vectors (a new install) takes the recommendation.
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
    model: ModelEntry,
    tiers: Vec<Tier>,
    runtime: Runtime,
    egress: Arc<dyn EgressSink>,
    installed: AtomicBool,
    active: Mutex<Option<Active>>,
    router: OnceLock<Weak<Router>>,
    on_ready: OnceLock<ReadyHook>,
    /// Interactive chats hold a guard on it so background triage yields (HH2).
    foreground: OnceLock<Arc<nugget_ingest::Foreground>>,
}

impl RuntimeSlot {
    pub fn model(&self) -> &ModelEntry {
        &self.model
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

    /// Start (or reuse) the server and return the provider in front of it.
    pub async fn ready(&self) -> Result<Arc<dyn Provider>> {
        if !self.is_installed() {
            return Err(Error::Unsupported(format!(
                "local model `{}` is not installed",
                self.model.id
            )));
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

    /// Run [`Runtime::ensure`] even while a server is up: it compares the installed model's
    /// SHA-256 with the running server's and restarts the server on a new model (after
    /// `models.install` updated it). Then route the result.
    pub async fn refresh(&self) -> Result<Arc<dyn Provider>> {
        if !self.is_installed() {
            return Err(Error::Unsupported(format!(
                "local model `{}` is not installed",
                self.model.id
            )));
        }
        let cfg = self
            .runtime
            .ensure(self.role, &self.model)
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
    /// ready before, the slot is routable for nothing again, so requests go straight to the
    /// next candidate instead of retrying the start each time.
    ///
    /// Except when a server was launched and failed (state `failed`, KK1): the runtime then
    /// fails requests at once with one plain sentence while it backs off or after it gave up,
    /// so the slot stays routable. The router still falls back to the next candidate on that
    /// error, and with no other candidate the user sees why (and Status offers Retry) instead
    /// of "no provider has the required capabilities".
    pub fn start_failed(&self) {
        if starting_capabilities(self.role).is_none() {
            return;
        }
        if self.state() == Some(RuntimeState::Failed) {
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
            tracing::info!(provider = %self.id, model = %self.model.id, "local runtime routed");
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

    fn meta(&self) -> ProviderMeta {
        ProviderMeta {
            kind: ProviderKind::OpenaiCompatible,
            model: self.model.id.clone(),
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
        // Interactive chats (Ask, Double Tap, agent, drafts) are marked for their whole stream
        // so background triage leaves the model alone; background work (knowledge, proactive
        // judge, probes, triage itself) is not (HH2).
        // Other background work (knowledge extraction, the proactive judge) waits until no
        // interactive chat is in flight or just ended (knowledge for at most
        // `BACKGROUND_MAX_WAIT`), so it never takes the server's one slot just ahead of an Ask (QA GG2 follow-up: a judge
        // request sent the moment one Ask ended held the next for ~67 s). Triage yields on its
        // own. The wait is bounded so callers' timeouts (which mark the provider as failing)
        // are never reached because of it.
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
            // it is back within a moment, else say plainly what happened (GG6-17).
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
        // KK1: the request stays "in flight" for the watchdog until the stream ends. HH2: the
        // interactive guard is held for the stream's life, and a background judge stream is
        // preempted when an interactive chat starts.
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
        if self.role != RuntimeRole::Embedding {
            return Err(Error::Unsupported(format!("{} does not embed", self.id)));
        }
        self.ready().await?.embed(texts).await
    }

    async fn embed_query(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if self.role != RuntimeRole::Embedding {
            return Err(Error::Unsupported(format!("{} does not embed", self.id)));
        }
        self.ready().await?.embed_query(texts).await
    }

    async fn embed_items(&self, texts: &[String], items: &[ItemRef]) -> Result<Vec<Vec<f32>>> {
        if self.role != RuntimeRole::Embedding {
            return Err(Error::Unsupported(format!("{} does not embed", self.id)));
        }
        self.ready().await?.embed_items(texts, items).await
    }

    async fn embed_probe(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if self.role != RuntimeRole::Embedding {
            return Err(Error::Unsupported(format!("{} does not embed", self.id)));
        }
        self.ready().await?.embed_probe(texts).await
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
            None => nugget_infer::model_embed_prefixes(&self.model.id),
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

/// A provider the user configured in `providers.toml` on this device or the LAN (Ollama, a LAN
/// server): like a [`RuntimeSlot`], its interactive chats hold a foreground guard for the life
/// of their stream, so background triage on the same server yields to Ask and Double Tap (HH2).
/// Everything else is forwarded unchanged.
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
                    model,
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

    /// Call `hook` whenever a slot's server becomes ready on a new config (set once).
    /// Mark interactive chats on `fg` (see `nugget_ingest::Foreground`). Call once.
    pub fn set_foreground(&self, fg: Arc<nugget_ingest::Foreground>) {
        for s in &self.slots {
            let _ = s.foreground.set(fg.clone());
        }
    }

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
                model_id: s.model.id.clone(),
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

    #[tokio::test(start_paused = true)]
    async fn background_chats_wait_for_interactive_ones_but_not_forever() {
        use std::time::Duration;
        let fg = std::sync::Arc::new(nugget_ingest::Foreground::new(Duration::from_millis(500)));
        let max = Duration::from_secs(30);
        // Nothing in flight: no wait.
        let t = tokio::time::Instant::now();
        yield_to_interactive(&fg, "knowledge", max).await;
        assert_eq!(t.elapsed(), Duration::ZERO);
        // An Ask in flight: the judge waits until it ends plus the grace period.
        let ask = fg.begin();
        let fg2 = fg.clone();
        let judge = tokio::spawn(async move {
            yield_to_interactive(&fg2, "proactive_judge", max).await;
            tokio::time::Instant::now()
        });
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(!judge.is_finished());
        let ended = tokio::time::Instant::now();
        drop(ask);
        let went = judge.await.unwrap();
        assert!(went - ended >= Duration::from_millis(500));
        // Interactive chats and triage never wait here.
        let _ask = fg.begin();
        let t = tokio::time::Instant::now();
        yield_to_interactive(&fg, "ask", max).await;
        yield_to_interactive(&fg, nugget_ingest::FEATURE_TRIAGE, max).await;
        assert_eq!(t.elapsed(), Duration::ZERO);
        // A background chat waits at most `max_wait`.
        yield_to_interactive(&fg, "knowledge", max).await;
        assert_eq!(t.elapsed(), max);
    }

    #[tokio::test(start_paused = true)]
    async fn the_judge_waits_out_any_run_of_asks_and_yields_mid_stream() {
        use futures::StreamExt;
        use std::time::Duration;
        let fg = std::sync::Arc::new(nugget_ingest::Foreground::new(Duration::from_millis(500)));
        // Back-to-back Asks for longer than knowledge's cap: the judge still waits.
        let ask = fg.begin();
        let fg2 = fg.clone();
        let judge = tokio::spawn(async move {
            yield_to_interactive(
                &fg2,
                nugget_proactive::judge::FEATURE_JUDGE,
                BACKGROUND_MAX_WAIT,
            )
            .await;
        });
        tokio::time::sleep(BACKGROUND_MAX_WAIT * 3).await;
        assert!(!judge.is_finished());
        drop(ask);
        judge.await.unwrap();

        // A judge stream in flight ends (and drops the connection) when an Ask starts.
        let (tx, rx) = futures::channel::mpsc::unbounded::<Result<nugget_infer::ChatEvent>>();
        let inner: ChatStream = Box::pin(rx);
        let dropped = std::sync::Arc::new(AtomicBool::new(false));
        struct OnDrop(std::sync::Arc<AtomicBool>);
        impl Drop for OnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let flag = OnDrop(dropped.clone());
        let inner: ChatStream = Box::pin(inner.map(move |ev| {
            let _f = &flag;
            ev
        }));
        let mut s = preempt_on_foreground(
            inner,
            Some(fg.clone()),
            nugget_proactive::judge::FEATURE_JUDGE,
        );
        tx.unbounded_send(Ok(nugget_infer::ChatEvent::TextDelta("{".into())))
            .unwrap();
        assert!(matches!(s.next().await, Some(Ok(_))));
        let _ask = fg.begin();
        assert!(matches!(s.next().await, Some(Err(_))));
        assert!(dropped.load(Ordering::SeqCst));
        assert!(s.next().await.is_none());

        // Other features are left alone.
        let (tx, rx) = futures::channel::mpsc::unbounded::<Result<nugget_infer::ChatEvent>>();
        let mut s = preempt_on_foreground(Box::pin(rx), Some(fg.clone()), "knowledge");
        tx.unbounded_send(Ok(nugget_infer::ChatEvent::TextDelta("x".into())))
            .unwrap();
        assert!(matches!(s.next().await, Some(Ok(_))));
    }

    #[test]
    fn background_features_never_hold_the_foreground() {
        // HH2: triage yields to interactive chats only.
        for f in [
            nugget_ingest::FEATURE_TRIAGE,
            nugget_knowledge::llm::FEATURE_KNOWLEDGE,
            nugget_proactive::judge::FEATURE_JUDGE,
            nugget_infer::FEATURE_PROBE,
        ] {
            assert!(!nugget_ingest::is_interactive(f), "{f}");
        }
        for f in ["ask", "chat", "agent", "draft", "query_rewrite"] {
            assert!(nugget_ingest::is_interactive(f), "{f}");
        }
    }
    use super::*;
    use nugget_models::GpuKind;

    fn pid(s: &str) -> ProviderId {
        ProviderId::new(s)
    }

    #[test]
    fn local_providers_come_before_the_runtime_and_cloud_after() {
        let configured = vec![
            (
                pid("cloud"),
                vec![Tier::Triage, Tier::Synthesis, Tier::Embedding],
                Locality::Cloud,
                true,
            ),
            (
                pid("ollama"),
                vec![Tier::Synthesis, Tier::Embedding],
                Locality::OnDevice,
                true,
            ),
            (
                pid("lan"),
                vec![Tier::Triage],
                Locality::LocalNetwork,
                false,
            ),
        ];
        let local = vec![
            (
                pid("local-chat"),
                vec![Tier::Triage, Tier::Light, Tier::Synthesis],
            ),
            (pid("local-embedding"), vec![Tier::Embedding]),
        ];
        let map = tier_map(&configured, &local);
        assert_eq!(
            map[&Tier::Triage],
            vec![pid("lan"), pid("local-chat"), pid("cloud")]
        );
        assert_eq!(map[&Tier::Light], vec![pid("local-chat")]);
        assert_eq!(
            map[&Tier::Synthesis],
            vec![pid("ollama"), pid("local-chat"), pid("cloud")]
        );
        assert_eq!(
            map[&Tier::Embedding],
            vec![pid("ollama"), pid("local-embedding"), pid("cloud")]
        );
        assert!(!map.contains_key(&Tier::Agentic));
    }

    #[test]
    fn unprobed_on_device_providers_come_after_the_runtime() {
        let configured = vec![
            (
                pid("cloud"),
                vec![Tier::Triage, Tier::Light],
                Locality::Cloud,
                false,
            ),
            (
                pid("example"),
                vec![Tier::Triage, Tier::Light, Tier::Synthesis],
                Locality::OnDevice,
                false,
            ),
            (pid("probed"), vec![Tier::Light], Locality::OnDevice, true),
            (pid("lan"), vec![Tier::Light], Locality::LocalNetwork, false),
        ];
        let local = vec![(
            pid("local-chat"),
            vec![Tier::Triage, Tier::Light, Tier::Synthesis],
        )];
        let map = tier_map(&configured, &local);
        assert_eq!(
            map[&Tier::Light],
            vec![
                pid("probed"),
                pid("lan"),
                pid("local-chat"),
                pid("example"),
                pid("cloud")
            ]
        );
        assert_eq!(
            map[&Tier::Triage],
            vec![pid("local-chat"), pid("example"), pid("cloud")]
        );
        assert_eq!(
            map[&Tier::Synthesis],
            vec![pid("local-chat"), pid("example")]
        );
        // Without the runtime, the unprobed provider still serves.
        let map = tier_map(&configured, &[]);
        assert_eq!(map[&Tier::Synthesis], vec![pid("example")]);
    }

    fn manifest() -> Manifest {
        nugget_models::default_manifest().unwrap()
    }

    fn reranker(m: &Manifest) -> ModelEntry {
        m.models
            .iter()
            .find(|e| e.tiers.contains(&Tier::Rerank))
            .expect("the shipped manifest has a reranker")
            .clone()
    }

    #[test]
    fn a_reranker_is_never_selected_for_chat_or_embeddings() {
        let m = manifest();
        let rerank = reranker(&m);
        // Recommendations never pick it, whatever the RAM.
        for gb in [4, 8, 16, 64, 1024] {
            for t in Tier::ROUTED {
                assert_ne!(
                    recommend(&m, &hw(gb), t).map(|e| e.id.as_str()),
                    Some(rerank.id.as_str())
                );
            }
        }
        // Configured explicitly for chat or embeddings (or via models.selected): refused.
        for toml in [
            format!("[runtime]\nchat_model = \"{}\"\n", rerank.id),
            format!("[runtime]\nembedding_model = \"{}\"\n", rerank.id),
            format!(
                "[models.selected]\ntriage = \"{0}\"\nembedding = \"{0}\"\n",
                rerank.id
            ),
        ] {
            let cfg = Config::parse(&toml).unwrap();
            for (role, e) in selected_models(&cfg, &m, &hw(64)) {
                assert_ne!(e.id, rerank.id, "{toml}: {role:?}");
            }
        }
        assert!(
            slot_tiers(RuntimeRole::Chat, &rerank)
                .iter()
                .all(|t| t.is_chat())
        );
    }

    fn hw(gb: u64) -> Hardware {
        Hardware {
            total_ram: gb << 30,
            available_ram: gb << 29,
            cpu_cores: 8,
            gpu: GpuKind::Unknown,
        }
    }

    #[test]
    fn selection_prefers_config_then_selected_then_recommendation() {
        let m = manifest();
        let chat_rec = recommend(&m, &hw(64), Tier::Triage).unwrap().id.clone();
        let emb_rec = recommend(&m, &hw(64), Tier::Embedding).unwrap().id.clone();

        let cfg = Config::parse("").unwrap();
        let sel = selected_models(&cfg, &m, &hw(64));
        assert_eq!(sel.len(), 2);
        assert_eq!(
            (sel[0].0, sel[0].1.id.as_str()),
            (RuntimeRole::Chat, chat_rec.as_str())
        );
        assert_eq!(
            (sel[1].0, sel[1].1.id.as_str()),
            (RuntimeRole::Embedding, emb_rec.as_str())
        );

        // An explicit chat model wins; an embedding-only "chat" model is refused.
        let cfg = Config::parse(&format!("[runtime]\nchat_model = \"{emb_rec}\"\n")).unwrap();
        let sel = selected_models(&cfg, &m, &hw(64));
        assert!(sel.iter().all(|(r, _)| *r == RuntimeRole::Embedding));

        // models.selected is the fallback; unknown ids select nothing.
        let cfg = Config::parse(
            "[models.selected]\nembedding = \"not-in-manifest\"\n[runtime]\nchat_model = \"also-missing\"\n",
        )
        .unwrap();
        assert!(selected_models(&cfg, &m, &hw(64)).is_empty());
    }

    #[test]
    fn a_store_with_vectors_keeps_the_embedding_model_that_built_them() {
        let mut m = manifest();
        let old = recommend(&m, &hw(64), Tier::Embedding).unwrap().clone();
        // A newer, larger embedding model the manifest now recommends.
        let mut newer = old.clone();
        newer.id = "newer-embedder".into();
        newer.size = old.size * 4;
        m.models.push(newer);
        assert_eq!(
            recommend(&m, &hw(64), Tier::Embedding).unwrap().id,
            "newer-embedder"
        );
        let embedder = |cfg: &str, index: Option<&str>| {
            selected_models_for_index(&Config::parse(cfg).unwrap(), &m, &hw(64), index)
                .into_iter()
                .find(|(r, _)| *r == RuntimeRole::Embedding)
                .map(|(_, e)| e.id)
        };
        // New install: the recommendation. A store built with the old model keeps it.
        assert_eq!(embedder("", None).as_deref(), Some("newer-embedder"));
        assert_eq!(embedder("", Some(&old.id)), Some(old.id.clone()));
        // An index model the manifest lacks (a cloud embedder, say): the recommendation.
        assert_eq!(
            embedder("", Some("text-embedding-3-small")).as_deref(),
            Some("newer-embedder")
        );
        // Explicit config wins over the index.
        assert_eq!(
            embedder(
                "[runtime]\nembedding_model = \"newer-embedder\"\n",
                Some(&old.id)
            )
            .as_deref(),
            Some("newer-embedder")
        );
        // The chat model is unaffected by the index.
        let chat = |index| {
            selected_models_for_index(&Config::parse("").unwrap(), &m, &hw(64), index)
                .into_iter()
                .find(|(r, _)| *r == RuntimeRole::Chat)
                .map(|(_, e)| e.id)
        };
        assert_eq!(chat(Some(old.id.as_str())), chat(None));
        // Legacy stores (vectors, no record) count as built with the manifest's original model.
        let store = nugget_store::Store::open_in_memory().unwrap();
        assert_eq!(index_embedding_model(&store), None);
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
        // Pinned at start (EM-1): a later embedder of the same dimension cannot claim it.
        assert_eq!(
            pin_legacy_embedding_model(&store).unwrap().as_deref(),
            Some(LEGACY_EMBEDDING_MODEL)
        );
        assert_eq!(pin_legacy_embedding_model(&store).unwrap(), None);
        assert_eq!(
            store.adopt_embedding_model("newer-embedder").unwrap(),
            LEGACY_EMBEDDING_MODEL
        );
        assert_eq!(
            store.embedding_model().unwrap().as_deref(),
            Some(LEGACY_EMBEDDING_MODEL)
        );
        // A store without vectors stays open, and an existing record is kept.
        let fresh = nugget_store::Store::open_in_memory().unwrap();
        assert_eq!(pin_legacy_embedding_model(&fresh).unwrap(), None);
        assert_eq!(fresh.embedding_model().unwrap(), None);
        let recorded = nugget_store::Store::open_in_memory().unwrap();
        recorded
            .upsert_item_with_chunks(
                &nugget_store::NewItem::new("src", "a", "h").with_text("a"),
                &[nugget_store::NewChunk::embedded("a", vec![1.0, 0.0])],
            )
            .unwrap();
        recorded.adopt_embedding_model("recorded").unwrap();
        assert_eq!(pin_legacy_embedding_model(&recorded).unwrap(), None);
        assert_eq!(
            index_embedding_model(&recorded).as_deref(),
            Some("recorded")
        );
        assert!(
            m.get(LEGACY_EMBEDDING_MODEL).is_some(),
            "legacy model left the manifest"
        );
    }

    #[test]
    fn a_reranker_is_selected_only_when_configured_and_never_routed() {
        let m = manifest();
        // No automatic recommendation.
        let sel = selected_models(&Config::parse("").unwrap(), &m, &hw(64));
        assert!(sel.iter().all(|(r, _)| *r != RuntimeRole::Rerank));
        // Configured: the manifest's reranker.
        let rerank = reranker(&m);
        let cfg = Config::parse(&format!("[runtime]\nrerank_model = \"{}\"\n", rerank.id)).unwrap();
        let sel = selected_models(&cfg, &m, &hw(64));
        let rr: Vec<_> = sel
            .iter()
            .filter(|(r, _)| *r == RuntimeRole::Rerank)
            .collect();
        assert_eq!(rr.len(), 1);
        assert_eq!(rr[0].1.id, rerank.id);
        // A model without the rerank tier is refused for the role.
        let emb = recommend(&m, &hw(64), Tier::Embedding).unwrap().clone();
        let cfg = Config::parse(&format!("[runtime]\nrerank_model = \"{}\"\n", emb.id)).unwrap();
        assert!(
            selected_models(&cfg, &m, &hw(64))
                .iter()
                .all(|(r, _)| *r != RuntimeRole::Rerank)
        );
        // Not in the manifest: no reranker.
        let cfg = Config::parse("[runtime]\nrerank_model = \"not-in-manifest\"\n").unwrap();
        assert!(
            selected_models(&cfg, &m, &hw(64))
                .iter()
                .all(|(r, _)| *r != RuntimeRole::Rerank)
        );
        assert_eq!(slot_tiers(RuntimeRole::Rerank, &rerank), vec![Tier::Rerank]);
        assert_eq!(
            known_capabilities(
                RuntimeRole::Rerank,
                &serde_json::from_value(serde_json::json!({
                    "id": "local-rerank", "kind": "openai_compatible",
                    "base_url": "http://127.0.0.1:9/v1", "model": "m",
                }))
                .unwrap()
            ),
            Capabilities::default()
        );
    }

    /// The rerank slot is a slot (status, installs) but not a router provider, for any tier.
    #[tokio::test]
    async fn the_rerank_slot_stays_out_of_the_router() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = nugget_core::paths::Paths::rooted(tmp.path().to_path_buf());
        let egress: Arc<dyn EgressSink> = Arc::new(NoEgress);
        let models = Arc::new(nugget_models::ModelStore::open(&paths, egress.clone()).unwrap());
        let runtime = Runtime::new(&paths, models, egress.clone()).unwrap();
        let m = manifest();
        let emb = recommend(&m, &hw(64), Tier::Embedding).unwrap().clone();
        let local = LocalRuntime::new(
            runtime,
            vec![
                (RuntimeRole::Embedding, emb.clone()),
                (RuntimeRole::Rerank, reranker(&m)),
            ],
            &|_| true,
            egress,
        );
        let rerank = local.rerank_slot().expect("a rerank slot");
        assert_eq!(rerank.id().as_str(), nugget_runtime::RERANK_PROVIDER_ID);
        let providers = as_providers(&local);
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id().as_str(), EMBEDDING_ID);
        let routes = slot_routes(Some(&local));
        assert_eq!(routes.len(), 1);
        let router = Arc::new(Router::new(providers, tier_map(&[], &routes)));
        local.attach(&router);
        assert!(router.tiers().get(&Tier::Rerank).is_none());
        for t in Tier::ALL {
            assert!(!router.tiers_of(rerank.id()).contains(&t));
        }
        assert!(router.provider(rerank.id()).is_none());
        assert!(router.capabilities(rerank.id()).is_none());
        // Reported like the other roles; never started, so not ready.
        let st = local.status();
        let r = st.iter().find(|i| i.role == RuntimeRole::Rerank).unwrap();
        assert_eq!(r.state, RuntimeState::Stopped);
        assert!(rerank.ready_config().is_none());
        assert_eq!(rerank.state(), None);
        // It neither chats nor embeds.
        assert!(matches!(
            rerank.embed(&["x".into()]).await,
            Err(Error::Unsupported(_))
        ));
    }

    fn chat_model(m: &Manifest) -> ModelEntry {
        m.models
            .iter()
            .find(|e| e.tiers.iter().any(|t| t.is_chat()) && !e.tiers.contains(&Tier::Rerank))
            .expect("the shipped manifest has a chat model")
            .clone()
    }

    fn draft_slots(cfg: &str, m: &Manifest) -> Vec<ModelEntry> {
        let cfg = Config::parse(cfg).unwrap();
        selected_models(&cfg, m, &hw(64))
            .into_iter()
            .filter(|(r, _)| *r == RuntimeRole::Draft)
            .map(|(_, e)| e)
            .collect()
    }

    #[test]
    fn a_draft_model_is_selected_only_when_configured_and_a_chat_model() {
        let m = manifest();
        // Not configured: no draft role, and the other roles are unchanged.
        let none = selected_models(&Config::parse("").unwrap(), &m, &hw(64));
        assert!(none.iter().all(|(r, _)| *r != RuntimeRole::Draft));
        let chat = chat_model(&m);
        let cfg = format!("[draft.local]\nmodel = \"{}\"\n", chat.id);
        let sel = draft_slots(&cfg, &m);
        assert_eq!(sel.len(), 1);
        assert_eq!(sel[0].id, chat.id);
        // Beside the usual roles, not instead of them.
        let all = selected_models(&Config::parse(&cfg).unwrap(), &m, &hw(64));
        assert_eq!(all.len(), none.len() + 1);
        assert_eq!(&all[..none.len()], &none[..]);
        // An embedding model or a reranker cannot draft; unknown ids select nothing.
        let emb = recommend(&m, &hw(64), Tier::Embedding).unwrap().clone();
        for id in [emb.id.as_str(), reranker(&m).id.as_str(), "not-in-manifest"] {
            let cfg = format!("[draft.local]\nmodel = \"{id}\"\n");
            assert!(draft_slots(&cfg, &m).is_empty(), "{id}");
        }
        // A [draft] table the feature refuses selects nothing either.
        let cfg = format!("[draft.local]\nmodel = \"{}\"\nnope = 1\n", chat.id);
        assert!(draft_slots(&cfg, &m).is_empty());
        // Never routed for a tier; chat's capabilities once ready.
        assert!(slot_tiers(RuntimeRole::Draft, &chat).is_empty());
        let cfg: ProviderConfig = serde_json::from_value(serde_json::json!({
            "id": "local-draft", "kind": "openai_compatible",
            "base_url": "http://127.0.0.1:9/v1", "model": "m", "context_tokens": 8192,
        }))
        .unwrap();
        assert_eq!(
            known_capabilities(RuntimeRole::Draft, &cfg),
            known_capabilities(RuntimeRole::Chat, &cfg)
        );
    }

    /// The draft slot is a router provider (found by id) that no tier routes to.
    #[tokio::test]
    async fn the_draft_slot_is_a_router_provider_without_tiers() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = nugget_core::paths::Paths::rooted(tmp.path().to_path_buf());
        let egress: Arc<dyn EgressSink> = Arc::new(NoEgress);
        let models = Arc::new(nugget_models::ModelStore::open(&paths, egress.clone()).unwrap());
        let runtime = Runtime::new(&paths, models, egress.clone()).unwrap();
        let m = manifest();
        let chat = chat_model(&m);
        let local = LocalRuntime::new(
            runtime,
            vec![
                (RuntimeRole::Chat, chat.clone()),
                (RuntimeRole::Draft, chat.clone()),
            ],
            &|_| true,
            egress,
        );
        let providers = as_providers(&local);
        assert_eq!(providers.len(), 2);
        let routes = slot_routes(Some(&local));
        let router = Arc::new(Router::new(providers, tier_map(&[], &routes)));
        local.attach(&router);
        let id = nugget_runtime::provider_id(RuntimeRole::Draft);
        assert_eq!(id.as_str(), nugget_runtime::DRAFT_PROVIDER_ID);
        let found = router.provider(&id).expect("found by id");
        assert_eq!(found.id(), &id);
        assert_eq!(found.meta().locality, Locality::OnDevice);
        assert!(router.tiers_of(&id).is_empty());
        for (tier, ids) in router.tiers() {
            assert!(!ids.contains(&id), "{tier:?}");
        }
        // Chat keeps its tiers.
        assert!(router.tiers()[&Tier::Synthesis].contains(&provider_id(RuntimeRole::Chat)));
        // Unroutable until ready, then chat's capabilities.
        assert_eq!(router.capabilities(&id), Some(Capabilities::default()));
        let slot = local.slot(&id).unwrap();
        let cfg: ProviderConfig = serde_json::from_value(serde_json::json!({
            "id": "local-draft", "kind": "openai_compatible",
            "base_url": "http://127.0.0.1:9/v1", "model": chat.id,
            "api_key": nugget_runtime::api_key_ref(RuntimeRole::Draft).to_string(),
            "locality": "on_device",
        }))
        .unwrap();
        slot.activate(cfg).unwrap();
        assert!(router.capabilities(&id).unwrap().streaming);
        // Still no tier routes to it.
        for tier in Tier::ROUTED {
            let plain = Capabilities::default();
            assert!(
                router
                    .candidates(tier, &plain)
                    .iter()
                    .all(|p| p.id() != &id),
                "{tier:?}"
            );
        }
        // It drafts (chats), never embeds; reported in status like the other roles.
        assert!(matches!(
            slot.embed(&["x".into()]).await,
            Err(Error::Unsupported(_))
        ));
        let st = local.status();
        let d = st.iter().find(|i| i.role == RuntimeRole::Draft).unwrap();
        assert_eq!(d.provider, id);
        assert_eq!(d.state, RuntimeState::Stopped);
    }

    struct NoEgress;
    impl EgressSink for NoEgress {
        fn record(&self, _event: nugget_core::egress::EgressEvent) {}
    }

    /// The router is built once; a slot joins it unroutable and becomes routable (and rebuilt
    /// only when its server's config changes) once its server is ready.
    #[tokio::test]
    async fn a_slot_becomes_routable_once_its_server_is_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = nugget_core::paths::Paths::rooted(tmp.path().to_path_buf());
        let egress: Arc<dyn EgressSink> = Arc::new(NoEgress);
        let models = Arc::new(nugget_models::ModelStore::open(&paths, egress.clone()).unwrap());
        let runtime = Runtime::new(&paths, models, egress.clone()).unwrap();
        let m = manifest();
        let emb = m
            .models
            .iter()
            .find(|e| e.tiers.contains(&Tier::Embedding))
            .unwrap()
            .clone();
        let local = LocalRuntime::new(
            runtime,
            vec![(RuntimeRole::Embedding, emb)],
            &|_| false,
            egress,
        );
        let router = Arc::new(Router::new(
            as_providers(&local),
            tier_map(&[], &slot_routes(Some(&local))),
        ));
        local.attach(&router);
        let need = Capabilities {
            embeddings: true,
            ..Default::default()
        };
        let plain = Capabilities {
            streaming: true,
            ..Default::default()
        };
        assert!(router.candidates(Tier::Embedding, &need).is_empty());
        assert!(router.candidates(Tier::Embedding, &plain).is_empty());
        let slot = &local.slots[0];
        let err = slot.embed(&["x".into()]).await.unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "{err:?}");

        let cfg: ProviderConfig = serde_json::from_value(serde_json::json!({
            "id": EMBEDDING_ID,
            "kind": "openai_compatible",
            "base_url": "http://127.0.0.1:9/v1",
            "model": slot.model().id,
            "api_key": nugget_runtime::api_key_ref(RuntimeRole::Embedding).to_string(),
            "tiers": ["embedding"],
            "locality": "on_device",
        }))
        .unwrap();
        let a = slot.activate(cfg.clone()).unwrap();
        let routed = router.candidates(Tier::Embedding, &need);
        assert_eq!(routed.len(), 1);
        assert_eq!(routed[0].id().as_str(), EMBEDDING_ID);
        assert_eq!(routed[0].meta().locality, Locality::OnDevice);
        let b = slot.activate(cfg.clone()).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "same config: provider reused");
        let mut moved = cfg;
        moved.base_url = Some("http://127.0.0.1:10/v1".into());
        let c = slot.activate(moved).unwrap();
        assert!(!Arc::ptr_eq(&a, &c), "new port: provider rebuilt");
    }

    const EMBEDDING_ID: &str = nugget_runtime::EMBEDDING_PROVIDER_ID;

    /// Once its server is being started, an installed chat slot is routable for plain chat
    /// (the request waits for the server); embeddings still wait for readiness. A start that
    /// fails is a fallback error and makes the slot unroutable again.
    #[tokio::test]
    async fn a_starting_chat_slot_is_routable_for_chat_until_its_start_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = nugget_core::paths::Paths::rooted(tmp.path().to_path_buf());
        let egress: Arc<dyn EgressSink> = Arc::new(NoEgress);
        let models = Arc::new(nugget_models::ModelStore::open(&paths, egress.clone()).unwrap());
        let runtime = Runtime::new(&paths, models, egress.clone()).unwrap();
        let m = manifest();
        let emb = recommend(&m, &hw(64), Tier::Embedding).unwrap().clone();
        let local = LocalRuntime::new(
            runtime,
            vec![
                (RuntimeRole::Chat, chat_model(&m)),
                (RuntimeRole::Embedding, emb),
            ],
            &|_| true,
            egress,
        );
        let router = Arc::new(Router::new(
            as_providers(&local),
            tier_map(&[], &slot_routes(Some(&local))),
        ));
        local.attach(&router);
        let chat = local.slot(&provider_id(RuntimeRole::Chat)).unwrap();
        let embedding = local.slot(&pid(EMBEDDING_ID)).unwrap();
        let plain = Capabilities {
            streaming: true,
            ..Default::default()
        };
        let need = Capabilities {
            embeddings: true,
            ..Default::default()
        };
        assert!(router.candidates(Tier::Synthesis, &plain).is_empty());

        chat.starting();
        embedding.starting();
        let routed = router.candidates(Tier::Synthesis, &plain);
        assert_eq!(routed.len(), 1);
        assert_eq!(routed[0].id(), chat.id());
        let caps = router.capabilities(chat.id()).unwrap();
        assert!(caps.json_schema);
        assert_eq!(caps.context_tokens, None);
        assert!(router.candidates(Tier::Embedding, &need).is_empty());

        // No runtime build or model file here: the start fails with a fallback error.
        let err = match chat.chat(ChatRequest::default()).await {
            Err(e) => e,
            Ok(_) => panic!("no server can start here"),
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

    #[test]
    fn chat_slot_serves_the_core_chat_tiers() {
        let m = manifest();
        for e in &m.models {
            if e.tiers.iter().any(|t| t.is_chat()) {
                let t = slot_tiers(RuntimeRole::Chat, e);
                for c in CHAT_TIERS {
                    assert!(t.contains(&c), "{}: {t:?}", e.id);
                }
                assert!(t.iter().all(|t| t.is_chat()), "{}: {t:?}", e.id);
            }
        }
    }

    /// A configured provider for the foreground tests: chats stream one delta and stay open.
    struct Configured {
        id: ProviderId,
        locality: Locality,
    }

    #[async_trait]
    impl Provider for Configured {
        fn id(&self) -> &ProviderId {
            &self.id
        }
        fn meta(&self) -> ProviderMeta {
            ProviderMeta {
                kind: ProviderKind::OpenaiCompatible,
                model: "m".into(),
                locality: self.locality,
                context_tokens: Some(7),
            }
        }
        fn available(&self) -> bool {
            false
        }
        async fn chat(&self, _req: ChatRequest) -> Result<ChatStream> {
            let first =
                futures::stream::iter([Ok(nugget_infer::ChatEvent::TextDelta("hi".into()))]);
            Ok(Box::pin(futures::StreamExt::chain(
                first,
                futures::stream::pending(),
            )))
        }
        async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(vec![vec![1.0]])
        }
        async fn embed_query(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(vec![vec![2.0]])
        }
        async fn embed_items(&self, _t: &[String], _i: &[ItemRef]) -> Result<Vec<Vec<f32>>> {
            Ok(vec![vec![3.0]])
        }
        fn embed_prefixes(&self) -> (String, String) {
            ("q: ".into(), "d: ".into())
        }
        async fn prewarm(&self) -> Result<()> {
            Err(Error::Other("prewarm".into()))
        }
        async fn prewarm_prefix(&self, _req: &ChatRequest) -> Result<()> {
            Err(Error::Other("prewarm_prefix".into()))
        }
    }

    fn configured(id: &str, locality: Locality) -> Arc<dyn Provider> {
        Arc::new(Configured {
            id: pid(id),
            locality,
        })
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
            vec![
                configured("ollama", Locality::OnDevice),
                configured("lan", Locality::LocalNetwork),
                configured("cloud", Locality::Cloud),
            ],
            &cell,
        );
        // Before ingest publishes its foreground, chats work and hold nothing.
        let mut s = wrapped[0].chat(feature_req("ask")).await.unwrap();
        assert!(futures::StreamExt::next(&mut s).await.is_some());
        drop(s);
        let fg = Arc::new(nugget_ingest::Foreground::new(
            std::time::Duration::from_millis(50),
        ));
        cell.set(fg.clone()).unwrap();

        for p in &wrapped[..2] {
            let mut s = p.chat(feature_req("ask")).await.unwrap();
            assert!(fg.busy(), "{}", p.id());
            assert_eq!(fg.in_flight(), 1);
            assert!(futures::StreamExt::next(&mut s).await.is_some());
            assert_eq!(fg.in_flight(), 1, "held while the stream lives");
            drop(s);
            assert_eq!(fg.in_flight(), 0);
            assert!(fg.busy(), "grace after the stream ends");
            tokio::time::timeout(std::time::Duration::from_secs(10), fg.idle())
                .await
                .expect("idle after the grace period");
            assert!(!fg.busy());

            for f in [
                nugget_ingest::FEATURE_TRIAGE,
                nugget_knowledge::llm::FEATURE_KNOWLEDGE,
            ] {
                let s = p.chat(feature_req(f)).await.unwrap();
                assert!(!fg.busy(), "{f} on {}", p.id());
                drop(s);
            }
        }

        // The cloud provider is not wrapped: an interactive chat holds nothing.
        let s = wrapped[2].chat(feature_req("ask")).await.unwrap();
        assert_eq!(fg.in_flight(), 0);
        assert!(!fg.busy());
        drop(s);
    }

    /// The wrapper forwards every other `Provider` method, overrides included.
    #[tokio::test]
    async fn the_foreground_wrapper_forwards_everything_else() {
        let cell = ForegroundCell::default();
        let p = guard_configured(vec![configured("ollama", Locality::OnDevice)], &cell)
            .pop()
            .unwrap();
        assert_eq!(p.id(), &pid("ollama"));
        let m = p.meta();
        assert_eq!(m.locality, Locality::OnDevice);
        assert_eq!(m.context_tokens, Some(7));
        assert!(!p.available());
        let t = ["x".to_string()];
        assert_eq!(p.embed(&t).await.unwrap(), vec![vec![1.0]]);
        assert_eq!(p.embed_query(&t).await.unwrap(), vec![vec![2.0]]);
        assert_eq!(p.embed_items(&t, &[]).await.unwrap(), vec![vec![3.0]]);
        assert_eq!(p.embed_prefixes(), ("q: ".into(), "d: ".into()));
        assert!(matches!(p.prewarm().await, Err(Error::Other(m)) if m == "prewarm"));
        assert!(
            matches!(p.prewarm_prefix(&feature_req("ask")).await, Err(Error::Other(m)) if m == "prewarm_prefix")
        );
    }
}
