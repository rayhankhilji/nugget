//! Daemon lifecycle: start everything, serve IPC until shutdown, then wind down in order.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, RwLock};
use std::time::{Duration, Instant};

use nugget_connector::{Registry, SecretStore};
use nugget_core::capability::Locality;
use nugget_core::egress::EgressSink;
use nugget_core::ids::ProviderId;
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use nugget_infer::{Provider, ProviderConfig, Router, build_provider};
use nugget_ingest::{Ingest, KeyringSecrets, MemorySecrets, PowerPolicy, SystemPowerPolicy};
use nugget_ipc::Listener;
use nugget_models::{
    DownloadOptions, Hardware, Manifest, ModelStore, RuntimeAccel, default_manifest_checked,
};
use nugget_retrieve::Retriever;
use nugget_runtime::{Runtime, RuntimeOptions};
use nugget_store::{Store, StoreKey};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::config::{Config, tier_name};
use crate::handler::RpcHandler;
use crate::hub::Hub;
use crate::knowledge::KnowledgeService;
use crate::local::{self, LocalRuntime, RuntimeSlot};
use crate::mcp::{DaemonBackend, McpService};
use crate::probes::ProbeCache;
use crate::sources;

const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// How long shutdown waits for an interrupted knowledge run.
const KNOWLEDGE_GRACE: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub struct DaemonOptions {
    pub paths: Paths,
    /// Use this store key instead of [`StoreKey::load_or_create`] (tests).
    pub store_key: Option<StoreKey>,
    /// Where source credentials live.
    pub source_secrets: Option<Arc<dyn SecretStore>>,
    pub runtime: RuntimeOverrides,
    /// Power policy for ingest and knowledge extraction.
    pub power: Option<Arc<dyn PowerPolicy>>,
    /// Vault directory when `[vault] path` is unset.
    pub default_vault_dir: Option<PathBuf>,
    /// The data root given with `--home` / `NUGGET_HOME`, passed on by MCP setup snippets.
    pub home: Option<PathBuf>,
    /// The `nuggetd` executable MCP setup snippets launch. Default: this executable.
    pub nuggetd_exe: Option<PathBuf>,
    /// Connector registry override (tests). Default: `sources::registry()`.
    pub connectors: Option<Registry>,
}

/// Test hooks for the local runtime and model manifest.
#[derive(Clone, Default)]
pub struct RuntimeOverrides {
    /// Run this `llama-server` executable instead of downloading a manifest runtime build.
    pub server_binary: Option<(PathBuf, RuntimeAccel)>,
    pub options: Option<RuntimeOptions>,
    /// Use this (already verified) manifest instead of the bundled one, for models too.
    pub manifest: Option<Manifest>,
    pub hardware: Option<Hardware>,
}

impl DaemonOptions {
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            store_key: None,
            source_secrets: None,
            runtime: RuntimeOverrides::default(),
            power: None,
            default_vault_dir: None,
            home: None,
            nuggetd_exe: None,
            connectors: None,
        }
    }
}

pub enum Startup {
    Ready(Box<Daemon>),
    /// Another daemon already answers on this socket.
    AlreadyRunning {
        socket: String,
    },
}

/// A started daemon; nothing is served until [`Daemon::run_until`].
pub struct Daemon {
    state: Arc<State>,
    listener: Listener,
    config_created: bool,
}

pub(crate) struct State {
    pub paths: Paths,
    pub started: Instant,
    pub store: Store,
    pub router: Arc<Router>,
    pub local: Option<Arc<LocalRuntime>>,
    pub ingest: Ingest,
    pub retriever: Retriever,
    pub knowledge: Arc<KnowledgeService>,
    pub mcp: Option<Arc<McpService>>,
    pub secrets: Arc<dyn SecretStore>,
    pub hub: Arc<Hub>,
    /// Every configured provider, in config order (including ones that failed to build).
    pub configs: RwLock<Vec<ProviderConfig>>,
    /// Providers that could not be built, with the reason. They are not in the router.
    pub unavailable: RwLock<HashMap<ProviderId, String>>,
    /// Result of verifying the audit hash chain; unset until the startup check finishes.
    pub audit_chain_ok: Arc<OnceLock<bool>>,
    pub probes: Arc<Mutex<ProbeCache>>,
    pub manifest: std::result::Result<Manifest, String>,
    pub models: Arc<ModelStore>,
    pub hardware: Hardware,
    pub streams: Mutex<HashMap<String, CancellationToken>>,
    /// The sources of recent Ask answers, for follow-ups' history (`crate::ask::AnswerMemory`).
    pub answers: Mutex<crate::ask::AnswerMemory>,
    pub installs: Arc<InFlight>,
    pub tasks: TaskTracker,
    pub shutdown: CancellationToken,
    /// Wired-in subsystems (`crate::features`); set once, right after `State` is built.
    pub features: OnceLock<crate::features::Features>,
}

