//! The public [`Ingest`] handle and its shared state.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use nugget_connector::{
    AuthFlow, CancellationToken, Connector, ConnectorContext, KindInfo, Registry, SecretStore,
};
use nugget_core::egress::EgressSink;
use nugget_core::ids::{ItemKey, SourceId};
use nugget_core::ipc::{
    IngestProgress, IngestStatus, SourceInfo, SourceSpec, SourceState, SourceUpdate,
};
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use nugget_infer::Router;
use nugget_store::{SourceCounts, Store};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::sync::{Semaphore, broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio_util::task::TaskTracker;

use crate::ai;
use crate::config::IngestConfig;
use crate::known::{StoreKnownItems, join_err};
use crate::metrics::Metrics;
use crate::power::{PowerPolicy, SystemPowerPolicy};
use crate::secrets::{KeyringSecrets, ScopedSecrets, account_belongs_to, default_accounts};
use crate::sources::{self, SourceEntry};
use crate::supervisor::{Control, run_source};

/// Capacity of the event broadcast; slow subscribers miss old events (`RecvError::Lagged`).
const EVENT_CAPACITY: usize = 1024;

/// How long an interactive authorization ([`Ingest::begin_auth`]) may take before it is
/// abandoned (its loopback listener closed).
pub const AUTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Adds values that must never be persisted (for example an OAuth client secret read from the
/// keychain) to a source's settings right before its connector is built. Called with the source
/// id, its kind and its persisted settings; returns the settings to build with. The result is
/// only handed to [`nugget_connector::ConnectorFactory::build`], never stored or reported.
pub type SettingsResolver =
    Arc<dyn Fn(&SourceId, &str, serde_json::Value) -> Result<serde_json::Value> + Send + Sync>;

/// Something that changed, for UIs. `nuggetd` forwards these as notifications:
/// [`Progress`](Self::Progress) → `ingest.progress`, the others → `sources.changed`.
#[derive(Debug, Clone, PartialEq)]
pub enum IngestEvent {
    Progress(IngestProgress),
    /// A source was added or its configuration, state, counts or errors changed.
    SourceChanged(SourceInfo),
    /// A source was removed.
    SourceRemoved(SourceId),
}

/// One recorded failure: a sync pass (`key: None`) or a single item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemError {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub key: Option<ItemKey>,
    pub message: String,
}

#[derive(Debug)]
struct SourceStatus {
    state: SourceState,
    last_sync: Option<OffsetDateTime>,
    last_error: Option<String>,
    errors: VecDeque<ItemError>,
    paused: bool,
}

impl SourceStatus {
    fn new(enabled: bool) -> Self {
        Self {
            state: if enabled {
                SourceState::Idle
            } else {
                SourceState::Disabled
            },
            last_sync: None,
            last_error: None,
            errors: VecDeque::new(),
            paused: false,
        }
    }
}

struct Running {
    cancel: CancellationToken,
    ctl: Arc<Control>,
    join: JoinHandle<()>,
}

struct Slot {
    entry: SourceEntry,
    status: SourceStatus,
    running: Option<Running>,
    connector: Option<Arc<dyn Connector>>,
    /// Set while `update` / `remove` reconfigure the source: nothing may (re)start it.
    reconfiguring: bool,
}

/// State shared by the API, supervisors and pipeline workers.
pub(crate) struct Inner {
    pub cfg: IngestConfig,
    pub store: Store,
    pub router: Option<Arc<Router>>,
    pub power: Arc<dyn PowerPolicy>,
    pub metrics: Metrics,
    pub extract_slots: Arc<Semaphore>,
    registry: Registry,
    paths: Paths,
    secrets: Arc<dyn SecretStore>,
    settings_resolver: Option<SettingsResolver>,
    events: broadcast::Sender<IngestEvent>,
    slots: Mutex<BTreeMap<SourceId, Slot>>,
    /// Serializes registry mutations (add / update / remove).
    ops: tokio::sync::Mutex<()>,
    /// Serializes writes of `sources.toml`.
    file_lock: Mutex<()>,
    sources_file: PathBuf,
    shutdown: CancellationToken,
    tracker: TaskTracker,
    weak: Weak<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Inner {
    pub fn emit(&self, event: IngestEvent) {
        let _ = self.events.send(event);
    }

    pub fn record_item_error(&self, id: &SourceId, key: Option<ItemKey>, message: String) {
        let cap = self.cfg.error_ring.max(1);
        let mut slots = lock(&self.slots);
        if let Some(s) = slots.get_mut(id) {
            if s.status.errors.len() >= cap {
                s.status.errors.pop_front();
            }
            s.status.errors.push_back(ItemError {
                at: OffsetDateTime::now_utc(),
                key,
                message,
            });
        }
    }

    pub fn set_paused(&self, id: &SourceId, paused: bool) {
        if let Some(s) = lock(&self.slots).get_mut(id) {
            s.status.paused = paused;
        }
    }

    pub async fn set_state(&self, id: &SourceId, state: SourceState) {
        let changed = {
            let mut slots = lock(&self.slots);
            match slots.get_mut(id) {
                Some(s) if s.status.state != state => {
                    s.status.state = state;
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.emit_source(id).await;
        }
    }

    pub async fn sync_succeeded(&self, id: &SourceId) {
        if let Some(s) = lock(&self.slots).get_mut(id) {
            s.status.last_sync = Some(OffsetDateTime::now_utc());
            s.status.last_error = None;
        }
        self.emit_source(id).await;
    }

    pub async fn sync_failed(&self, id: &SourceId, state: SourceState, message: String) {
        if let Some(s) = lock(&self.slots).get_mut(id) {
            s.status.state = state;
            s.status.last_error = Some(message);
        }
        self.emit_source(id).await;
    }

    async fn emit_source(&self, id: &SourceId) {
        if let Ok(info) = self.source_info(id).await {
            self.emit(IngestEvent::SourceChanged(info));
        }
    }

    async fn counts(&self) -> Result<Vec<SourceCounts>> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.source_counts())
            .await
            .map_err(join_err)?
    }

    async fn source_info(&self, id: &SourceId) -> Result<SourceInfo> {
        let counts = self.counts().await.unwrap_or_default();
        let slots = lock(&self.slots);
        let slot = slots
            .get(id)
            .ok_or_else(|| Error::NotFound(format!("source `{id}`")))?;
        Ok(info_of(slot, counts.iter().find(|c| c.source == *id)))
    }

    /// Persist the current entries (blocking).
    fn persist(&self) -> Result<()> {
        let _g = lock(&self.file_lock);
        let entries: Vec<SourceEntry> = lock(&self.slots)
            .values()
            .map(|s| s.entry.clone())
            .collect();
        sources::save(&self.sources_file, &entries)
    }

    async fn persist_async(&self) -> Result<()> {
        let me = self.arc()?;
        tokio::task::spawn_blocking(move || me.persist())
            .await
            .map_err(join_err)?
    }

    fn arc(&self) -> Result<Arc<Inner>> {
        self.weak
            .upgrade()
            .ok_or_else(|| Error::Other("ingest is gone".into()))
    }

    /// Remember a keychain account written for `id` (names only), so removal can delete it.
    fn note_secret_account(&self, id: &SourceId, account: &str) {
        let added = {
            let mut slots = lock(&self.slots);
            match slots.get_mut(id) {
                Some(s) if !s.entry.secret_accounts.iter().any(|a| a == account) => {
                    s.entry.secret_accounts.push(account.to_owned());
                    true
                }
                _ => false,
            }
        };
        if added && let Err(e) = self.persist() {
            tracing::warn!(source = %id, error = %e, "could not record keychain account name");
        }
    }

    fn context(&self, id: &SourceId) -> ConnectorContext {
        let weak = self.weak.clone();
        ConnectorContext {
            source: id.clone(),
            paths: self.paths.clone(),
            egress: Arc::new(self.store.clone()) as Arc<dyn EgressSink>,
            secrets: Arc::new(ScopedSecrets {
                source: id.clone(),
                inner: self.secrets.clone(),
                on_written: Arc::new(move |source, account| {
                    if let Some(inner) = weak.upgrade() {
                        inner.note_secret_account(source, account);
                    }
                }),
            }),
            known: Arc::new(StoreKnownItems::new(self.store.clone(), id.clone())),
        }
    }

    /// The source's connector, built (and cached) on first use.
    fn connector(&self, id: &SourceId) -> Result<Arc<dyn Connector>> {
        let (kind, settings) = {
            let slots = lock(&self.slots);
            let s = slots
                .get(id)
                .ok_or_else(|| Error::NotFound(format!("source `{id}`")))?;
            if let Some(c) = &s.connector {
                return Ok(c.clone());
            }
            (s.entry.kind.clone(), s.entry.settings.clone())
        };
        let factory = self.registry.get(&kind)?;
        let settings = match &self.settings_resolver {
            Some(resolve) => resolve(id, &kind, settings)?,
            None => settings,
        };
        let connector = factory.build(&settings, self.context(id))?;
        if let Some(s) = lock(&self.slots).get_mut(id) {
            s.connector = Some(connector.clone());
        }
        Ok(connector)
    }

    /// Start the supervisor of an enabled source (no-op if running or shutting down).
    fn start_source(&self, id: &SourceId) {
        if self.shutdown.is_cancelled() {
            return;
        }
        {
            let slots = lock(&self.slots);
            match slots.get(id) {
                Some(s) if s.entry.enabled && s.running.is_none() && !s.reconfiguring => {}
                _ => return,
            }
        }
        let connector = match self.connector(id) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(source = %id, error = %e, "cannot start source");
                if let Some(s) = lock(&self.slots).get_mut(id) {
                    s.status.state = SourceState::Error;
                    s.status.last_error = Some(e.to_string());
                }
                return;
            }
        };
        let Ok(me) = self.arc() else { return };
        let ctl = Arc::new(Control::default());
        let cancel = self.shutdown.child_token();
        let mut slots = lock(&self.slots);
        let Some(slot) = slots.get_mut(id) else {
            return;
        };
        if slot.running.is_some() || slot.reconfiguring || !slot.entry.enabled {
            return;
        }
        let join = self.tracker.spawn(run_source(
            me,
            id.clone(),
            connector,
            ctl.clone(),
            cancel.clone(),
        ));
        slot.status.state = SourceState::Idle;
        slot.running = Some(Running { cancel, ctl, join });
    }

    /// Stop a source's supervisor and wait for it (and its worker) to exit.
    async fn stop_source(&self, id: &SourceId) {
        let running = lock(&self.slots).get_mut(id).and_then(|s| s.running.take());
        if let Some(r) = running {
            r.cancel.cancel();
            let _ = r.join.await;
        }
    }

    fn set_reconfiguring(&self, id: &SourceId, on: bool) {
        if let Some(s) = lock(&self.slots).get_mut(id) {
            s.reconfiguring = on;
        }
    }

    fn control(&self, id: &SourceId) -> Option<Arc<Control>> {
        lock(&self.slots)
            .get(id)
            .and_then(|s| s.running.as_ref().map(|r| r.ctl.clone()))
    }
}

