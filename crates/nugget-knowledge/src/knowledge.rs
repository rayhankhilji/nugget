//! The public handle: [`Knowledge`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nugget_core::capability::Tier;
use nugget_core::ids::{EntityId, ProviderId};
use nugget_core::{Error, Result};
use nugget_graph::GraphStore;
use nugget_infer::Router;
use nugget_store::Store;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::config::{KnowledgeConfig, RunBudget};
use crate::feed::ItemFeed;
use crate::session::{Plan, Session, plan};
use crate::splits::Split;
use crate::state::{SourceLedger, State};

/// Counters for one [`Knowledge::run_once`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunStats {
    /// Items the plan found new, changed, deleted or due for a retry.
    pub planned: u64,
    /// Items extracted (new or changed).
    pub items_processed: u64,
    /// Items checked and found unchanged.
    pub items_unchanged: u64,
    pub items_deleted: u64,
    pub items_failed: u64,
    /// Planned items left for the next run (budget exhausted).
    pub remaining: u64,
    pub entities_created: u64,
    pub entities_merged: u64,
    /// New claims (evidence added to an existing claim does not count).
    pub claims_added: u64,
    pub claims_superseded: u64,
    /// Superseded claims made active again (their successor lost its evidence).
    pub claims_reactivated: u64,
    /// Claims retracted because their last evidence went away.
    pub claims_retracted: u64,
    pub mentions_added: u64,
    /// Entities left without claims or mentions by retractions, removed (tombstoned).
    #[serde(default)]
    pub entities_pruned: u64,
    /// Wikilinks resolved into `references` claims.
    pub links_resolved: u64,
    pub llm_requests: u64,
    pub llm_failed_requests: u64,
    /// The provider that answered this run's last successful extraction request (cloud
    /// included), if any.
    #[serde(default)]
    pub llm_provider: Option<ProviderId>,
    pub llm_claims_accepted: u64,
    /// Claims dropped by the quote gate (no verbatim, relevant quote) or malformed.
    pub llm_claims_dropped: u64,
    pub llm_quotes_rejected: u64,
    /// Entities not named in the text (or malformed).
    pub llm_entities_dropped: u64,
    /// Items not sent to any model because only cloud providers exist and the item is
    /// sensitive or its source is not allow-listed.
    pub llm_skipped_privacy: u64,
    /// Entities whose data changed (also sent to [`Knowledge::subscribe`]rs).
    pub changed_entities: u64,
    pub elapsed_ms: u64,
    pub last_error: Option<String>,
}

/// What [`Knowledge::status`] reports.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeStatus {
    pub running: bool,
    pub runs: u64,
    pub last_run: Option<RunStats>,
    /// A run-level failure (store or graph unavailable); item failures are in `last_run`.
    pub last_error: Option<String>,
    /// Items processed since the pipeline started keeping state.
    pub processed_total: u64,
    /// The user's entity, once created.
    pub me: Option<EntityId>,
    pub llm_enabled: bool,
    /// Whether a synthesis provider is configured that extraction may use for at least
    /// local-only items.
    pub llm_local_available: bool,
    pub pending_links: u64,
    /// The provider that answered the most recent extraction request (cloud included), kept
    /// across runs that send none.
    #[serde(default)]
    pub llm_provider: Option<ProviderId>,
}

impl Inner {
    /// The user's splits (a snapshot; [`Knowledge::record_split`] replaces it).
    pub fn splits(&self) -> Result<Arc<Vec<Split>>> {
        let mut cached = self.splits.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(s) = cached.as_ref() {
            return Ok(s.clone());
        }
        let s = Arc::new(crate::splits::load(self.feed.as_ref())?);
        *cached = Some(s.clone());
        Ok(s)
    }
}

pub(crate) struct Inner {
    pub feed: Arc<dyn ItemFeed>,
    pub graph: Arc<dyn GraphStore>,
    pub router: Option<Arc<Router>>,
    pub cfg: KnowledgeConfig,
    pub emb_cache: Mutex<HashMap<String, Vec<f32>>>,
    /// User splits ([`crate::splits`]), loaded on first use.
    splits: Mutex<Option<Arc<Vec<Split>>>>,
    run_lock: tokio::sync::Mutex<()>,
    /// What a budget-cut run left of its plan (and whether the LLM was ready when planning),
    /// picked up by the next run instead of planning again: re-planning diffs every item
    /// against the ledger, which at scale costs more than the run's own work. Only runs (under
    /// `run_lock`) write the pipeline state, so the carried state equals what is persisted.
    carry: Mutex<Option<(bool, Plan)>>,
    status: Mutex<KnowledgeStatus>,
    tx: broadcast::Sender<Vec<EntityId>>,
}

