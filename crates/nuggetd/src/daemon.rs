//! Daemon lifecycle: bind the socket, open the store, build providers, the local runtime and
//! the router, start ingest, serve IPC until shutdown, then wind down in order: stop accepting
//! (and close connections), ingest, the local runtime, then flush the store.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use nugget_connector::{Registry, SecretStore};
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

/// How long shutdown waits for chat streams and installs to stop.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// How to start a daemon.
#[derive(Clone)]
pub struct DaemonOptions {
    pub paths: Paths,
    /// Use this store key instead of [`StoreKey::load_or_create`] (tests).
    pub store_key: Option<StoreKey>,
    /// Where source credentials live. Default: the OS keychain, or process memory (lost at
    /// exit, with a warning) when `NUGGET_KEYCHAIN=off`. Tests inject a `MemorySecrets`.
    pub source_secrets: Option<Arc<dyn SecretStore>>,
    /// Local runtime overrides (tests).
    pub runtime: RuntimeOverrides,
    /// Power policy for ingest and knowledge extraction. Default: the system policy at
    /// `[ingest] low_battery_threshold`.
    pub power: Option<Arc<dyn PowerPolicy>>,
    /// Vault directory when `[vault] path` is unset. Default: `<data>/vault`; the binary passes
    /// `~/Nugget Vault` for a normal install (no `--home` / `NUGGET_HOME`).
    pub default_vault_dir: Option<PathBuf>,
    /// The data root given with `--home` / `NUGGET_HOME`, which MCP setup snippets pass on as
    /// `NUGGET_HOME` so the launched bridge finds this daemon.
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
    /// Supervisor timings.
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

/// Result of [`Daemon::start`].
pub enum Startup {
    Ready(Box<Daemon>),
    /// Another daemon already answers on this socket.
    AlreadyRunning {
        socket: String,
    },
}

/// A started daemon: the socket is bound and everything is open, but nothing is served until
/// [`Daemon::run_until`].
pub struct Daemon {
    state: Arc<State>,
    listener: Listener,
    config_created: bool,
}

/// Everything handlers share.
pub(crate) struct State {
    pub paths: Paths,
    pub started: Instant,
    pub store: Store,
    pub router: Arc<Router>,
    /// The on-device runtime, unless `[runtime] enabled = false` (or no manifest).
    pub local: Option<Arc<LocalRuntime>>,
    pub ingest: Ingest,
    pub retriever: Retriever,
    /// Knowledge extraction, the graph and the vault.
    pub knowledge: Arc<KnowledgeService>,
    /// The MCP server, unless `[mcp] enabled = false`.
    pub mcp: Option<Arc<McpService>>,
    pub secrets: Arc<dyn SecretStore>,
    /// Broadcast notifications to every connection.
    pub hub: Arc<Hub>,
    /// Every configured provider, in config order (including ones that failed to build).
    pub configs: Vec<ProviderConfig>,
    /// Providers that could not be built, with the reason. They are not in the router.
    pub unavailable: HashMap<ProviderId, String>,
    /// Result of verifying the audit hash chain; unset until the startup check finishes.
    pub audit_chain_ok: Arc<OnceLock<bool>>,
    pub probes: Arc<Mutex<ProbeCache>>,
    pub manifest: std::result::Result<Manifest, String>,
    pub models: Arc<ModelStore>,
    pub hardware: Hardware,
    /// Cancellation per running chat stream.
    pub streams: Mutex<HashMap<String, CancellationToken>>,
    /// The sources of recent Ask answers, for follow-ups' history (`crate::ask::AnswerMemory`).
    pub answers: Mutex<crate::ask::AnswerMemory>,
    /// Model ids being installed.
    pub installs: Arc<InFlight>,
    pub tasks: TaskTracker,
    pub shutdown: CancellationToken,
    /// Wired-in subsystems (`crate::features`); set once, right after `State` is built.
    pub features: OnceLock<crate::features::Features>,
}

impl State {
    /// Config of a provider that was built (and so is in the router).
    pub fn config_of(&self, id: &ProviderId) -> Option<&ProviderConfig> {
        if self.unavailable.contains_key(id) {
            return None;
        }
        self.configs.iter().find(|c| &c.id == id)
    }