fn info_of(slot: &Slot, counts: Option<&SourceCounts>) -> SourceInfo {
    SourceInfo {
        id: slot.entry.id.clone(),
        kind: slot.entry.kind.clone(),
        name: slot.entry.name.clone(),
        enabled: slot.entry.enabled,
        state: if slot.entry.enabled {
            slot.status.state
        } else {
            SourceState::Disabled
        },
        items: counts.map_or(0, |c| c.items),
        chunks: counts.map_or(0, |c| c.chunks),
        last_sync: slot.status.last_sync,
        last_error: slot.status.last_error.clone(),
        settings: slot.entry.settings.clone(),
    }
}

/// Builder for [`Ingest`] with injectable secrets and power policy (tests, headless runs).
pub struct IngestBuilder {
    cfg: IngestConfig,
    store: Store,
    registry: Registry,
    paths: Paths,
    router: Option<Arc<Router>>,
    secrets: Option<Arc<dyn SecretStore>>,
    power: Option<Arc<dyn PowerPolicy>>,
    settings_resolver: Option<SettingsResolver>,
}

impl std::fmt::Debug for IngestBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IngestBuilder")
            .field("cfg", &self.cfg)
            .field("registry", &self.registry)
            .finish_non_exhaustive()
    }
}

impl IngestBuilder {
    /// Model router for triage and embeddings. Without one, items are indexed text-only.
    pub fn router(mut self, router: Option<Arc<Router>>) -> Self {
        self.router = router;
        self
    }

