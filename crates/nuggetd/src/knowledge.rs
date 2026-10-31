//! Knowledge extraction and the Markdown vault, wired into the daemon.
//!
//! - **Extraction** ([`Knowledge::run_once`]) runs on its own task: at startup, after ingest
//!   commits changes (debounced; woken by [`Ingest::subscribe_committed`]), periodically (`interval_minutes`, which also retries pending LLM
//!   extraction), and again right away while a run leaves planned items behind. While the power
//!   policy is constrained, runs make no LLM requests (deterministic extractors still keep the
//!   graph fresh) and periodic runs are skipped.
//! - **Notifications.** Entity ids changed by each run are broadcast as `knowledge.changed`
//!   ([`KnowledgeChanged`]); so are entities touched by `knowledge.merge`,
//!   `knowledge.claim.status` and by vault edits picked up by the watcher.
//! - **Vault.** After every run (and after merges and claim-status changes), pages of entities
//!   changed in the graph's change feed are re-rendered with `Vault::render_changed`; the
//!   high-water mark is persisted in the store (cursor key [`VAULT_MARK_KEY`], with the vault
//!   root, so moving the vault re-renders everything; a vault last rendered under older render
//!   rules, `Vault::needs_full_render`, is rendered in full once even with no graph change).
//!   `Vault::watch` applies user edits and signals them (`Vault::subscribe_edits`), which
//!   triggers that pass.
//! - **No polling.** The worker sleeps until one of those events or a deadline (debounce,
//!   `interval_minutes`), so an idle daemon spends no wakeups here.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use nugget_core::ids::{EntityId, SourceId};
use nugget_core::ipc::{KnowledgeChanged, VaultStatus, notification};
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use nugget_graph::GraphStore;
use nugget_infer::Router;
use nugget_ingest::{Ingest, PowerPolicy};
use nugget_knowledge::{Knowledge, RunBudget, RunStats};
use nugget_store::Store;
use nugget_vault::{ItemLookup, Vault};
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::sync::broadcast;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::config::{KnowledgeSection, VaultSection};
use crate::daemon::blocking;
use crate::hub::Hub;

/// Store cursor key holding the vault's render high-water mark `{"root": ..., "seq": n}`.
pub const VAULT_MARK_KEY: &str = "vault/render";
/// Entity ids sent in one `knowledge.changed` notification at most (clients refetch anyway).
const MAX_NOTIFIED_IDS: usize = 1000;

/// Knowledge, graph and vault, shared by the worker and the IPC handlers.
pub(crate) struct KnowledgeService {
    pub knowledge: Knowledge,
    pub graph: Arc<dyn GraphStore>,
    pub vault: Option<Arc<Vault>>,
    pub store: Store,
    cfg: KnowledgeSection,
    router: Arc<Router>,
    power: Arc<dyn PowerPolicy>,
    hub: Arc<Hub>,
    /// Serialises vault passes and their high-water mark.
    vault_lock: tokio::sync::Mutex<()>,
    last_run: Mutex<Option<OffsetDateTime>>,
    /// Pokes the worker (e.g. after a source was removed).
    wake: tokio::sync::Notify,
}

/// Resolve the vault directory: `[vault] path` (with `~` expanded), else `default`.
pub fn vault_root(section: &VaultSection, default: &Path) -> Result<PathBuf> {
    match section.path.as_deref().map(str::trim) {
        None => Ok(default.to_path_buf()),
        Some(p) => {
            let expanded = if p == "~" || p.starts_with("~/") || p.starts_with("~\\") {
                let home = std::env::home_dir().ok_or_else(|| {
                    Error::Invalid(
                        "vault.path starts with ~ but the home directory is unknown".into(),
                    )
                })?;
                if p.len() > 2 {
                    home.join(&p[2..])
                } else {
                    home
                }
            } else {
                PathBuf::from(p)
            };
            if !expanded.is_absolute() {
                return Err(Error::Invalid(format!(
                    "vault.path must be absolute (or start with ~): {p}"
                )));
            }
            Ok(expanded)
        }
    }
}

