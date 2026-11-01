//! `search.upgrade.*`: the one-time offer to move an existing search index to the recommended
//! embedding model (the multilingual one since manifest 5). Never automatic: only `start` runs it.
//!
//! Accepting records the target in the store (so it resumes after a restart), downloads the
//! model if needed, then runs it in a second, temporary embedding server
//! ([`nugget_runtime::Runtime::sibling`]) while ingest's re-embed job fills the store's staging
//! table at the lowest priority. Searches keep using the old vectors and model until every chunk
//! has a new vector; the store then switches in one transaction and the main embedding server
//! restarts on the new model. `dismiss` hides the offer for good and abandons a running upgrade.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use nugget_core::capability::Tier;
use nugget_core::ipc::search_upgrade_method::{UPGRADE_DISMISS, UPGRADE_START, UPGRADE_STATUS};
use nugget_core::ipc::{
    RuntimeRole, SearchUpgradeJob, SearchUpgradeOffer, SearchUpgradePhase, SearchUpgradeStatus,
};
use nugget_core::{Error, Result};
use nugget_infer::Provider;
use nugget_ipc::{Notifier, RpcError, to_result};
use nugget_models::{ModelEntry, recommend};
use nugget_store::ReindexTarget;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{Feature, FeatureContext};
use crate::daemon::{State, blocking};
use crate::local::{LocalRuntime, RuntimeSlot, index_embedding_model};

/// File under `data/` naming the model whose offer was dismissed.
const DISMISSED_FILE: &str = "search-upgrade-dismissed";

pub(crate) fn start(cx: &FeatureContext<'_>) -> Result<Arc<dyn Feature>> {
    let f = Arc::new(SearchUpgrade {
        state: cx.state.clone(),
        // An explicitly chosen embedding model is kept: no offer.
        pinned: cx.config.runtime.embedding_model.is_some()
            || cx.config.models.selected.embedding.is_some(),
        job: Arc::default(),
    });
    // Resume an upgrade accepted before a restart.
    if let Ok(Some(t)) = f.state.store.reindex_target()
        && let Some(entry) = f.manifest_entry(&t.model)
    {
        f.launch(entry);
    }
    Ok(f)
}

struct Job {
    to: String,
    phase: SearchUpgradePhase,
    downloaded: (u64, u64),
    error: Option<String>,
    cancel: CancellationToken,
    runtime: Option<Arc<LocalRuntime>>,
}