    /// Credential store. Default: the OS keychain ([`KeyringSecrets`]).
    pub fn secrets(mut self, secrets: Arc<dyn SecretStore>) -> Self {
        self.secrets = Some(secrets);
        self
    }

    /// Power policy. Default: [`SystemPowerPolicy`] at `cfg.low_battery_threshold`.
    pub fn power(mut self, power: Arc<dyn PowerPolicy>) -> Self {
        self.power = Some(power);
        self
    }

    /// Settings hook applied each time a connector is built (see [`SettingsResolver`]).
    /// After the values it adds change, call [`Ingest::reload`] to rebuild the connector.
    pub fn settings_resolver(mut self, resolver: SettingsResolver) -> Self {
        self.settings_resolver = Some(resolver);
        self
    }

    /// Load `<config>/sources.toml` and start a supervisor for every enabled source.
    /// Must be called inside a Tokio runtime.
    pub async fn start(self) -> Result<Ingest> {
        let file = sources::path(&self.paths.config);
        let f2 = file.clone();
        let entries = tokio::task::spawn_blocking(move || sources::load(&f2))
            .await
            .map_err(join_err)??;
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let power = self
            .power
            .unwrap_or_else(|| Arc::new(SystemPowerPolicy::new(self.cfg.low_battery_threshold)));
        let secrets = self
            .secrets
            .unwrap_or_else(|| Arc::new(KeyringSecrets::new()));
        let cfg = self.cfg;
        let inner = Arc::new_cyclic(|weak| Inner {
            metrics: Metrics::new(cfg.freshness_window),
            extract_slots: Arc::new(Semaphore::new(cfg.extraction_slots())),
            cfg,
            store: self.store,
            router: self.router,
            power,
            registry: self.registry,
            paths: self.paths,
            secrets,
            settings_resolver: self.settings_resolver,
            events,
            slots: Mutex::new(BTreeMap::new()),
            ops: tokio::sync::Mutex::new(()),
            file_lock: Mutex::new(()),
            sources_file: file,
            shutdown: CancellationToken::new(),
            tracker: TaskTracker::new(),
            weak: weak.clone(),
        });
        {
            let mut slots = lock(&inner.slots);
            for e in entries {
                let status = SourceStatus::new(e.enabled);
                slots.insert(
                    e.id.clone(),
                    Slot {
                        entry: e,
                        status,
                        running: None,
                        connector: None,
                        reconfiguring: false,
                    },
                );
            }
        }
        let ids: Vec<SourceId> = lock(&inner.slots).keys().cloned().collect();
        for id in &ids {
            inner.start_source(id);
        }
        Ok(Ingest { inner })
    }
}