impl State {
    /// Config of a provider that was built (and so is in the router).
    pub fn config_of(&self, id: &ProviderId) -> Option<ProviderConfig> {
        if self.unavailable_reason(id).is_some() {
            return None;
        }
        self.provider_configs().into_iter().find(|c| &c.id == id)
    }

    pub fn provider_configs(&self) -> Vec<ProviderConfig> {
        self.configs
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn unavailable_reason(&self, id: &ProviderId) -> Option<String> {
        self.unavailable
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    /// Route `cfg` from now on without a restart: added after the other providers, or replacing the
    /// one with its id (for example with a new key).
    pub fn set_provider(
        &self,
        cfg: ProviderConfig,
        built: std::result::Result<Arc<dyn Provider>, String>,
    ) {
        let id = cfg.id.clone();
        {
            let mut unavailable = self
                .unavailable
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            match built {
                Ok(p) => {
                    unavailable.remove(&id);
                    // Placed as at startup (`local::tier_map`).
                    let rank = |loc: Locality, probed: bool| match loc {
                        Locality::Cloud => 3,
                        Locality::OnDevice if !probed => 2,
                        _ => 0,
                    };
                    let mine = rank(p.meta().locality, false);
                    let before = |other: &ProviderId| {
                        let theirs = if self.slot(other).is_some() {
                            1
                        } else {
                            self.router.provider(other).map_or(3, |o| {
                                rank(o.meta().locality, self.router.capabilities(other).is_some())
                            })
                        };
                        theirs > mine
                    };
                    self.router.set_provider_before(p, &cfg.tiers, &before);
                }
                Err(reason) => {
                    self.router.remove_provider(&id);
                    unavailable.insert(id.clone(), reason);
                }
            }
        }
        let mut configs = self.configs.write().unwrap_or_else(PoisonError::into_inner);
        match configs.iter_mut().find(|c| c.id == id) {
            Some(c) => *c = cfg,
            None => configs.push(cfg),
        }
    }

    /// Stop routing to provider `id` and forget its config. Returns whether it was configured.
    pub fn remove_provider(&self, id: &ProviderId) -> bool {
        self.router.remove_provider(id);
        self.unavailable
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        let mut configs = self.configs.write().unwrap_or_else(PoisonError::into_inner);
        let before = configs.len();
        configs.retain(|c| &c.id != id);
        configs.len() != before
    }

    pub fn streams(&self) -> MutexGuard<'_, HashMap<String, CancellationToken>> {
        self.streams.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn slot(&self, id: &ProviderId) -> Option<&Arc<RuntimeSlot>> {
        self.local.as_ref().and_then(|l| l.slot(id))
    }
}

/// Start `slot`'s server in the background (tracked; stops on shutdown).
pub(crate) fn spawn_ensure(state: &Arc<State>, slot: Arc<RuntimeSlot>) {
    spawn_start(state, slot, false);
}

/// Like [`spawn_ensure`], but restarts a running server on a newly installed model.
pub(crate) fn spawn_refresh(state: &Arc<State>, slot: Arc<RuntimeSlot>) {
    spawn_start(state, slot, true);
}

fn spawn_start(state: &Arc<State>, slot: Arc<RuntimeSlot>, refresh: bool) {
    let stop = state.shutdown.clone();
    let knowledge = state.knowledge.clone();
    // Route chat to the slot while its model loads (the request waits for the server).
    slot.starting();
    state.tasks.spawn(async move {
        let model = slot.model().id.clone();
        let start = async {
            if refresh {
                slot.refresh().await
            } else {
                slot.ready().await
            }
        };
        tokio::select! {
            () = stop.cancelled() => {}
            r = start => match r {
                Ok(_) => {
                    tracing::info!(provider = %slot.id(), %model, "local runtime ready");
                    // Items waiting for a model are retried now, not at the periodic run.
                    knowledge.wake();
                }
                Err(e) => {
                    slot.start_failed();
                    tracing::warn!(provider = %slot.id(), %model, error = %e, "local runtime did not start");
                }
            },
        }
    });
}

/// A set of ids with RAII membership: at most one holder per id.
#[derive(Debug, Default)]
pub struct InFlight(Mutex<HashSet<String>>);

impl InFlight {
    pub fn begin(self: &Arc<Self>, id: &str) -> Option<InFlightGuard> {
        let mut set = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        set.insert(id.to_string()).then(|| InFlightGuard {
            set: Arc::clone(self),
            id: id.to_string(),
        })
    }
}

#[derive(Debug)]
pub struct InFlightGuard {
    set: Arc<InFlight>,
    id: String,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.set
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
    }
}

impl Daemon {
    /// Bind the socket, then load config, open the store and build providers.
    pub async fn start(opts: DaemonOptions) -> Result<Startup> {
        let paths = opts.paths;
        let socket = nugget_ipc::socket_display(&paths);

        // Bind first so two daemons can never open the same store.
        if nugget_ipc::connect_stream(&paths).await.is_ok() {
            return Ok(Startup::AlreadyRunning { socket });
        }
        let listener = match nugget_ipc::bind(&paths).await {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                return Ok(Startup::AlreadyRunning { socket });
            }
            Err(e) => {
                return Err(Error::Io(std::io::Error::new(
                    e.kind(),
                    format!("cannot listen on {socket}: {e}"),
                )));
            }
        };