/// The knowledge pipeline. Cheap to clone; clones share state.
///
/// ```text
/// store items ──(work queue: change log, or ledger diff)──▶ retract_item ─▶ deterministic extractors
///                                                         ─▶ LLM extractor (optional, gated)
///                                            ─▶ resolve entities ─▶ mentions, claims, provenance
///                                               (one GraphBatch per item)
///                                            ─▶ supersede / aggregate ─▶ prune orphans ─▶ subscribe()
/// ```
///
/// `run_once` performs short synchronous store and graph calls between its awaits (the plan
/// itself runs on a blocking thread); run it from its own task.
#[derive(Clone)]
pub struct Knowledge {
    inner: Arc<Inner>,
}

impl Knowledge {
    /// A pipeline over a [`Store`]'s items (state lives in the store's cursor table).
    pub fn new(
        store: Store,
        graph: Arc<dyn GraphStore>,
        router: Option<Arc<Router>>,
        config: KnowledgeConfig,
    ) -> Self {
        Self::with_feed(Arc::new(store), graph, router, config)
    }

    /// A pipeline over any [`ItemFeed`].
    pub fn with_feed(
        feed: Arc<dyn ItemFeed>,
        graph: Arc<dyn GraphStore>,
        router: Option<Arc<Router>>,
        config: KnowledgeConfig,
    ) -> Self {
        let (tx, _) = broadcast::channel(64);
        let status = KnowledgeStatus {
            llm_enabled: config.llm.enabled,
            ..Default::default()
        };
        Self {
            inner: Arc::new(Inner {
                feed,
                graph,
                router,
                cfg: config,
                emb_cache: Mutex::new(HashMap::new()),
                splits: Mutex::new(None),
                run_lock: tokio::sync::Mutex::new(()),
                carry: Mutex::new(None),
                status: Mutex::new(status),
                tx,
            }),
        }
    }

    pub fn config(&self) -> &KnowledgeConfig {
        &self.inner.cfg
    }

    /// Process changed and deleted items within `budget`. Idempotent and resumable: progress
    /// is persisted as items complete, and an interrupted item is simply redone. Concurrent
    /// calls run one after the other.
    pub async fn run_once(&self, budget: RunBudget) -> Result<RunStats> {
        let _guard = self.inner.run_lock.lock().await;
        self.set_status(|s| s.running = true);
        let res = self.run(budget).await;
        let local = self.llm_local_available();
        self.set_status(|s| {
            s.running = false;
            s.runs += 1;
            s.llm_local_available = local;
            match &res {
                Ok((stats, state)) => {
                    s.last_run = Some(stats.clone());
                    s.last_error = None;
                    s.processed_total = state.processed_total;
                    s.me = state.me.clone();
                    s.pending_links = state.pending_links.len() as u64;
                    if stats.llm_provider.is_some() {
                        s.llm_provider = stats.llm_provider.clone();
                    }
                }
                Err(e) => s.last_error = Some(e.to_string()),
            }
        });
        res.map(|(stats, _)| stats)
    }