/// The ingest runtime: source registry, one supervisor per enabled source, and the indexing
/// pipeline. Cheap to clone; all clones share one runtime. Call [`shutdown`](Self::shutdown)
/// before dropping the last clone.
///
/// ```no_run
/// # async fn demo(store: nugget_store::Store, registry: nugget_connector::Registry,
/// #               paths: nugget_core::paths::Paths) -> nugget_core::Result<()> {
/// use nugget_ingest::{Ingest, IngestConfig};
/// let ingest = Ingest::start(IngestConfig::default(), store, None, registry, paths).await?;
/// let mut events = ingest.subscribe();
/// // ... wire `ingest.list()`, `ingest.add(spec)`, `ingest.status()` to IPC ...
/// ingest.shutdown().await;
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct Ingest {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Ingest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = lock(&self.inner.slots).len();
        f.debug_struct("Ingest")
            .field("sources", &n)
            .finish_non_exhaustive()
    }
}

impl Ingest {
    /// Start with the OS keychain and the system power policy. See [`Ingest::builder`].
    pub async fn start(
        cfg: IngestConfig,
        store: Store,
        router: Option<Arc<Router>>,
        registry: Registry,
        paths: Paths,
    ) -> Result<Self> {
        Self::builder(cfg, store, registry, paths)
            .router(router)
            .start()
            .await
    }

