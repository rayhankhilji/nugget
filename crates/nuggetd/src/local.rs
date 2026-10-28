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
//! - A slot is routable for nothing (empty capabilities) until its server first became ready,
//!   so neither chat, triage nor embeddings pick it before then. Once ready, the role's known
//!   capabilities are set (chat streams and follows JSON schemas, embedding embeds) and stay
//!   set: after an idle stop the next request starts the server again.
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

const ALL_TIERS: [Tier; 5] = [
    Tier::Triage,
    Tier::Light,
    Tier::Synthesis,
    Tier::Agentic,
    Tier::Embedding,
];

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
        if m.tiers.iter().any(|t| *t != Tier::Embedding) {
            out.push((RuntimeRole::Chat, m));
        } else {
            tracing::warn!(model = %m.id, "runtime.chat_model is an embedding-only model; no local chat");
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
    out
}

/// Tiers a role's slot is routed for.
pub(crate) fn slot_tiers(role: RuntimeRole, model: &ModelEntry) -> Vec<Tier> {
    match role {
        RuntimeRole::Embedding => vec![Tier::Embedding],
        RuntimeRole::Chat => {
            let mut t: Vec<Tier> = CHAT_TIERS.to_vec();
            for x in &model.tiers {
                if *x != Tier::Embedding && !t.contains(x) {
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
        RuntimeRole::Chat => Capabilities {
            streaming: true,
            json_schema: true,
            context_tokens: cfg.context_tokens,
            ..Capabilities::default()
        },
        RuntimeRole::Embedding => Capabilities {
            embeddings: true,
            ..Capabilities::default()
        },
    }
}

/// The routing order described in the module docs. `configured` are the built user providers
/// in config order with their tiers and locality; `local` the runtime slots with their tiers.
pub(crate) fn tier_map(
    configured: &[(ProviderId, Vec<Tier>, Locality)],
    local: &[(ProviderId, Vec<Tier>)],
) -> TierMap {
    let mut map = TierMap::new();
    for tier in ALL_TIERS {
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

    pub fn is_installed(&self) -> bool {
        self.installed.load(Ordering::SeqCst)
    }

    pub fn set_installed(&self, yes: bool) {
        self.installed.store(yes, Ordering::SeqCst);
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
            .map_err(|e| match e {
                e @ (Error::Network(_) | Error::Provider(_)) => e,
                other => Error::Unsupported(format!(
                    "local {} runtime unavailable: {other}",
                    role_name(self.role)
                )),
            })?;
        self.activate(cfg)
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
        let provider = build_provider_with(&cfg, self.egress.clone(), &self.runtime.secrets())?;
        if let Some(router) = self.router.get().and_then(Weak::upgrade) {
            router.set_capabilities(&self.id, known_capabilities(self.role, &cfg));
        }
        tracing::info!(provider = %self.id, model = %self.model.id, "local runtime routed");
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
        if self.role != RuntimeRole::Chat {
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

    /// Start the server if it stopped (Double Tap trigger), then warm the ready provider.
    async fn prewarm(&self) -> Result<()> {
        self.ready().await?.prewarm().await
    }

    async fn prewarm_prefix(&self, req: &ChatRequest) -> Result<()> {
        self.ready().await?.prewarm_prefix(req).await
    }
}

fn role_name(role: RuntimeRole) -> &'static str {
    match role {
        RuntimeRole::Chat => "chat",
        RuntimeRole::Embedding => "embedding",
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
    /// server is first ready it is routable for nothing.
    pub fn attach(&self, router: &Arc<Router>) {
        for s in &self.slots {
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
        .slots
        .iter()
        .map(|s| s.clone() as Arc<dyn Provider>)
        .collect()
}

/// `(id, tiers)` of every slot, for [`tier_map`].
pub(crate) fn slot_routes(local: Option<&LocalRuntime>) -> Vec<(ProviderId, Vec<Tier>)> {
    local
        .map(|l| {
            l.slots
                .iter()
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

    #[test]
    fn chat_slot_serves_the_core_chat_tiers() {
        let m = manifest();
        for e in &m.models {
            if e.tiers.iter().any(|t| *t != Tier::Embedding) {
                let t = slot_tiers(RuntimeRole::Chat, e);
                for c in CHAT_TIERS {
                    assert!(t.contains(&c), "{}: {t:?}", e.id);
                }
                assert!(!t.contains(&Tier::Embedding));
            }
        }
    }
}