        let store = {
            let paths = paths.clone();
            let key = opts.store_key;
            blocking(move || {
                let key = match key {
                    Some(k) => k,
                    None => StoreKey::load_or_create(&paths)?,
                };
                tracing::info!(source = ?key.source(), "store key loaded");
                // Applies a staged restore; a damaged database is never opened (recovery mode).
                crate::features::backup::open_store(&paths, key)
            })
            .await?
        };
        // Pin a legacy store's embedding model before anything can embed (EM-1).
        {
            let store = store.clone();
            blocking(move || local::pin_legacy_embedding_model(&store)).await?;
        }
        // After the store: applying a staged restore may have put the backup's config in place.
        let (mut config, config_created) = Config::load_or_create(&paths)?;
        if config_created {
            tracing::info!(path = %paths.config_file().display(), "created default config");
        }
        let egress: Arc<dyn EgressSink> = Arc::new(store.clone());

        let (providers, unavailable) = {
            let egress = egress.clone();
            let wanted = config.providers.clone();
            blocking(move || Ok(build_providers(&wanted, egress))).await?
        };
        let configs = config.providers.clone();
        let built: Vec<ProviderConfig> = configs
            .iter()
            .filter(|c| !unavailable.contains_key(&c.id))
            .cloned()
            .collect();

        let overrides = opts.runtime;
        let manifest = match overrides.manifest.clone() {
            Some(m) => Ok(m),
            // Signature, entry validation and rollback protection.
            None => default_manifest_checked(&paths).map_err(|e| {
                tracing::error!(error = %e, "bundled model manifest failed verification");
                e.to_string()
            }),
        };
        if let Ok(m) = &manifest {
            for (tier, id) in config.models.selected.iter() {
                if m.get(id).is_none() {
                    tracing::warn!(
                        tier = tier_name(tier),
                        model = id,
                        "models.selected names a model that is not in the manifest"
                    );
                }
            }
        }
        // Creates the models directory and an HTTP client (which loads TLS roots).
        let models = {
            let (paths, egress) = (paths.clone(), egress.clone());
            Arc::new(
                blocking(move || ModelStore::open(&paths, egress))
                    .await?
                    .with_download_options(DownloadOptions {
                        parts: config.models.download_parts,
                        ..DownloadOptions::default()
                    }),
            )
        };
        let hardware = match overrides.hardware {
            Some(hw) => hw,
            None => blocking(|| Ok(nugget_models::detect_hardware())).await?,
        };