    pub fn builder(
        cfg: IngestConfig,
        store: Store,
        registry: Registry,
        paths: Paths,
    ) -> IngestBuilder {
        IngestBuilder {
            cfg,
            store,
            registry,
            paths,
            router: None,
            secrets: None,
            power: None,
            settings_resolver: None,
        }
    }

    /// Connector kinds available in this build (`sources.kinds` for UIs).
    pub fn kinds(&self) -> Vec<KindInfo> {
        self.inner.registry.kinds()
    }

    /// Add a source (`sources.add`). Settings are validated by the kind's factory and must not
    /// contain secrets. Without an id, one is generated as `<kind>-<n>`. The source starts
    /// enabled and syncing immediately.
    pub async fn add(&self, spec: SourceSpec) -> Result<SourceInfo> {
        let inner = &self.inner;
        let _g = inner.ops.lock().await;
        let factory = inner.registry.get(&spec.kind)?.clone();
        let settings = sources::normalize_settings(spec.settings)?;
        factory.validate(&settings)?;
        let id = match spec.id {
            Some(id) => {
                sources::validate_id(&id)?;
                if lock(&inner.slots).contains_key(&id) {
                    return Err(Error::Invalid(format!("source `{id}` already exists")));
                }
                id
            }
            None => {
                let counts = inner.counts().await?;
                let prefix: String = spec
                    .kind
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
                    .take(48)
                    .collect();
                let prefix = if prefix.is_empty()
                    || !prefix.starts_with(|c: char| c.is_ascii_alphanumeric())
                {
                    "source".to_owned()
                } else {
                    prefix
                };
                let slots = lock(&inner.slots);
                (1u64..)
                    .map(|n| SourceId::new(format!("{prefix}-{n}")))
                    .find(|id| !slots.contains_key(id) && !counts.iter().any(|c| c.source == *id))
                    .unwrap_or_else(|| SourceId::new(prefix))
            }
        };
        let name = match spec.name.trim() {
            "" => factory.info().display_name.to_owned(),
            n => n.to_owned(),
        };
        let entry = SourceEntry {
            id: id.clone(),
            kind: spec.kind,
            name,
            enabled: true,
            settings,
            secret_accounts: Vec::new(),
        };
        lock(&inner.slots).insert(
            id.clone(),
            Slot {
                entry,
                status: SourceStatus::new(true),
                running: None,
                connector: None,
                reconfiguring: false,
            },
        );
        if let Err(e) = inner.persist_async().await {
            lock(&inner.slots).remove(&id);
            return Err(e);
        }
        inner.start_source(&id);
        let info = inner.source_info(&id).await?;
        inner.emit(IngestEvent::SourceChanged(info.clone()));
        Ok(info)
    }

    /// Change name, settings and/or enabled state (`sources.update`). New settings are
    /// validated, rebuild the connector and reset the cursor so the next pass reconciles
    /// everything against the new settings.
    pub async fn update(&self, update: SourceUpdate) -> Result<SourceInfo> {
        let inner = &self.inner;
        let _g = inner.ops.lock().await;
        let id = update.id;
        let old = lock(&inner.slots)
            .get(&id)
            .map(|s| s.entry.clone())
            .ok_or_else(|| Error::NotFound(format!("source `{id}`")))?;
        let mut new = old.clone();
        if let Some(name) = update.name {
            let name = name.trim();
            if name.is_empty() {
                return Err(Error::Invalid("source name must not be empty".into()));
            }
            new.name = name.to_owned();
        }
        if let Some(settings) = update.settings {
            let settings = sources::normalize_settings(settings)?;
            inner.registry.get(&new.kind)?.validate(&settings)?;
            new.settings = settings;
        }
        if let Some(enabled) = update.enabled {
            new.enabled = enabled;
        }
        let settings_changed = new.settings != old.settings;
        let enabled_changed = new.enabled != old.enabled;

        let reconfigure = settings_changed || enabled_changed;
        if reconfigure {
            inner.set_reconfiguring(&id, true);
            inner.stop_source(&id).await;
        }
        let result = self
            .apply_update(&id, &old, &new, settings_changed, enabled_changed)
            .await;
        if reconfigure {
            inner.set_reconfiguring(&id, false);
        }
        let start = match &result {
            Ok(()) => new.enabled,
            Err(_) => old.enabled,
        };
        if start {
            inner.start_source(&id);
        }
        result?;
        let info = inner.source_info(&id).await?;
        inner.emit(IngestEvent::SourceChanged(info.clone()));
        Ok(info)
    }