    pub fn streams(&self) -> MutexGuard<'_, HashMap<String, CancellationToken>> {
        self.streams.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The runtime slot routed under `id`, if any.
    pub fn slot(&self, id: &ProviderId) -> Option<&Arc<RuntimeSlot>> {
        self.local.as_ref().and_then(|l| l.slot(id))
    }
}

/// Start `slot`'s server in the background (tracked; stops on shutdown).
pub(crate) fn spawn_ensure(state: &Arc<State>, slot: Arc<RuntimeSlot>) {
    spawn_start(state, slot, false);
}

/// Like [`spawn_ensure`], but always asks the runtime to check the running server against the
/// installed model (restarting it on a new one). Used after `models.install`.
pub(crate) fn spawn_refresh(state: &Arc<State>, slot: Arc<RuntimeSlot>) {
    spawn_start(state, slot, true);
}

fn spawn_start(state: &Arc<State>, slot: Arc<RuntimeSlot>, refresh: bool) {
    let stop = state.shutdown.clone();
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
                Ok(_) => tracing::info!(provider = %slot.id(), %model, "local runtime ready"),
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
    /// Claim `id`, or `None` if it is already claimed.
    pub fn begin(self: &Arc<Self>, id: &str) -> Option<InFlightGuard> {
        let mut set = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        set.insert(id.to_string()).then(|| InFlightGuard {
            set: Arc::clone(self),
            id: id.to_string(),
        })
    }

    pub fn contains(&self, id: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(id)
    }
}

/// Releases its id when dropped.
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
    ///
    /// Returns [`Startup::AlreadyRunning`] if another daemon answers on the socket.
    pub async fn start(opts: DaemonOptions) -> Result<Startup> {
        let paths = opts.paths;
        let socket = nugget_ipc::socket_display(&paths);

        // Bind first so two daemons can never open the same store. A live daemon makes `bind`
        // fail with AddrInUse; the connect probe also covers named pipes, where a second
        // listener instance could otherwise be created.
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
            // Signature, entry validation and rollback protection (per-key version record in the
            // model store).
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

        // The local runtime's slots join the router from the start (see `local`).
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
                let selected = local::selected_models(&config, m, &hardware);
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
        let configured: Vec<_> = built
            .iter()
            .map(|c| {
                let loc = localities
                    .get(&c.id)
                    .copied()
                    .unwrap_or(nugget_core::capability::Locality::Cloud);
                (c.id.clone(), c.tiers.clone(), loc)
            })
            .collect();
        let tier_map = local::tier_map(&configured, &local::slot_routes(local.as_deref()));
        let mut all_providers = providers;
        if let Some(l) = &local {
            all_providers.extend(local::as_providers(l));
        }
        let router = Arc::new(Router::new(all_providers, tier_map));
        if let Some(l) = &local {
            l.attach(&router);
        }
        let probes = ProbeCache::load(ProbeCache::path_for(&paths));
        for cfg in &built {
            if let Some(caps) = probes.get(cfg) {
                router.set_capabilities(&cfg.id, caps.clone());
            }
        }

        // Ingest: every M1 connector kind, credentials in the keychain.
        // Recovery mode (damaged database, empty temporary store): no source kinds and no
        // keychain, so nothing runs and source ids reused in the empty store can never touch the
        // real credentials (see `features::backup`).
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
        // Items indexed text-only while no embedder was routable get vectors once one is.
        if let Some(l) = &local {
            let ingest = ingest.clone();
            l.on_ready(Arc::new(move |role| {
                if role == nugget_core::ipc::RuntimeRole::Embedding {
                    ingest.reembed_now();
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

        // Knowledge graph and vault.
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

        // MCP: backend over store, graph and retriever; HTTP on loopback when enabled.
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
            configs,
            unavailable,
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
        state
            .knowledge
            .spawn(&state.ingest, &state.tasks, state.shutdown.clone());
        if let Some(l) = &state.local {
            for slot in &l.slots {
                if slot.is_installed() {
                    spawn_ensure(&state, slot.clone());
                }
            }
        }

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

    /// The daemon's ingest runtime (tests).
    pub fn ingest(&self) -> Ingest {
        self.state.ingest.clone()
    }

    /// The local runtime, if enabled (tests).
    pub fn runtime(&self) -> Option<Runtime> {
        self.state.local.as_ref().map(|l| l.runtime.clone())
    }

    /// The daemon's store (tests seed it directly).
    pub fn store(&self) -> Store {
        self.state.store.clone()
    }

    /// Where the MCP HTTP listener is bound, if it runs.
    pub fn mcp_http_addr(&self) -> Option<std::net::SocketAddr> {
        self.state.mcp.as_ref().and_then(|m| m.http_addr())
    }

    /// The vault root, if the vault is enabled.
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

    /// Whether this start wrote the default config.
    pub fn config_created(&self) -> bool {
        self.config_created
    }

    /// Serve IPC until `shutdown` resolves, then stop chat streams and installs (waiting up to
    /// a few seconds for them) and close the socket.
    ///
    /// Connections stay open while background tasks wind down, so chat streams can tell their
    /// clients (`chat.done`) that the daemon is stopping.
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
        // Returns once the listener stopped accepting and every connection is closed.
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
        if let Some(l) = &state.local {
            l.runtime.shutdown().await;
        }
        let store = state.store.clone();
        let _ = blocking(move || {
            store.flush_egress();
            Ok(())
        })
        .await;
        served.map_err(Error::Io)?;
        tracing::info!("stopped");
        Ok(())
    }
}

/// Signal background tasks to stop and wait (bounded) for them to finish.
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

/// Verify the audit hash chain in the background; the result lands in `out`. A broken chain is
/// logged loudly but does not stop the daemon.
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

/// Build every provider that can be built. One that cannot (for example its API key variable is
/// unset) is reported with its reason instead of keeping the daemon from starting.
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
        let b = set.begin("other").expect("other ids are independent");
        assert!(set.contains("m"));
        drop(a);
        assert!(!set.contains("m"));
        assert!(set.begin("m").is_some(), "released on drop");
        drop(b);
    }
}