/// The default vault directory for a data root: `~/Nugget Vault` for a normal install (the
/// binary passes it in), `<data>/vault` otherwise.
pub fn default_vault_dir(paths: &Paths) -> PathBuf {
    paths.data.join("vault")
}

impl KnowledgeService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Store,
        router: Arc<Router>,
        cfg: KnowledgeSection,
        vault_section: &VaultSection,
        vault_dir: PathBuf,
        power: Arc<dyn PowerPolicy>,
        hub: Arc<Hub>,
    ) -> Result<Self> {
        let graph: Arc<dyn GraphStore> = Arc::new(store.graph());
        let knowledge = Knowledge::new(
            store.clone(),
            graph.clone(),
            Some(router.clone()),
            cfg.pipeline.clone(),
        );
        let vault = if vault_section.enabled {
            let items: Arc<dyn ItemLookup> = Arc::new(store.clone());
            match Vault::new(
                &vault_dir,
                graph.clone(),
                items,
                vault_section.render.clone(),
            ) {
                Ok(v) => {
                    tracing::info!(root = %v.root().display(), "vault open");
                    Some(Arc::new(v))
                }
                Err(e) => {
                    tracing::warn!(path = %vault_dir.display(), error = %e, "vault disabled: cannot open it");
                    None
                }
            }
        } else {
            None
        };
        Ok(Self {
            knowledge,
            graph,
            vault,
            store,
            cfg,
            router,
            power,
            hub,
            vault_lock: tokio::sync::Mutex::new(()),
            last_run: Mutex::new(None),
            wake: tokio::sync::Notify::new(),
        })
    }

    /// Start the extraction worker, the change forwarder and the vault watcher (all tracked and
    /// stopped by `stop`).
    pub fn spawn(self: &Arc<Self>, ingest: &Ingest, tasks: &TaskTracker, stop: CancellationToken) {
        tasks.spawn(forward_changes(
            self.hub.clone(),
            self.knowledge.subscribe(),
            stop.clone(),
        ));
        if let Some(v) = &self.vault {
            let v = v.clone();
            let cancel = stop.child_token();
            tasks.spawn(async move {
                if let Err(e) = v.watch(cancel).await {
                    tracing::warn!(error = %e, "vault watcher stopped; edits are applied at the next render");
                }
            });
        }
        // Stop runs between items once the daemon stops, before the model runtime goes away.
        let k = self.knowledge.clone();
        let stopping = stop.clone();
        tasks.spawn(async move {
            stopping.cancelled().await;
            k.interrupt();
        });
        let me = self.clone();
        let ingest = ingest.clone();
        tasks.spawn(async move { me.worker(ingest, stop).await });
    }

    /// Ask the worker to run soon (e.g. after a source was removed, or when a local model
    /// became ready: items waiting for the model are retried).
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// The extraction loop. It sleeps until something happens: an ingest commit (watched via
    /// [`Ingest::subscribe_committed`]), a [`wake`](Self::wake), a batch of vault edits, the
    /// debounce or periodic deadline, or shutdown. Nothing polls, so an idle daemon does not
    /// wake up here at all.
    async fn worker(self: Arc<Self>, ingest: Ingest, stop: CancellationToken) {
        let mut committed = ingest.subscribe_committed();
        let mut seen = *committed.borrow_and_update();
        let mut commits_open = true;
        let mut edits = self.vault.as_ref().map(|v| v.subscribe_edits());
        let debounce = Duration::from_millis(self.cfg.debounce_ms);
        let interval = (self.cfg.interval_minutes > 0)
            .then(|| Duration::from_secs(self.cfg.interval_minutes * 60));
        // Startup run: pick up whatever was indexed while extraction was not running.
        let mut due: Option<Instant> = self.cfg.enabled.then(Instant::now);
        let mut periodic = interval.map(|i| Instant::now() + i);
        // The commit counter when the current chain of runs started. A run that cannot finish
        // its plan carries the rest over to the next run, which does not plan again, so commits
        // that arrive meanwhile are planned only by a run after the chain.
        let mut chain_from: Option<u64> = None;
        // Some run of the chain left items for the model after spending its request budget.
        let mut chain_deferred = false;
        loop {
            let deadline = if self.cfg.enabled {
                [due, periodic].into_iter().flatten().min()
            } else {
                None
            };
            let mut vault_dirty = false;
            tokio::select! {
                () = stop.cancelled() => return,
                // While a run is already scheduled, commits are only looked at when it is due
                // (below), so a burst of commits does not wake this loop once per item.
                r = committed.changed(), if self.cfg.enabled && commits_open && due.is_none() => {
                    if r.is_err() {
                        commits_open = false;
                    }
                }
                () = self.wake.notified() => {
                    vault_dirty = true;
                    due.get_or_insert_with(Instant::now);
                }
                () = edits_changed(&mut edits) => vault_dirty = true,
                () = sleep_until(deadline) => {}
            }
            if !self.cfg.enabled {
                if vault_dirty {
                    self.sync_vault_and_notify().await;
                }
                continue;
            }
            let now = Instant::now();
            let latest = *committed.borrow_and_update();
            if latest != seen {
                // More changes: (re)start the quiet period.
                seen = latest;
                due = Some(now + debounce);
            }
            if periodic.is_some_and(|p| now >= p) {
                periodic = interval.map(|i| now + i);
                if self.power.constrained() {
                    tracing::debug!("knowledge: periodic run skipped (power constrained)");
                } else {
                    due.get_or_insert(now);
                }
            }
            match due {
                Some(at) if now >= at => {
                    due = None;
                    let from = *chain_from.get_or_insert(seen);
                    match self.run_now().await {
                        Ok(stats) => {
                            chain_deferred |= deferred_for_budget(&stats);
                            if run_again(&stats, from, *committed.borrow(), chain_deferred) {
                                due = Some(Instant::now());
                            }
                            if stats.remaining == 0 {
                                chain_from = None;
                                chain_deferred = false;
                            }
                        }
                        Err(e) => {
                            chain_from = None;
                            chain_deferred = false;
                            tracing::warn!(error = %e, "knowledge run failed");
                        }
                    }
                }
                // Vault edits applied by the watcher change the graph too (a run syncs the
                // vault itself).
                _ if vault_dirty => self.sync_vault_and_notify().await,
                _ => {}
            }
        }
    }

    /// One extraction run (respecting the power policy), then a vault pass.
    pub async fn run_now(&self) -> Result<RunStats> {
        let started = std::time::Instant::now();
        let power = self.power.clone();
        let (max_items, max_llm, max_secs) = (
            self.cfg.max_items_per_run.max(1),
            self.cfg.max_llm_requests_per_run,
            self.cfg.max_run_seconds,
        );
        let k = self.knowledge.clone();
        let handle = tokio::runtime::Handle::current();
        // `run_once` makes synchronous store calls between its awaits, and the power probe may
        // read the OS: keep both off the workers.
        let (constrained, stats) = blocking(move || {
            let constrained = power.constrained();
            let budget = RunBudget {
                max_items,
                max_llm_requests: if constrained { 0 } else { max_llm },
                max_millis: (max_secs > 0).then(|| max_secs * 1000),
            };
            Ok((constrained, handle.block_on(k.run_once(budget))))
        })
        .await?;
        *self.last_run.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(OffsetDateTime::now_utc());
        let stats = stats?;
        let took_ms = started.elapsed().as_millis() as u64;
        if stats.planned > 0 {
            tracing::info!(
                processed = stats.items_processed,
                deleted = stats.items_deleted,
                failed = stats.items_failed,
                remaining = stats.remaining,
                deferred = stats.items_deferred,
                llm_requests = stats.llm_requests,
                entities = stats.changed_entities,
                constrained,
                took_ms,
                "knowledge run"
            );
        } else {
            tracing::debug!(took_ms, "knowledge run: nothing to do");
        }
        if let Err(e) = self.sync_vault().await {
            tracing::warn!(error = %e, "vault render failed");
        }
        Ok(stats)
    }

    /// Re-render vault pages of entities changed since the persisted mark. Returns the changed
    /// entity ids (at most [`MAX_NOTIFIED_IDS`]).
    pub async fn sync_vault(&self) -> Result<Vec<EntityId>> {
        let Some(vault) = self.vault.clone() else {
            return Ok(Vec::new());
        };
        let _guard = self.vault_lock.lock().await;
        let store = self.store.clone();
        let graph = self.graph.clone();
        blocking(move || {
            let root = vault.root().display().to_string();
            let mark = load_mark(&store, &root, &vault)?;
            let (peek, _) = graph.changes_since(mark, MAX_NOTIFIED_IDS)?;
            // After an upgrade that changed what pages show, render everything once even when
            // the graph has not changed (`render_changed` does the full pass itself).
            if peek.is_empty() && !vault.needs_full_render() {
                return Ok(Vec::new());
            }
            let report = vault.render_changed(mark)?;
            if report.high_water > mark {
                store.set_cursor(
                    &SourceId::new(VAULT_MARK_KEY),
                    json!({"root": root, "seq": report.high_water}),
                )?;
            }
            if !report.conflicts.is_empty() {
                tracing::info!(
                    pages = report.conflicts.len(),
                    "vault pages with unmerged edits"
                );
            }
            let mut ids: Vec<EntityId> = peek
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            ids.truncate(MAX_NOTIFIED_IDS);
            Ok(ids)
        })
        .await
    }

    /// [`sync_vault`](Self::sync_vault), broadcasting what changed (vault edits, merges).
    async fn sync_vault_and_notify(&self) {
        match self.sync_vault().await {
            Ok(ids) if !ids.is_empty() => self.notify_changed(ids),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "vault render failed"),
        }
    }

    pub fn notify_changed(&self, entities: Vec<EntityId>) {
        broadcast_changed(&self.hub, entities);
    }

    pub fn last_run(&self) -> Option<OffsetDateTime> {
        *self.last_run.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn vault_status(&self) -> VaultStatus {
        self.vault.as_ref().map(|v| v.status()).unwrap_or_default()
    }

    /// The provider of the last LLM extraction (cloud included), or before any, the local
    /// provider it would use.
    pub fn extractor_provider(&self) -> Option<nugget_core::ids::ProviderId> {
        let st = self.knowledge.status();
        if !st.llm_enabled {
            return None;
        }
        if st.llm_provider.is_some() {
            return st.llm_provider;
        }
        // Asked of the router now, not as of the last run: a model that became ready since is
        // reported right away.
        let caps = nugget_core::capability::Capabilities {
            json_schema: true,
            ..Default::default()
        };
        self.router
            .candidates(nugget_core::capability::Tier::Synthesis, &caps)
            .into_iter()
            .find(|p| p.meta().locality != nugget_core::capability::Locality::Cloud)
            .map(|p| p.id().clone())
    }
}

