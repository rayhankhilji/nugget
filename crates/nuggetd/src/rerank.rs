//! Which reranker search uses (see `nugget_retrieve::rerank`).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nugget_core::egress::EgressSink;
use nugget_core::ids::ItemRef;
use nugget_core::ipc::RuntimeState;
use nugget_core::{Error, Result};
use nugget_infer::{ProviderConfig, SecretResolver};
use nugget_retrieve::{HttpReranker, HttpRerankerConfig, Reranker, RetrieveConfig, Retriever};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::local::{LocalRuntime, RuntimeSlot};

pub(crate) type Starter = Arc<dyn Fn(Arc<RuntimeSlot>) + Send + Sync>;

/// Least time between two background starts of a server that is not running.
const START_INTERVAL: Duration = Duration::from_secs(30);

/// `retriever` with the reranker the config selects (see the module docs). Logs the choice.
pub(crate) fn attach(
    retriever: Retriever,
    cfg: &RetrieveConfig,
    local: Option<&LocalRuntime>,
    egress: Arc<dyn EgressSink>,
    tasks: &TaskTracker,
    shutdown: &CancellationToken,
) -> Retriever {
    match reranker(cfg, local, egress, starter(tasks.clone(), shutdown.clone())) {
        Some(r) => retriever.with_reranker(r),
        None => retriever,
    }
}

/// The reranker for `cfg` and `local`, if any. `start` starts the rerank runtime on demand.
pub(crate) fn reranker(
    cfg: &RetrieveConfig,
    local: Option<&LocalRuntime>,
    egress: Arc<dyn EgressSink>,
    start: Starter,
) -> Option<Arc<dyn Reranker>> {
    if let Some(rc) = &cfg.reranker {
        match HttpReranker::new(rc, None, egress.clone()) {
            Ok(r) => {
                tracing::info!(url = %rc.base_url, locality = ?r.locality(), "search reranker: [retrieve.reranker]");
                return Some(Arc::new(r));
            }
            Err(e) => {
                tracing::warn!(error = %e, "[retrieve.reranker] is unusable; ignoring it");
            }
        }
    }
    if let Some(slot) = local.and_then(LocalRuntime::rerank_slot) {
        tracing::info!(provider = %slot.id(), model = %slot.model().id, "search reranker: local runtime");
        return Some(Arc::new(LocalReranker::new(slot.clone(), egress, start)));
    }
    tracing::info!("search reranker: none");
    None
}

fn starter(tasks: TaskTracker, shutdown: CancellationToken) -> Starter {
    Arc::new(move |slot: Arc<RuntimeSlot>| {
        let stop = shutdown.clone();
        tasks.spawn(async move {
            tokio::select! {
                () = stop.cancelled() => {}
                r = slot.ready() => if let Err(e) = r {
                    tracing::warn!(provider = %slot.id(), error = %e, "local reranker did not start");
                },
            }
        });
    })
}

pub(crate) struct LocalReranker {
    slot: Arc<RuntimeSlot>,
    egress: Arc<dyn EgressSink>,
    start: Starter,
    /// The client for the server's current config (rebuilt when it restarts with a new key).
    client: Mutex<Option<(ProviderConfig, Arc<HttpReranker>)>>,
    last_start: Mutex<Option<Instant>>,
    name: String,
}

impl LocalReranker {
    pub(crate) fn new(slot: Arc<RuntimeSlot>, egress: Arc<dyn EgressSink>, start: Starter) -> Self {
        let name = slot.id().to_string();
        Self {
            slot,
            egress,
            start,
            client: Mutex::new(None),
            last_start: Mutex::new(None),
            name,
        }
    }

    /// Start the server in the background if installed and stopped (at most every interval).
    fn start_if_stopped(&self) {
        if !self.slot.is_installed() {
            return;
        }
        if matches!(
            self.slot.state(),
            Some(RuntimeState::Starting | RuntimeState::Ready | RuntimeState::Stopping)
        ) {
            return;
        }
        {
            let mut last = self
                .last_start
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if last.is_some_and(|t| t.elapsed() < START_INTERVAL) {
                return;
            }
            *last = Some(Instant::now());
        }
        (self.start)(self.slot.clone());
    }

