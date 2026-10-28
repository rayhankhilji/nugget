//! Daemon lifecycle: bind the socket, open the store, build providers and the router, serve IPC
//! until shutdown, then wind down background tasks.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use nugget_core::egress::EgressSink;
use nugget_core::ids::ProviderId;
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use nugget_infer::{Provider, ProviderConfig, Router, build_provider, tier_map_from_configs};
use nugget_ipc::Listener;
use nugget_models::{DownloadOptions, Hardware, Manifest, ModelStore, default_manifest};
use nugget_store::{Store, StoreKey};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::config::{Config, tier_name};
use crate::handler::RpcHandler;
use crate::probes::ProbeCache;

/// How long shutdown waits for chat streams and installs to stop.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// How to start a daemon.
#[derive(Clone)]
pub struct DaemonOptions {
    pub paths: Paths,
    /// Use this store key instead of [`StoreKey::load_or_create`] (tests).
    pub store_key: Option<StoreKey>,
}

impl DaemonOptions {
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            store_key: None,
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
    /// Configs of the providers that were built, in config order.
    pub configs: Vec<ProviderConfig>,
    pub probes: Arc<Mutex<ProbeCache>>,
    pub manifest: std::result::Result<Manifest, String>,
    pub models: Arc<ModelStore>,
    pub hardware: Hardware,
    /// Cancellation per running chat stream.
    pub streams: Mutex<HashMap<String, CancellationToken>>,
    /// Model ids being installed.
    pub installs: Arc<InFlight>,
    pub tasks: TaskTracker,
    pub shutdown: CancellationToken,
}

impl State {
    pub fn config_of(&self, id: &ProviderId) -> Option<&ProviderConfig> {
        self.configs.iter().find(|c| &c.id == id)
    }

    pub fn streams(&self) -> MutexGuard<'_, HashMap<String, CancellationToken>> {
        self.streams.lock().unwrap_or_else(PoisonError::into_inner)
    }
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

        let (config, config_created) = Config::load_or_create(&paths)?;
        if config_created {
            tracing::info!(path = %paths.config_file().display(), "created default config");
        }

        let store = {
            let paths = paths.clone();
            let key = opts.store_key;
            blocking(move || {
                let key = match key {
                    Some(k) => k,
                    None => StoreKey::load_or_create(&paths)?,
                };
                tracing::info!(source = ?key.source(), "store key loaded");
                Store::open(paths.db_file(), &key)
            })
            .await?
        };
        let egress: Arc<dyn EgressSink> = Arc::new(store.clone());

        let (providers, configs) = {
            let egress = egress.clone();
            let wanted = config.providers.clone();
            blocking(move || Ok(build_providers(&wanted, egress))).await?
        };
        let router = Router::new(providers, tier_map_from_configs(&configs));
        let probes = ProbeCache::load(ProbeCache::path_for(&paths));
        for cfg in &configs {
            if let Some(caps) = probes.get(cfg) {
                router.set_capabilities(&cfg.id, caps.clone());
            }
        }

        let manifest = default_manifest().map_err(|e| {
            tracing::error!(error = %e, "bundled model manifest failed verification");
            e.to_string()
        });
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
        let models =
            ModelStore::open(&paths, egress.clone())?.with_download_options(DownloadOptions {
                parts: config.models.download_parts,
                ..DownloadOptions::default()
            });
        let hardware = blocking(|| Ok(nugget_models::detect_hardware())).await?;

        tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            socket = %socket,
            providers = configs.len(),
            encrypted = store.is_encrypted(),
            ram_gb = hardware.total_ram / nugget_models::GIB,
            "nuggetd ready"
        );

        Ok(Startup::Ready(Box::new(Daemon {
            state: Arc::new(State {
                paths,
                started: Instant::now(),
                store,
                router: Arc::new(router),
                configs,
                probes: Arc::new(Mutex::new(probes)),
                manifest,
                models: Arc::new(models),
                hardware,
                streams: Mutex::new(HashMap::new()),
                installs: Arc::new(InFlight::default()),
                tasks: TaskTracker::new(),
                shutdown: CancellationToken::new(),
            }),
            listener,
            config_created,
        })))
    }

    /// The daemon's store (tests seed it directly).
    pub fn store(&self) -> Store {
        self.state.store.clone()
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
    pub async fn run_until<F>(self, shutdown: F) -> Result<()>
    where
        F: Future<Output = ()>,
    {
        let state = self.state;
        let handler = Arc::new(RpcHandler::new(state.clone()));
        let served = nugget_ipc::serve_until(self.listener, handler, shutdown).await;
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
        served.map_err(Error::Io)?;
        tracing::info!("stopped");
        Ok(())
    }
}

/// Build every provider that can be built. One that cannot (for example its API key variable is
/// unset) is skipped with a warning instead of keeping the daemon from starting.
fn build_providers(
    configs: &[ProviderConfig],
    egress: Arc<dyn EgressSink>,
) -> (Vec<Arc<dyn Provider>>, Vec<ProviderConfig>) {
    let mut providers = Vec::new();
    let mut built = Vec::new();
    for cfg in configs {
        match build_provider(cfg, egress.clone()) {
            Ok(p) => {
                tracing::info!(provider = %cfg.id, kind = %cfg.kind, model = %cfg.model, locality = ?p.meta().locality, "provider ready");
                providers.push(p);
                built.push(cfg.clone());
            }
            Err(e) => {
                tracing::warn!(provider = %cfg.id, error = %e, "provider unavailable; skipping");
            }
        }
    }
    (providers, built)
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