    async fn run(&self, budget: RunBudget) -> Result<(RunStats, State)> {
        let started = Instant::now();
        let deadline = budget
            .max_millis
            .map(|ms| started + Duration::from_millis(ms));
        let llm_ready = self.inner.cfg.llm.enabled
            && self.inner.router.as_ref().is_some_and(|r| {
                !r.candidates(Tier::Synthesis, &crate::llm::json_caps())
                    .is_empty()
            });
        // Taken, so a failed run leaves nothing behind and the next one plans afresh.
        let carried = self
            .inner
            .carry
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .filter(|(ready, _)| *ready == llm_ready);
        let mut p = match carried {
            Some((_, p)) => p,
            None => {
                let feed = self.inner.feed.clone();
                let attempts = self.inner.cfg.llm.max_attempts;
                tokio::task::spawn_blocking(move || plan(feed.as_ref(), llm_ready, attempts))
                    .await
                    .map_err(|e| Error::Other(format!("knowledge plan task failed: {e}")))??
            }
        };
        let total = (p.deletes.len() + p.upserts.len()) as u64;
        let mut s = Session::new(
            &self.inner,
            p.state,
            p.notes,
            p.ledgers,
            budget.max_llm_requests,
        );
        s.stats.planned = total;
        let (mut deletes_done, mut upserts_done) = (0usize, 0usize);
        let mut spent = 0usize;
        for item in &p.deletes {
            if spent >= budget.max_items || s.out_of_time(deadline) {
                break;
            }
            s.process_delete(item)?;
            spent += 1;
            deletes_done += 1;
        }
        for w in &p.upserts {
            if spent >= budget.max_items || s.out_of_time(deadline) {
                break;
            }
            if s.process_upsert(w).await? {
                spent += 1;
            }
            upserts_done += 1;
        }
        let done = (deletes_done + upserts_done) as u64;
        s.resolve_pending_links()?;
        if s.retracted_any {
            // Retractions may leave entities with nothing behind them. Every batch is applied
            // by now, so whatever is orphaned really is.
            let pruned = self.inner.graph.prune_orphans(None)?;
            s.stats.entities_pruned += pruned.len() as u64;
            s.touched.extend(pruned);
        }
        s.stats.remaining = total - done;
        if s.stats.remaining == 0
            && let Some(c) = p.next_cursor
        {
            // Everything up to `c` is processed; a budget cut keeps the old cursor, so the next
            // run plans the rest again (processed items are then seen as unchanged).
            s.state.item_cursor = Some(c);
        }
        s.flush()?;
        s.stats.changed_entities = s.touched.len() as u64;
        s.stats.elapsed_ms = started.elapsed().as_millis() as u64;
        if !s.touched.is_empty() {
            let _ = self.inner.tx.send(s.touched.iter().cloned().collect());
        }
        let Session {
            state,
            notes,
            ledgers,
            stats,
            ..
        } = s;
        if stats.remaining > 0 {
            p.deletes.drain(..deletes_done);
            p.upserts.drain(..upserts_done);
            let rest = Plan {
                state: state.clone(),
                notes,
                ledgers,
                ..p
            };
            *self.inner.carry.lock().unwrap_or_else(|p| p.into_inner()) = Some((llm_ready, rest));
        }
        Ok((stats, state))
    }

    /// Current status (cheap).
    pub fn status(&self) -> KnowledgeStatus {
        self.inner
            .status
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Entity ids changed by each run (sent once per run that changed something).
    pub fn subscribe(&self) -> broadcast::Receiver<Vec<EntityId>> {
        self.inner.tx.subscribe()
    }

    /// Forget which items were processed, so the next runs re-extract everything (e.g. after
    /// enabling the LLM extractor or changing the configuration). The graph is kept;
    /// re-extraction replaces each item's evidence.
    pub async fn reset(&self) -> Result<()> {
        let _guard = self.inner.run_lock.lock().await;
        self.inner
            .carry
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        let feed = self.inner.feed.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut state = State::load(feed.as_ref())?;
            for source in state.sources.clone() {
                let mut l = SourceLedger::load(feed.as_ref(), &source)?;
                l.clear();
                l.flush(feed.as_ref(), &source)?;
            }
            state.sources.clear();
            state.item_cursor = None;
            state.save(feed.as_ref())
        })
        .await
        .map_err(|e| Error::Other(format!("knowledge reset task failed: {e}")))?
    }

    /// Remember that the user split `new` off `from` (call right after `GraphStore::split`), so
    /// re-extraction never merges them back together, and the occurrences the split moved (by
    /// item, or by a name only `new` has) keep resolving to `new` with their facts. Kept until
    /// the user merges the two again or one of them is gone; [`Knowledge::reset`] and extractor
    /// upgrades keep it. Blocking (store writes): call it off the async runtime.
    pub fn record_split(&self, from: &EntityId, new: &EntityId) -> Result<()> {
        let Some(split) = crate::splits::describe(self.inner.graph.as_ref(), from, new)? else {
            return Err(Error::NotFound(format!(
                "split of {new} off {from}: an entity is missing"
            )));
        };
        let mut cached = self.inner.splits.lock().unwrap_or_else(|p| p.into_inner());
        let mut list: Vec<Split> = match cached.as_ref() {
            Some(s) => s.as_ref().clone(),
            None => crate::splits::load(self.inner.feed.as_ref())?,
        };
        list.retain(|s| !(s.from == split.from && s.to == split.to));
        list.push(split);
        let excess = list.len().saturating_sub(crate::splits::MAX_SPLITS);
        list.drain(..excess);
        crate::splits::save(self.inner.feed.as_ref(), &list)?;
        *cached = Some(Arc::new(list));
        Ok(())
    }

    fn llm_local_available(&self) -> bool {
        self.inner.cfg.llm.enabled
            && self.inner.router.as_ref().is_some_and(|r| {
                !crate::llm::local_providers(r, Tier::Synthesis, &crate::llm::json_caps())
                    .is_empty()
            })
    }

    fn set_status(&self, f: impl FnOnce(&mut KnowledgeStatus)) {
        f(&mut self.inner.status.lock().unwrap_or_else(|p| p.into_inner()));
    }
}