    fn client_for(&self, cfg: ProviderConfig) -> Result<Arc<HttpReranker>> {
        let mut g = self.client.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((c, r)) = g.as_ref()
            && *c == cfg
        {
            return Ok(r.clone());
        }
        // The runtime's config names the OpenAI-style root (`…/v1`); the reranker its own path.
        let base = cfg.base_url.as_deref().unwrap_or_default();
        let base = base.trim_end_matches('/');
        let base = base.strip_suffix("/v1").unwrap_or(base);
        let rc = HttpRerankerConfig {
            base_url: base.to_string(),
            model: cfg.model.clone(),
            socket_path: cfg.socket_path.clone(),
            ..HttpRerankerConfig::default()
        };
        let key = cfg
            .api_key
            .as_ref()
            .map(|k| self.slot.runtime().secrets().resolve(k))
            .transpose()?;
        let r = Arc::new(HttpReranker::new(&rc, key, self.egress.clone())?);
        *g = Some((cfg, r.clone()));
        Ok(r)
    }
}

#[async_trait]
impl Reranker for LocalReranker {
    fn name(&self) -> &str {
        &self.name
    }

    fn locality(&self) -> nugget_core::capability::Locality {
        nugget_core::capability::Locality::OnDevice
    }

    async fn rerank(&self, query: &str, passages: &[String]) -> Result<Vec<f32>> {
        self.rerank_items(query, passages, &[]).await
    }

    async fn rerank_items(
        &self,
        query: &str,
        passages: &[String],
        items: &[ItemRef],
    ) -> Result<Vec<f32>> {
        let Some(cfg) = self.slot.ready_config() else {
            self.start_if_stopped();
            return Err(Error::Unsupported(format!(
                "local reranker `{}` is not running",
                self.slot.model().id
            )));
        };
        let client = self.client_for(cfg)?;
        client.rerank_items(query, passages, items).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use nugget_core::egress::NullEgress;
    use nugget_core::ipc::RuntimeRole;
    use nugget_models::ModelStore;
    use nugget_runtime::Runtime;
    use nugget_store::{NewChunk, NewItem, Store, content_hash};

    use super::*;

    fn add(store: &Store, key: &str, text: &str) {
        let item = NewItem::new("notes", key, content_hash(text)).with_title(key);
        store.upsert_item(&item).unwrap();
        store
            .replace_chunks(&item.item, &[NewChunk::text(text)])
            .unwrap();
    }

    fn store() -> Store {
        let store = Store::open_in_memory().unwrap();
        add(&store, "a", "Boiler boiler boiler: the boiler manual.");
        add(
            &store,
            "b",
            "Boiler repair: fault F28, ignition electrode replaced.",
        );
        store
    }

    fn cfg() -> RetrieveConfig {
        RetrieveConfig {
            mmr: false,
            rerank_timeout_ms: 20_000,
            ..Default::default()
        }
    }

    fn no_start() -> (Starter, Arc<AtomicUsize>) {
        let n = Arc::new(AtomicUsize::new(0));
        let c = n.clone();
        (
            Arc::new(move |_| {
                c.fetch_add(1, Ordering::SeqCst);
            }),
            n,
        )
    }

    fn keys(r: &nugget_retrieve::SearchResults) -> Vec<&str> {
        r.items.iter().map(|i| i.item.key.as_str()).collect()
    }

    /// A local reranker that is not ready fails at once and starts its server only when installed.
    #[tokio::test]
    async fn a_local_reranker_that_is_not_ready_fails_fast() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = nugget_core::paths::Paths::rooted(tmp.path().to_path_buf());
        let egress: Arc<dyn EgressSink> = Arc::new(NullEgress);
        let models = Arc::new(ModelStore::open(&paths, egress.clone()).unwrap());
        let runtime = Runtime::new(&paths, models, egress.clone()).unwrap();
        let entry = nugget_models::default_manifest().unwrap().models[0].clone();
        let local = LocalRuntime::new(
            runtime,
            vec![(RuntimeRole::Rerank, entry)],
            &|_| false,
            egress.clone(),
        );
        let (start, started) = no_start();
        let r = reranker(&cfg(), Some(&local), egress, start).expect("a local reranker");
        assert_eq!(r.name(), "local-rerank");
        let retriever = Retriever::new(store(), None, cfg()).with_reranker(r.clone());

        let t = Instant::now();
        let res = retriever.search("boiler", 5).await.unwrap();
        assert!(!res.reranked);
        assert_eq!(keys(&res), ["a", "b"]);
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        assert_eq!(started.load(Ordering::SeqCst), 0, "not installed: no start");

        // Installed but never started: a background start, then no more within the interval.
        local.rerank_slot().unwrap().set_installed(true);
        let res = retriever.search("boiler", 5).await.unwrap();
        assert!(!res.reranked);
        assert_eq!(started.load(Ordering::SeqCst), 1);
        let err = r.rerank("q", &["p".into()]).await.unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "{err:?}");
        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    }
}
