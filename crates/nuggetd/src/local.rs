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
//!
//! **Routing order**, per tier: configured providers that are not `Cloud` (config order), then
//! the runtime slot, then configured `Cloud` providers. Ingest (triage, embeddings) and
//! retrieval (query embeddings) additionally refuse cloud providers unless allowed in config.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use async_trait::async_trait;
use nugget_core::capability::{Capabilities, Locality, Tier};
use nugget_core::egress::EgressSink;
use nugget_core::ids::ProviderId;
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

/// Tiers the local chat server always serves, whatever else its model lists.
const CHAT_TIERS: [Tier; 3] = [Tier::Triage, Tier::Light, Tier::Synthesis];

/// The manifest model each runtime role should run, per config and hardware.
pub(crate) fn selected_models(
    cfg: &Config,
    manifest: &Manifest,
    hw: &Hardware,
) -> Vec<(RuntimeRole, ModelEntry)> {
    let pick = |explicit: Option<&String>, fallback: Option<&String>, tiers: &[Tier]| {
        explicit.or(fallback).map_or_else(
            || {
                tiers
                    .iter()
                    .find_map(|t| recommend(manifest, hw, *t))
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

/// The routing order described in the module docs. `configured` are the built user providers
/// in config order with their tiers and locality; `local` the runtime slots with their tiers.
pub(crate) fn tier_map(
    configured: &[(ProviderId, Vec<Tier>, Locality)],
    local: &[(ProviderId, Vec<Tier>)],
) -> TierMap {
    let mut map = TierMap::new();
    for tier in Tier::ROUTED {
        let user = |cloud: bool| {
            configured
                .iter()
                .filter(move |(_, tiers, loc)| {
                    tiers.contains(&tier) && (*loc == Locality::Cloud) == cloud
                })
                .map(|(id, _, _)| id.clone())
        };
        let mut ids: Vec<ProviderId> = Vec::new();
        let runtime = local
            .iter()
            .filter(|(_, tiers)| tiers.contains(&tier))
            .map(|(id, _)| id.clone());
        for id in user(false).chain(runtime).chain(user(true)) {
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
    pub fn start_failed(&self) {
        if starting_capabilities(self.role).is_none() {
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

    async fn chat(&self, req: ChatRequest) -> Result<ChatStream> {
        if !matches!(self.role, RuntimeRole::Chat | RuntimeRole::Draft) {
            return Err(Error::Unsupported(format!("{} does not chat", self.id)));
        }
        self.ready().await?.chat(req).await
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
            ),
            (
                pid("ollama"),
                vec![Tier::Synthesis, Tier::Embedding],
                Locality::OnDevice,
            ),
            (pid("lan"), vec![Tier::Triage], Locality::LocalNetwork),
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
}