    async fn apply_update(
        &self,
        id: &SourceId,
        old: &SourceEntry,
        new: &SourceEntry,
        settings_changed: bool,
        enabled_changed: bool,
    ) -> Result<()> {
        let inner = &self.inner;
        if let Some(s) = lock(&inner.slots).get_mut(id) {
            s.entry = new.clone();
            if settings_changed {
                s.connector = None;
            }
            if !new.enabled {
                s.status.state = SourceState::Disabled;
                s.status.paused = false;
            } else if enabled_changed || settings_changed {
                s.status.state = SourceState::Idle;
                s.status.last_error = None;
            }
        }
        if let Err(e) = inner.persist_async().await {
            if let Some(s) = lock(&inner.slots).get_mut(id) {
                s.entry = old.clone();
                s.status.state = if old.enabled {
                    SourceState::Idle
                } else {
                    SourceState::Disabled
                };
            }
            return Err(e);
        }
        if settings_changed {
            // Start over so the next pass reconciles everything against the new settings.
            let store = inner.store.clone();
            let sid = id.clone();
            tokio::task::spawn_blocking(move || store.set_cursor(&sid, serde_json::Value::Null))
                .await
                .map_err(join_err)??;
        }
        Ok(())
    }

    /// Enable or disable a source.
    pub async fn set_enabled(&self, id: &SourceId, enabled: bool) -> Result<SourceInfo> {
        self.update(SourceUpdate {
            id: id.clone(),
            name: None,
            enabled: Some(enabled),
            settings: None,
        })
        .await
    }

    /// Remove a source (`sources.remove`): stop it, forget it, and delete its keychain entries
    /// (`oauth:<id>`, `password:<id>` and any other account it wrote). With `purge`, also
    /// delete everything indexed from it and its cursor.
    pub async fn remove(&self, id: &SourceId, purge: bool) -> Result<()> {
        let inner = &self.inner;
        let _g = inner.ops.lock().await;
        if !lock(&inner.slots).contains_key(id) {
            return Err(Error::NotFound(format!("source `{id}`")));
        }
        inner.set_reconfiguring(id, true);
        inner.stop_source(id).await;
        let Some(mut slot) = lock(&inner.slots).remove(id) else {
            return Err(Error::NotFound(format!("source `{id}`")));
        };
        if let Err(e) = inner.persist_async().await {
            slot.reconfiguring = false;
            lock(&inner.slots).insert(id.clone(), slot);
            inner.start_source(id);
            return Err(e);
        }
        let mut accounts: Vec<String> = default_accounts(id).into();
        for a in &slot.entry.secret_accounts {
            if !accounts.contains(a) && account_belongs_to(a, id) {
                accounts.push(a.clone());
            }
        }
        let secrets = inner.secrets.clone();
        let sid = id.clone();
        let _ = tokio::task::spawn_blocking(move || {
            for a in accounts {
                if let Err(e) = secrets.delete(&a) {
                    tracing::warn!(source = %sid, account = %a, error = %e, "could not delete keychain entry");
                }
            }
        })
        .await;
        if purge {
            let store = inner.store.clone();
            let sid = id.clone();
            let n = tokio::task::spawn_blocking(move || store.delete_source(&sid))
                .await
                .map_err(join_err)??;
            tracing::info!(source = %id, items = n, "purged source");
        }
        inner.emit(IngestEvent::SourceRemoved(id.clone()));
        Ok(())
    }