        let local = match (&manifest, config.runtime.enabled) {
            (Ok(m), true) => {
                let installed = {
                    let models = models.clone();
                    blocking(move || models.installed())
                        .await
                        .unwrap_or_else(|e| {
                            tracing::warn!(error = %e, "cannot list installed models");
                            Default::default()
                        })
                };
                let mut runtime = Runtime::new(&paths, models.clone(), egress.clone())?
                    .with_manifest(m.clone())
                    .with_hardware(hardware);
                let mut ropts = overrides.options.clone().unwrap_or_default();
                if ropts.idle_shutdown.is_none() {
                    ropts.idle_shutdown = (config.runtime.idle_stop_minutes > 0)
                        .then(|| Duration::from_secs(config.runtime.idle_stop_minutes * 60));
                }
                runtime = runtime.with_options(ropts);
                if let Some((bin, accel)) = overrides.server_binary.clone() {
                    runtime = runtime.with_server_binary(bin, accel);
                }
                let index_model = {
                    let store = store.clone();
                    blocking(move || Ok(local::index_embedding_model(&store)))
                        .await
                        .unwrap_or_default()
                };
                let selected =
                    local::selected_models_for_index(&config, m, &hardware, index_model.as_deref());
                Some(Arc::new(LocalRuntime::new(
                    runtime,
                    selected,
                    &|id| installed.contains_key(id),
                    egress.clone(),
                )))
            }
            (Err(_), true) => {
                tracing::warn!("local runtime disabled: no verified model manifest");
                None
            }
            (_, false) => None,
        };

        let localities = local::localities(&providers);
        let probes = ProbeCache::load(ProbeCache::path_for(&paths));
        let configured: Vec<_> = built
            .iter()
            .map(|c| {
                let loc = localities
                    .get(&c.id)
                    .copied()
                    .unwrap_or(nugget_core::capability::Locality::Cloud);
                (c.id.clone(), c.tiers.clone(), loc, probes.get(c).is_some())
            })
            .collect();
        let tier_map = local::tier_map(&configured, &local::slot_routes(local.as_deref()));
        // Interactive chats on configured on-device and LAN providers hold the foreground too.
        let foreground = local::ForegroundCell::default();
        let mut all_providers = local::guard_configured(providers, &foreground);
        if let Some(l) = &local {
            all_providers.extend(local::as_providers(l));
        }
        let router = Arc::new(Router::new(all_providers, tier_map));
        if let Some(l) = &local {
            l.attach(&router);
        }
        for cfg in &built {
            if let Some(caps) = probes.get(cfg) {
                router.set_capabilities(&cfg.id, caps.clone());
            }
        }

        // Recovery mode (damaged database, empty temporary store): no source kinds and no keychain,
        // so source ids reused in the empty store never touch the real credentials.
        let recovery = crate::features::backup::in_recovery(&paths);
        let secrets: Arc<dyn SecretStore> = match opts.source_secrets {
            _ if recovery => Arc::new(MemorySecrets::new()),
            Some(s) => s,
            None if sources::keychain_disabled() => {
                tracing::warn!(
                    "NUGGET_KEYCHAIN is off: source passwords and tokens are kept in memory only and must be set again after a restart"
                );
                Arc::new(MemorySecrets::new())
            }
            None => Arc::new(KeyringSecrets::new()),
        };
        let mut registry = opts.connectors.unwrap_or_else(sources::registry);
        // Opening the plugin store and compiling installed plugins is file and CPU work.
        let plugin_factories = {
            let (paths, config) = (paths.clone(), config.clone());
            blocking(move || {
                Ok(crate::features::plugins::connector_factories(
                    &paths, &config,
                ))
            })
            .await?
        };
        plugin_factories
            .into_iter()
            .for_each(|f| registry.register(f));
        if recovery {
            registry = Registry::new();
        }
        let resolver =
            sources::client_secret_resolver(secrets.clone(), sources::oauth_kinds(&registry));
        let power: Arc<dyn PowerPolicy> = opts.power.unwrap_or_else(|| {
            Arc::new(SystemPowerPolicy::new(config.ingest.low_battery_threshold))
        });
        let ingest = Ingest::builder(
            config.ingest.clone(),
            store.clone(),
            registry,
            paths.clone(),
        )
        .router(Some(router.clone()))
        .secrets(secrets.clone())
        .settings_resolver(resolver)
        .power(power.clone())
        .start()
        .await
        .map_err(|e| match e {
            Error::Invalid(m) => Error::Invalid(format!("sources: {m}")),
            other => other,
        })?;
        let _ = foreground.set(ingest.foreground());
        // Items indexed text-only while no embedder was routable get vectors once one is.
        if let Some(l) = &local {
            let ingest = ingest.clone();
            l.set_foreground(ingest.foreground());
            l.on_ready(Arc::new(move |role| {
                if role == nugget_core::ipc::RuntimeRole::Embedding {
                    ingest.reembed_now();
                }
                // A chat model became ready: background triage can start (HH2).
                if role == nugget_core::ipc::RuntimeRole::Chat {
                    ingest.triage_now();
                }
            }));
        }
        let retriever =
            Retriever::new(store.clone(), Some(router.clone()), config.retrieve.clone());
        let hub = Arc::new(Hub::default());
        let tasks = TaskTracker::new();
        let shutdown = CancellationToken::new();
        let retriever = crate::rerank::attach(
            retriever,
            &config.retrieve,
            local.as_deref(),
            egress.clone(),
            &tasks,
            &shutdown,
        );