struct SearchUpgrade {
    state: Arc<State>,
    pinned: bool,
    job: Arc<Mutex<Option<Job>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl SearchUpgrade {
    fn manifest_entry(&self, id: &str) -> Option<ModelEntry> {
        self.state.manifest.as_ref().ok()?.get(id).cloned()
    }

    fn embedding_slot(&self) -> Option<Arc<RuntimeSlot>> {
        let local = self.state.local.as_ref()?;
        let slot = local
            .slots
            .iter()
            .find(|s| s.role() == RuntimeRole::Embedding);
        slot.cloned()
    }

    /// The offer, when it applies: the index was built with an on-device model the slot serves,
    /// another one is recommended, nothing was dismissed for it and no upgrade runs.
    fn offer(&self) -> Option<(String, ModelEntry)> {
        if self.pinned || lock(&self.job).is_some() {
            return None;
        }
        let manifest = self.state.manifest.as_ref().ok()?;
        let to = recommend(manifest, &self.state.hardware, Tier::Embedding)?.clone();
        let from = index_embedding_model(&self.state.store)?;
        let local = manifest
            .get(&from)
            .is_some_and(|e| e.tiers.contains(&Tier::Embedding));
        let dismissed = std::fs::read_to_string(self.state.paths.data.join(DISMISSED_FILE));
        (local
            && from != to.id
            && self.embedding_slot()?.model().id == from
            && dismissed.ok().as_deref().map(str::trim) != Some(to.id.as_str())
            && matches!(self.state.store.reindex_target(), Ok(None)))
        .then_some((from, to))
    }

    async fn status(&self) -> Result<SearchUpgradeStatus> {
        let offer = self.offer().map(|(from, to)| SearchUpgradeOffer {
            download_bytes: match self.state.models.slot(&to.id) {
                Ok(Some(_)) => 0,
                _ => to.size,
            },
            from,
            to: to.id,
        });
        let job = lock(&self.job).as_ref().map(|j| SearchUpgradeJob {
            to: j.to.clone(),
            phase: j.phase,
            done: j.downloaded.0,
            total: j.downloaded.1,
            error: j
                .error
                .clone()
                .or_else(|| self.state.ingest.reindex_error()),
        });
        let job = match job {
            Some(mut j) if j.phase == SearchUpgradePhase::Embedding => {
                let store = self.state.store.clone();
                (j.done, j.total) = blocking(move || store.reindex_progress()).await?;
                Some(j)
            }
            other => other,
        };
        Ok(SearchUpgradeStatus { offer, job })
    }

    async fn accept(&self) -> Result<()> {
        // An accepted upgrade that stopped on an error (say, the download failed): try again.
        let failed = lock(&self.job).as_ref().is_some_and(|j| j.error.is_some());
        if failed
            && let Ok(Some(t)) = self.state.store.reindex_target()
            && let Some(to) = self.manifest_entry(&t.model)
        {
            self.launch(to);
            return Ok(());
        }
        let Some((_, to)) = self.offer() else {
            return Err(Error::Invalid("no search upgrade is on offer".into()));
        };
        let (query_prefix, document_prefix) = nugget_infer::model_embed_prefixes(&to.id);
        let target = ReindexTarget {
            model: to.id.clone(),
            query_prefix,
            document_prefix,
        };
        let store = self.state.store.clone();
        blocking(move || store.begin_reindex(&target)).await?;
        tracing::info!(model = %to.id, "search upgrade accepted");
        self.launch(to);
        Ok(())
    }

    async fn dismiss(&self) -> Result<()> {
        let target = self
            .offer()
            .map(|(_, to)| to.id)
            .or_else(|| lock(&self.job).as_ref().map(|j| j.to.clone()));
        if let Some(job) = lock(&self.job).take() {
            job.cancel.cancel();
            self.state.ingest.stop_reindex();
            if let Some(rt) = job.runtime {
                let rt = rt.runtime.clone();
                self.state.tasks.spawn(async move { rt.shutdown().await });
            }
        }
        if let Some(e) = self.manifest_entry(target.as_deref().unwrap_or_default()) {
            self.state.models.cancel_install(&e);
        }
        let store = self.state.store.clone();
        blocking(move || store.cancel_reindex()).await?;
        if let Some(id) = target {
            std::fs::write(self.state.paths.data.join(DISMISSED_FILE), &id)
                .map_err(|e| Error::Storage(format!("record the dismissed offer: {e}")))?;
            tracing::info!(model = %id, "search upgrade dismissed");
        }
        Ok(())
    }

    /// Run the upgrade to `to` in the background (download, then re-embed; ingest switches).
    fn launch(&self, to: ModelEntry) {
        let Some(main) = self.embedding_slot() else {
            return;
        };
        let cancel = CancellationToken::new();
        *lock(&self.job) = Some(Job {
            to: to.id.clone(),
            phase: SearchUpgradePhase::Downloading,
            downloaded: (0, to.size),
            error: None,
            cancel: cancel.clone(),
            runtime: None,
        });
        let (state, job) = (self.state.clone(), self.job.clone());
        self.state.tasks.spawn(async move {
            let stop = state.shutdown.clone();
            tokio::select! {
                () = cancel.cancelled() => {}
                () = stop.cancelled() => {}
                r = run(state.clone(), job.clone(), main, to) => if let Err(e) = r {
                    tracing::warn!(error = %e, "search upgrade stopped");
                    if let Some(j) = lock(&job).as_mut() {
                        j.error = Some(nugget_ingest::cap_error(e.to_string()));
                    }
                },
            }
        });
    }
}

async fn run(
    state: Arc<State>,
    job: Arc<Mutex<Option<Job>>>,
    main: Arc<RuntimeSlot>,
    to: ModelEntry,
) -> Result<()> {
    let installed = matches!(state.models.slot(&to.id), Ok(Some(_)));
    if !installed {
        let _guard = state.installs.begin(&to.id).ok_or_else(|| {
            Error::Invalid(format!("model `{}` is already being installed", to.id))
        })?;
        let progress = job.clone();
        state
            .models
            .install(&to, move |p| {
                if let Some(j) = lock(&progress).as_mut() {
                    j.downloaded = (p.done, p.total);
                }
            })
            .await?;
    }
    let local = state
        .local
        .as_ref()
        .ok_or_else(|| Error::Unsupported("the local runtime is off".into()))?;
    let runtime = local.runtime.sibling("reindex")?;
    let reindex = Arc::new(LocalRuntime::new(
        runtime,
        vec![(RuntimeRole::Embedding, to.clone())],
        &|_| true,
        Arc::new(state.store.clone()),
    ));
    let slot = reindex.slots[0].clone();
    slot.ready().await?;
    if let Some(j) = lock(&job).as_mut() {
        j.phase = SearchUpgradePhase::Embedding;
        j.runtime = Some(reindex.clone());
    }
    let tasks = state.tasks.clone();
    state.ingest.start_reindex(
        slot as Arc<dyn Provider>,
        Arc::new(move |t| {
            let (job, main, to, reindex) = (job.clone(), main.clone(), to.clone(), reindex.clone());
            tasks.spawn(async move {
                reindex.runtime.shutdown().await;
                lock(&job).take();
                tracing::info!(model = %t.model, "search upgrade complete");
                if let Err(e) = main.switch_model(to).await {
                    tracing::warn!(error = %e, "the search model did not restart on the new model");
                }
            });
        }),
    );
    Ok(())
}

#[async_trait]
impl Feature for SearchUpgrade {
    fn name(&self) -> &'static str {
        "search_upgrade"
    }

    fn methods(&self) -> &'static [&'static str] {
        &[UPGRADE_STATUS, UPGRADE_START, UPGRADE_DISMISS]
    }

    async fn call(
        &self,
        method: &str,
        _params: Value,
        _notify: Notifier,
    ) -> std::result::Result<Value, RpcError> {
        match method {
            UPGRADE_STATUS => {}
            UPGRADE_START => self.accept().await?,
            UPGRADE_DISMISS => self.dismiss().await?,
            other => return Err(RpcError::method_not_found(other)),
        }
        to_result(self.status().await?)
    }

    async fn shutdown(&self) {
        let rt = lock(&self.job).as_ref().and_then(|j| j.runtime.clone());
        if let Some(rt) = rt {
            rt.runtime.shutdown().await;
        }
    }
}