    /// All sources with counts, state, last sync and last error (`sources.list`).
    pub async fn list(&self) -> Result<Vec<SourceInfo>> {
        let counts = self.inner.counts().await?;
        let slots = lock(&self.inner.slots);
        Ok(slots
            .values()
            .map(|s| info_of(s, counts.iter().find(|c| c.source == s.entry.id)))
            .collect())
    }

    /// One source.
    pub async fn get(&self, id: &SourceId) -> Result<SourceInfo> {
        self.inner.source_info(id).await
    }

    /// Run a sync pass now (`sources.sync`): one source, or every enabled source with `None`.
    /// Also lifts `NeedsAuth` and error backoff, and lets a power-paused backfill proceed.
    /// A source that failed to start (bad settings, unknown kind) is retried.
    pub fn sync_now(&self, id: Option<&SourceId>) -> Result<()> {
        let inner = &self.inner;
        let ids: Vec<SourceId> = match id {
            Some(id) => {
                let slots = lock(&inner.slots);
                let s = slots
                    .get(id)
                    .ok_or_else(|| Error::NotFound(format!("source `{id}`")))?;
                if !s.entry.enabled {
                    return Err(Error::Invalid(format!("source `{id}` is disabled")));
                }
                vec![id.clone()]
            }
            None => lock(&inner.slots)
                .values()
                .filter(|s| s.entry.enabled)
                .map(|s| s.entry.id.clone())
                .collect(),
        };
        for id in ids {
            match inner.control(&id) {
                Some(ctl) => ctl.sync_now(),
                None => inner.start_source(&id),
            }
        }
        Ok(())
    }

    /// Queue depth, freshness percentiles, providers in use and every source (`ingest.status`).
    pub async fn status(&self) -> Result<IngestStatus> {
        let (p50, p95) = self.inner.metrics.freshness();
        let (embedding_provider, triage_provider) = match &self.inner.router {
            Some(r) => (
                if self.inner.cfg.embeddings {
                    ai::embedding_providers(r, false, self.inner.cfg.allow_cloud_embeddings)
                        .first()
                        .map(|p| p.id().clone())
                } else {
                    None
                },
                if self.inner.cfg.triage {
                    ai::triage_provider(r).map(|p| p.id().clone())
                } else {
                    None
                },
            ),
            None => (None, None),
        };
        Ok(IngestStatus {
            queue_depth: self.inner.metrics.queue_depth(),
            freshness_p50_ms: p50,
            freshness_p95_ms: p95,
            embedding_provider,
            triage_provider,
            sources: self.list().await?,
        })
    }

    /// Progress and source-change events. Each subscriber gets every event from now on (up to
    /// a buffer of 1024; a lagging subscriber sees `RecvError::Lagged`).
    pub fn subscribe(&self) -> broadcast::Receiver<IngestEvent> {
        self.inner.events.subscribe()
    }

    /// Recent failures of a source (sync passes and single items), oldest first.
    pub fn errors(&self, id: &SourceId) -> Result<Vec<ItemError>> {
        lock(&self.inner.slots)
            .get(id)
            .map(|s| s.status.errors.iter().cloned().collect())
            .ok_or_else(|| Error::NotFound(format!("source `{id}`")))
    }

    /// Whether a source's backfill is currently paused by the power policy.
    pub fn is_paused(&self, id: &SourceId) -> bool {
        lock(&self.inner.slots)
            .get(id)
            .is_some_and(|s| s.status.paused)
    }

    /// Changes committed since start (applied upserts and deletes; skips excluded).
    pub fn committed(&self) -> u64 {
        self.inner.metrics.committed()
    }