        let default_vault = opts
            .default_vault_dir
            .clone()
            .unwrap_or_else(|| crate::knowledge::default_vault_dir(&paths));
        let vault_dir = crate::knowledge::vault_root(&config.vault, &default_vault)?;
        crate::features::backup::prepare_config(&paths, &mut config, &vault_dir, &default_vault);
        let knowledge = {
            let (store, router, hub, power) =
                (store.clone(), router.clone(), hub.clone(), power.clone());
            let (kcfg, vcfg) = (config.knowledge.clone(), config.vault.clone());
            Arc::new(
                blocking(move || {
                    KnowledgeService::new(store, router, kcfg, &vcfg, vault_dir, power, hub)
                })
                .await?,
            )
        };

        let mcp = if config.mcp.enabled {
            let backend = Arc::new(DaemonBackend {
                store: store.clone(),
                graph: knowledge.graph.clone(),
                retriever: retriever.clone(),
            });
            let exe = opts
                .nuggetd_exe
                .clone()
                .or_else(|| std::env::current_exe().ok())
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "nuggetd".into());
            Some(Arc::new(
                McpService::start(
                    &config.mcp,
                    backend,
                    store.clone(),
                    exe,
                    opts.home.clone(),
                    &tasks,
                    shutdown.clone(),
                )
                .await?,
            ))
        } else {
            None
        };

        tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            socket = %socket,
            providers = built.len(),
            unavailable = unavailable.len(),
            local_runtime = local.is_some(),
            knowledge = config.knowledge.enabled,
            vault = knowledge.vault.is_some(),
            mcp_http = mcp.as_ref().and_then(|m| m.http_addr()).map(|a| a.to_string()),
            encrypted = store.is_encrypted(),
            ram_gb = hardware.total_ram / nugget_models::GIB,
            "nuggetd ready"
        );

        let audit_chain_ok = Arc::new(OnceLock::new());
        spawn_audit_check(store.clone(), audit_chain_ok.clone());

        let state = Arc::new(State {
            paths,
            started: Instant::now(),
            store,
            router,
            local,
            ingest,
            retriever,
            knowledge,
            mcp,
            secrets,
            hub,
            configs: RwLock::new(configs),
            unavailable: RwLock::new(unavailable),
            audit_chain_ok,
            probes: Arc::new(Mutex::new(probes)),
            manifest,
            models,
            hardware,
            streams: Mutex::new(HashMap::new()),
            answers: Mutex::new(crate::ask::AnswerMemory::default()),
            installs: Arc::new(InFlight::default()),
            tasks,
            shutdown,
            features: OnceLock::new(),
        });

        // Forward ingest events to every client; start installed local models (not awaited).
        crate::hub::spawn_forwarder(state.hub.clone(), &state.ingest, state.shutdown.clone());
        // Local models first, so knowledge's startup run extracts with a starting chat model.
        if let Some(l) = &state.local {
            for slot in &l.slots {
                if slot.is_installed() {
                    spawn_ensure(&state, slot.clone());
                }
            }
        }
        state
            .knowledge
            .spawn(&state.ingest, &state.tasks, state.shutdown.clone());

        let features = crate::features::build(&crate::features::FeatureContext {
            state: &state,
            config: &config,
        })
        .await;
        let _ = state.features.set(features);

        Ok(Startup::Ready(Box::new(Daemon {
            state,
            listener,
            config_created,
        })))
    }

    pub fn ingest(&self) -> Ingest {
        self.state.ingest.clone()
    }

    pub fn runtime(&self) -> Option<Runtime> {
        self.state.local.as_ref().map(|l| l.runtime.clone())
    }

    pub fn store(&self) -> Store {
        self.state.store.clone()
    }

    pub fn mcp_http_addr(&self) -> Option<std::net::SocketAddr> {
        self.state.mcp.as_ref().and_then(|m| m.http_addr())
    }

    pub fn vault_root(&self) -> Option<PathBuf> {
        self.state
            .knowledge
            .vault
            .as_ref()
            .map(|v| v.root().to_path_buf())
    }

    pub fn paths(&self) -> &Paths {
        &self.state.paths
    }

    pub fn config_created(&self) -> bool {
        self.config_created
    }

    /// Serve IPC until `shutdown` resolves, then wind down.
    pub async fn run_until<F>(self, shutdown: F) -> Result<()>
    where
        F: Future<Output = ()>,
    {
        let state = self.state;
        let handler = Arc::new(RpcHandler::new(state.clone()));
        let winding = state.clone();
        let stop = state.shutdown.clone();
        let shutdown = async move {
            // A feature may stop the daemon itself (a staged restore) by cancelling `shutdown`.
            tokio::select! {
                () = shutdown => {}
                () = stop.cancelled() => {}
            }
            wind_down(&winding).await;
        };
        let served = nugget_ipc::serve_until(self.listener, handler, shutdown).await;
        // `serve_until` can also return early on a listener failure.
        if !state.shutdown.is_cancelled() {
            wind_down(&state).await;
        }
        if let Some(f) = state.features.get() {
            f.shutdown().await;
        }
        if let Some(m) = &state.mcp {
            m.shutdown().await;
        }
        state.ingest.shutdown().await;
        // An interrupted knowledge run may still wait on a model request: let it end first.
        let _ = tokio::time::timeout(KNOWLEDGE_GRACE, state.knowledge.knowledge.idle()).await;
        if let Some(l) = &state.local {
            l.runtime.shutdown().await;
        }
        let _ = tokio::time::timeout(KNOWLEDGE_GRACE, state.knowledge.knowledge.idle()).await;
        let store = state.store.clone();
        let _ = blocking(move || {
            // The process exits without dropping the store, which would otherwise read as a crash
            // at the next start and cost a full page check (II5).
            store.close_cleanly();
            Ok(())
        })
        .await;
        served.map_err(Error::Io)?;
        tracing::info!("stopped");
        Ok(())
    }
}