/// Whether the worker runs again right away after a run: the plan was cut by the budget (the
/// rest is carried over); or the chain of carried-over runs that began when the commit counter
/// was `chain_from` is over and commits arrived since (a carried plan never plans again, so
/// they are not in it); or items were left for the model because this run's request budget
/// ran out somewhere in the chain (`chain_deferred`, see [`deferred_for_budget`]).
fn run_again(stats: &RunStats, chain_from: u64, committed: u64, chain_deferred: bool) -> bool {
    stats.remaining > 0 || committed != chain_from || chain_deferred
}

/// Whether a run left items for the model because its request budget ran out while it made
/// progress with the model (so a fresh budget will get further). A run that extracted nothing
/// (every request failed, power constrained) does not count: that would loop.
fn deferred_for_budget(stats: &RunStats) -> bool {
    stats.items_deferred > 0
        && stats.llm_requests > stats.llm_failed_requests
        && stats.items_processed > 0
}

/// Sleep until `deadline`, or forever without one.
async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Resolves when the vault has applied user edits; never without a vault (or once its watcher
/// is gone).
async fn edits_changed(edits: &mut Option<tokio::sync::watch::Receiver<u64>>) {
    match edits {
        Some(rx) => {
            if rx.changed().await.is_err() {
                *edits = None;
                std::future::pending::<()>().await;
            }
        }
        None => std::future::pending().await,
    }
}

/// The persisted render mark, or 0 when the vault moved or has never been rendered (its state
/// file is gone).
fn load_mark(store: &Store, root: &str, vault: &Vault) -> Result<u64> {
    let Some(v) = store.get_cursor(&SourceId::new(VAULT_MARK_KEY))? else {
        return Ok(0);
    };
    let same_root = v.get("root").and_then(Value::as_str) == Some(root);
    let rendered = vault.status().last_render.is_some();
    Ok(match v.get("seq").and_then(Value::as_u64) {
        Some(seq) if same_root && rendered => seq,
        _ => 0,
    })
}

fn broadcast_changed(hub: &Hub, mut entities: Vec<EntityId>) {
    entities.truncate(MAX_NOTIFIED_IDS);
    let params = serde_json::to_value(KnowledgeChanged { entities }).unwrap_or(Value::Null);
    hub.broadcast(notification::KNOWLEDGE_CHANGED, params);
}

/// Forward each run's changed entities to every client.
async fn forward_changes(
    hub: Arc<Hub>,
    mut rx: broadcast::Receiver<Vec<EntityId>>,
    stop: CancellationToken,
) {
    loop {
        let next = tokio::select! {
            () = stop.cancelled() => return,
            n = rx.recv() => n,
        };
        match next {
            Ok(ids) => broadcast_changed(&hub, ids),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(missed = n, "knowledge change events lagged");
                broadcast_changed(&hub, Vec::new());
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_again_after_a_carried_chain_that_missed_commits() {
        let done = RunStats::default();
        let carried = RunStats {
            remaining: 3,
            ..RunStats::default()
        };
        // Budget cut: continue the chain.
        assert!(run_again(&carried, 7, 7, false));
        // Chain over, nothing committed since it started: sleep.
        assert!(!run_again(&done, 7, 7, false));
        // Chain over, commits arrived while it ran (QA GG5-7): plan again now.
        assert!(run_again(&done, 7, 9, false));
        // Items deferred somewhere in the chain because the request budget ran out.
        assert!(run_again(&done, 7, 7, true));
        let budget = RunStats {
            items_deferred: 2,
            items_processed: 3,
            llm_requests: 4,
            ..RunStats::default()
        };
        assert!(deferred_for_budget(&budget));
        // No progress with the model (constrained, or every request failed): no busy loop.
        let constrained = RunStats {
            items_deferred: 2,
            ..RunStats::default()
        };
        assert!(!deferred_for_budget(&constrained));
        let failing = RunStats {
            items_deferred: 2,
            items_processed: 1,
            llm_requests: 4,
            llm_failed_requests: 4,
            ..RunStats::default()
        };
        assert!(!deferred_for_budget(&failing));
    }

    #[test]
    fn vault_paths() {
        let paths = Paths::rooted(PathBuf::from("/tmp/nh"));
        let d = default_vault_dir(&paths);
        assert_eq!(d, PathBuf::from("/tmp/nh/data/vault"));
        let mut s = VaultSection::default();
        assert_eq!(vault_root(&s, &d).unwrap(), d);
        s.path = Some("relative/dir".into());
        assert!(vault_root(&s, &d).is_err());
        if let Some(home) = std::env::home_dir() {
            s.path = Some("~/Nugget Vault".into());
            assert_eq!(vault_root(&s, &d).unwrap(), home.join("Nugget Vault"));
        }
        #[cfg(unix)]
        {
            s.path = Some("/srv/vault".into());
            assert_eq!(vault_root(&s, &d).unwrap(), PathBuf::from("/srv/vault"));
        }
    }
}