    /// Start interactive authorization (`sources.auth.begin`). Show `url` to the user. The
    /// completion is also handled here: on success the source leaves `NeedsAuth`, re-syncs and a
    /// [`IngestEvent::SourceChanged`] is emitted; awaiting `done` is optional.
    pub async fn begin_auth(&self, id: &SourceId) -> Result<AuthFlow> {
        let inner = self.inner.clone();
        let connector = {
            let inner = inner.clone();
            let id = id.clone();
            tokio::task::spawn_blocking(move || inner.connector(&id))
                .await
                .map_err(join_err)??
        };
        let flow = connector.begin_auth().await?;
        let (tx, rx) = oneshot::channel();
        let id = id.clone();
        let done = flow.done;
        let task_inner = inner.clone();
        inner.tracker.spawn(async move {
            let inner = task_inner;
            // Dropping `done` (timeout, shutdown) aborts the flow and closes its listener.
            let r = tokio::select! {
                r = tokio::time::timeout(AUTH_TIMEOUT, done) => r.unwrap_or_else(|_| {
                    Err(Error::Other(format!(
                        "authorization timed out after {} s",
                        AUTH_TIMEOUT.as_secs()
                    )))
                }),
                _ = inner.shutdown.cancelled() => Err(Error::Cancelled),
            };
            match &r {
                Ok(()) => {
                    tracing::info!(source = %id, "authorization completed");
                    if let Some(s) = lock(&inner.slots).get_mut(&id)
                        && s.status.state == SourceState::NeedsAuth
                    {
                        s.status.last_error = None;
                    }
                    match inner.control(&id) {
                        Some(ctl) => ctl.sync_now(),
                        None => inner.start_source(&id),
                    }
                    inner.emit_source(&id).await;
                }
                Err(e) => {
                    inner.record_item_error(&id, None, format!("authorization failed: {e}"));
                    inner.emit_source(&id).await;
                }
            }
            let _ = tx.send(r);
        });
        Ok(AuthFlow {
            url: flow.url,
            done: Box::pin(async move { rx.await.unwrap_or(Err(Error::Cancelled)) }),
        })
    }

    /// Store a credential for a source in the keychain and re-sync it. `account` must belong to
    /// the source (`<prefix>:<id>`, e.g. [`nugget_core::secrets::password_account`]).
    pub async fn set_secret(&self, id: &SourceId, account: &str, value: &str) -> Result<()> {
        let inner = &self.inner;
        if !lock(&inner.slots).contains_key(id) {
            return Err(Error::NotFound(format!("source `{id}`")));
        }
        if !account_belongs_to(account, id) {
            return Err(Error::Invalid(format!(
                "keychain account `{account}` does not belong to source `{id}`"
            )));
        }
        let secrets = inner.secrets.clone();
        let (a, v) = (account.to_owned(), value.to_owned());
        tokio::task::spawn_blocking(move || secrets.set(&a, &v))
            .await
            .map_err(join_err)??;
        let me = inner.clone();
        let (sid, a) = (id.clone(), account.to_owned());
        tokio::task::spawn_blocking(move || me.note_secret_account(&sid, &a))
            .await
            .map_err(join_err)?;
        match inner.control(id) {
            Some(ctl) => ctl.sync_now(),
            None => inner.start_source(id),
        }
        Ok(())
    }

    /// Rebuild a source's connector (re-running the [`SettingsResolver`]) and restart its
    /// supervisor if it is enabled. Use after credentials the resolver injects have changed.
    /// The cursor is kept.
    pub async fn reload(&self, id: &SourceId) -> Result<()> {
        let inner = &self.inner;
        let _g = inner.ops.lock().await;
        if !lock(&inner.slots).contains_key(id) {
            return Err(Error::NotFound(format!("source `{id}`")));
        }
        inner.set_reconfiguring(id, true);
        inner.stop_source(id).await;
        if let Some(s) = lock(&inner.slots).get_mut(id) {
            s.connector = None;
            s.reconfiguring = false;
        }
        inner.start_source(id);
        Ok(())
    }

    /// Store a password or app password (`sources.secret.set`) under `password:<id>`.
    pub async fn set_password(&self, id: &SourceId, value: &str) -> Result<()> {
        self.set_secret(
            id,
            &nugget_core::secrets::password_account(id.as_str()),
            value,
        )
        .await
    }

    /// Stop every supervisor and worker and wait for them. Changes queued but not yet applied
    /// are dropped (their cursor was never committed, so the next start re-reads them).
    /// Idempotent.
    pub async fn shutdown(&self) {
        self.inner.shutdown.cancel();
        self.inner.tracker.close();
        self.inner.tracker.wait().await;
        for s in lock(&self.inner.slots).values_mut() {
            s.running = None;
        }
    }
}