async fn wind_down(state: &State) {
    tracing::info!("shutting down");
    state.shutdown.cancel();
    state.tasks.close();
    if tokio::time::timeout(SHUTDOWN_GRACE, state.tasks.wait())
        .await
        .is_err()
    {
        tracing::warn!(
            remaining = state.tasks.len(),
            "background tasks did not stop in time"
        );
    }
}

/// Verify the audit hash chain in the background; the result lands in `out`.
fn spawn_audit_check(store: Store, out: Arc<OnceLock<bool>>) {
    tokio::task::spawn_blocking(move || {
        let ok = match store.verify_audit_chain() {
            Ok(()) => {
                tracing::info!("audit chain verified");
                true
            }
            Err(e) => {
                tracing::error!(error = %e, "AUDIT CHAIN VERIFICATION FAILED: the audit log may have been tampered with");
                false
            }
        };
        let _ = out.set(ok);
    });
}

/// Build every provider that can be built.
fn build_providers(
    configs: &[ProviderConfig],
    egress: Arc<dyn EgressSink>,
) -> (Vec<Arc<dyn Provider>>, HashMap<ProviderId, String>) {
    let mut providers = Vec::new();
    let mut unavailable = HashMap::new();
    for cfg in configs {
        match build_provider(cfg, egress.clone()) {
            Ok(p) => {
                tracing::info!(provider = %cfg.id, kind = %cfg.kind, model = %cfg.model, locality = ?p.meta().locality, "provider ready");
                providers.push(p);
            }
            Err(e) => {
                tracing::warn!(provider = %cfg.id, error = %e, "provider unavailable; not routed");
                unavailable.insert(cfg.id.clone(), e.to_string());
            }
        }
    }
    (providers, unavailable)
}

/// Run blocking work (SQLite, keychain, filesystem) off the async workers.
pub(crate) async fn blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::Other(format!("background task failed: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_flight_allows_one_holder_per_id() {
        let set = Arc::new(InFlight::default());
        let a = set.begin("m").expect("first claim");
        assert!(set.begin("m").is_none(), "duplicate claim must be rejected");
        assert!(set.begin("other").is_some(), "other ids are independent");
        drop(a);
        assert!(set.begin("m").is_some(), "released on drop");
    }
}
