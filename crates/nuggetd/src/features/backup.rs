//! Backup, restore and export (`backup.*`, `export.run`), the backup scheduler, and the checks
//! made before the store is opened (staged restores, damaged databases).
//!
//! IPC (params and results are JSON objects, snake_case):
//! - `backup.status` → [`Status`].
//! - `backup.configure {folder?, interval_hours?, keep?}` → status. Settings live in
//!   `<data>/backup.json` (defaults: no folder, every 24 hours, keep 7). `folder: ""` clears it.
//! - `backup.set_passphrase {passphrase}` → `{ok: true}`: seals the current store key under it
//!   into `<data>/backup-key.json` (at least 8 characters, else invalid params).
//! - `backup.run {folder?}` → [`BackupInfo`]. Errors without a folder or a passphrase. `folder`
//!   (the CLI's `--to`) backs up elsewhere once; retention then only runs in the configured folder.
//! - `backup.list {folder?}` → `{backups: [BackupInfo]}`, newest first.
//! - `backup.restore {path, passphrase?}` → `{staged: true, restart_required: true}`: verifies
//!   and stages the backup ([`restore`]); the daemon shuts down by itself ~0.5 s later and
//!   applies the restore at its next start.
//! - `export.run {folder}` → [`export::ExportResult`], with `export.progress {phase, done}`
//!   notifications.
//! - Notification `backup.changed` (params: status) when a backup starts, finishes or fails.
//!
//! **Before the store opens** ([`open_store`], called by the daemon): a staged restore is
//! applied; otherwise the database is checked read-only with [`nugget_store::check_file`]. A
//! damaged database is never opened or written: the daemon runs on an empty in-memory store in
//! *recovery mode* (`backup.status.recovery`), with knowledge extraction and the vault turned
//! off ([`prepare_config`]) so the empty graph can never prune the user's vault pages. Sources
//! live in the store, so none run. Backups and exports are refused in recovery mode (a backup
//! of the empty store would rotate good backups out); restoring moves the damaged file aside
//! as `nugget.db.corrupt-<ts>`.
//!
//! **Scheduler.** One task: the next backup is due `interval_hours` after the last one (a
//! failed attempt is retried after at most an hour); nothing runs with `interval_hours = 0`,
//! without a folder or passphrase, or in recovery mode. One backup runs at a time.
//!
//! **Test hook.** `NUGGET_BACKUP_TEST_HOUR_SECS` (read once) makes one "hour" of the schedule
//! that many seconds. Test-only; never set it in a real install.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use nugget_ipc::{Notifier, RpcError, to_result};
use nugget_store::{FileHealth, Store, StoreKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::{Feature, FeatureContext};
use crate::config::Config;
use crate::daemon::{State, blocking};

pub(crate) mod disk;
pub(crate) mod export;
pub(crate) mod restore;

pub(crate) use disk::BackupInfo;

const STATUS: &str = "backup.status";
const CONFIGURE: &str = "backup.configure";
const SET_PASSPHRASE: &str = "backup.set_passphrase";
const RUN: &str = "backup.run";
const LIST: &str = "backup.list";
const RESTORE: &str = "backup.restore";
const EXPORT: &str = "export.run";
/// Notification: status after a backup started, finished or failed.
const CHANGED: &str = "backup.changed";
/// Notification: `{phase, done}` while an export runs.
const EXPORT_PROGRESS: &str = "export.progress";
/// How long after answering `backup.restore` the daemon shuts itself down.
const RESTORE_EXIT_DELAY: Duration = Duration::from_millis(500);
/// Test-only: seconds per schedule "hour" (see the module docs).
const TEST_HOUR_ENV: &str = "NUGGET_BACKUP_TEST_HOUR_SECS";

/// One schedule hour (an hour, unless the test hook says otherwise).
fn hour() -> Duration {
    static HOUR: OnceLock<Duration> = OnceLock::new();
    *HOUR.get_or_init(|| {
        std::env::var(TEST_HOUR_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|&s| s > 0)
            .map_or(Duration::from_secs(3600), Duration::from_secs)
    })
}

// ---- before the store opens ----

/// The daemon found its database damaged and did not open it.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Recovery {
    pub reason: String,
    pub db_path: String,
    pub moved_to: Option<String>,
    pub latest_backup: Option<BackupInfo>,
}

/// What [`open_store`] learned, handed to the feature when it starts (per data directory, so
/// several daemons in one process, as in tests, stay apart).
#[derive(Default)]
struct Boot {
    key: Option<StoreKey>,
    recovery: Option<Recovery>,
    vault_root: Option<PathBuf>,
}

fn boots() -> std::sync::MutexGuard<'static, HashMap<PathBuf, Boot>> {
    static BOOTS: OnceLock<Mutex<HashMap<PathBuf, Boot>>> = OnceLock::new();
    BOOTS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Open the daemon's store: apply a staged restore first, then check the database read-only.
/// A damaged database is left untouched and an empty in-memory store is returned instead
/// (recovery mode, see the module docs). A wrong key fails as [`Store::open`] does. Blocking.
pub(crate) fn open_store(paths: &Paths, key: StoreKey) -> Result<Store> {
    let (store, key, recovery) = open_checked(paths, key)?;
    boots().insert(
        paths.data.clone(),
        Boot {
            key: Some(key),
            recovery,
            vault_root: None,
        },
    );
    Ok(store)
}

fn open_checked(paths: &Paths, key: StoreKey) -> Result<(Store, StoreKey, Option<Recovery>)> {
    let db = paths.db_file();
    let applied = match restore::apply_pending(paths, Some(&key)) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(error = %e, staging = %restore::pending_dir(paths).display(),
                "RESTORE FAILED: the staged restore could not be applied; the previous data is untouched or set aside next to the database, and the staging folder is kept");
            return Err(Error::Other(format!(
                "applying the staged restore failed: {e} (data is kept; see {})",
                restore::pending_dir(paths).display()
            )));
        }
    };
    if let Some((key, applied)) = applied {
        let store = Store::open(&db, &key)?;
        restore::finish_applied(paths, &store, Some(&applied))?;
        return Ok((store, key, None));
    }
    if restore::has_applied(paths) {
        let store = Store::open(&db, &key)?;
        restore::finish_applied(paths, &store, None)?;
        return Ok((store, key, None));
    }
    match nugget_store::check_file(&db, &key, false)? {
        FileHealth::Missing | FileHealth::Healthy | FileHealth::WrongKey => {
            // WrongKey: `open` reports it as it always has.
            let store = Store::open(&db, &key)?;
            Ok((store, key, None))
        }
        FileHealth::Corrupt(reason) => {
            tracing::error!(db = %db.display(), %reason,
                "DATABASE DAMAGED: not opening or writing it; running on an empty temporary store until it is restored from a backup");
            let recovery = Recovery {
                reason,
                db_path: db.display().to_string(),
                moved_to: None,
                latest_backup: None,
            };
            Ok((Store::open_in_memory()?, key, Some(recovery)))
        }
    }
}

/// Adjust the daemon's config before knowledge and the vault are built: in recovery mode turn
/// both off (an empty graph must never re-render or prune the vault). Also records the vault
/// directory for restores.
pub(crate) fn prepare_config(paths: &Paths, config: &mut Config, vault_dir: &Path) {
    let mut boots = boots();
    let boot = boots.entry(paths.data.clone()).or_default();
    boot.vault_root = Some(vault_dir.to_path_buf());
    if boot.recovery.is_some() {
        tracing::warn!("recovery mode: knowledge extraction and the vault are off");
        config.knowledge.enabled = false;
        config.vault.enabled = false;
    }
}

// ---- the feature ----

pub(crate) fn start(cx: &FeatureContext<'_>) -> Result<Arc<dyn Feature>> {
    let state = cx.state.clone();
    let boot = boots().remove(&state.paths.data).unwrap_or_default();
    let key = match boot.key {
        Some(k) => Some(k),
        None => StoreKey::load_existing(&state.paths)?,
    };
    let f = Arc::new(BackupFeature {
        inner: Arc::new(Inner {
            state: state.clone(),
            key,
            recovery: boot.recovery,
            vault_root: boot.vault_root,
            lock: tokio::sync::Mutex::new(()),
            settings_lock: Mutex::new(()),
            changed: tokio::sync::Notify::new(),
        }),
    });
    let inner = f.inner.clone();
    let stop = state.shutdown.clone();
    state
        .tasks
        .spawn(async move { inner.scheduler(stop).await });
    Ok(f)
}

struct BackupFeature {
    inner: Arc<Inner>,
}

struct Inner {
    state: Arc<State>,
    /// The key the store was opened with (`None` only if it cannot be found at all).
    key: Option<StoreKey>,
    recovery: Option<Recovery>,
    /// The configured vault directory (from the daemon's config).
    vault_root: Option<PathBuf>,
    /// Held while a backup, export or restore runs.
    lock: tokio::sync::Mutex<()>,
    /// Serialises read-modify-write of `backup.json`.
    settings_lock: Mutex<()>,
    /// Settings changed (reschedule).
    changed: tokio::sync::Notify,
}

/// `backup.status`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Status {
    pub folder: Option<String>,
    pub interval_hours: u32,
    pub keep: u32,
    pub passphrase_set: bool,
    pub running: bool,
    pub last_backup: Option<BackupInfo>,
    pub last_error: Option<String>,
    pub next_due: Option<String>,
    pub restore_pending: bool,
    pub recovery: Option<Recovery>,
}

#[derive(Debug, Default, Deserialize)]
struct ConfigureParams {
    folder: Option<String>,
    interval_hours: Option<u32>,
    keep: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct PassphraseParams {
    passphrase: String,
}

#[derive(Debug, Default, Deserialize)]
struct FolderParams {
    folder: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RestoreParams {
    path: String,
    passphrase: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExportParams {
    folder: String,
}

fn parse<T: serde::de::DeserializeOwned + Default>(params: Value) -> Result<T> {
    if params.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value(params).map_err(|e| Error::Invalid(e.to_string()))
}

fn parse_required<T: serde::de::DeserializeOwned>(params: Value) -> Result<T> {
    serde_json::from_value(params).map_err(|e| Error::Invalid(e.to_string()))
}

/// An absolute folder from a request.
fn folder_param(s: &str) -> Result<PathBuf> {
    let p = PathBuf::from(s.trim());
    if !p.is_absolute() {
        return Err(Error::Invalid(format!(
            "the folder must be an absolute path: {s}"
        )));
    }
    Ok(p)
}

#[async_trait]
impl Feature for BackupFeature {
    fn name(&self) -> &'static str {
        "backup"
    }

    fn methods(&self) -> &'static [&'static str] {
        &[
            STATUS,
            CONFIGURE,
            SET_PASSPHRASE,
            RUN,
            LIST,
            RESTORE,
            EXPORT,
        ]
    }

    async fn call(
        &self,
        method: &str,
        params: Value,
        _notify: Notifier,
    ) -> std::result::Result<Value, RpcError> {
        let me = &self.inner;
        match method {
            STATUS => to_result(me.status().await?),
            CONFIGURE => to_result(me.configure(parse(params)?).await?),
            SET_PASSPHRASE => {
                let p: PassphraseParams = parse_required(params)?;
                me.set_passphrase(p.passphrase).await?;
                Ok(json!({"ok": true}))
            }
            RUN => {
                let p: FolderParams = parse(params)?;
                let folder = p.folder.filter(|f| !f.trim().is_empty());
                let folder = folder.as_deref().map(folder_param).transpose()?;
                to_result(me.clone().backup(folder).await?)
            }
            LIST => {
                let p: FolderParams = parse(params)?;
                let folder = match p.folder.filter(|f| !f.trim().is_empty()) {
                    Some(f) => Some(folder_param(&f)?),
                    None => me.settings().await?.folder(),
                };
                let backups = match folder {
                    Some(f) => blocking(move || Ok(disk::list_backups(&f))).await?,
                    None => Vec::new(),
                };
                Ok(json!({ "backups": backups }))
            }
            RESTORE => {
                let p: RestoreParams = parse_required(params)?;
                me.clone().restore(p).await?;
                Ok(json!({"staged": true, "restart_required": true}))
            }
            EXPORT => {
                let p: ExportParams = parse_required(params)?;
                let folder = folder_param(&p.folder)?;
                to_result(me.clone().export(folder).await?)
            }
            _ => Err(RpcError::method_not_found(method)),
        }
    }
}

impl Inner {
    fn paths(&self) -> &Paths {
        &self.state.paths
    }

    fn key(&self) -> Result<&StoreKey> {
        self.key
            .as_ref()
            .ok_or_else(|| Error::Unsupported("the store key is not available".into()))
    }

    async fn settings(&self) -> Result<disk::Settings> {
        let paths = self.paths().clone();
        blocking(move || disk::Settings::load(&paths)).await
    }

    /// Read-modify-write `backup.json`.
    async fn update_settings(
        self: &Arc<Self>,
        f: impl FnOnce(&mut disk::Settings) + Send + 'static,
    ) -> Result<disk::Settings> {
        let me = self.clone();
        blocking(move || {
            let _g = me
                .settings_lock
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let mut s = disk::Settings::load(me.paths())?;
            f(&mut s);
            s.save(me.paths())?;
            Ok(s)
        })
        .await
    }

    /// The vault to back up or export: the running vault, if enabled.
    fn live_vault(&self) -> Option<PathBuf> {
        self.state
            .knowledge
            .vault
            .as_ref()
            .map(|v| v.root().to_path_buf())
    }

    fn refuse_in_recovery(&self, what: &str) -> Result<()> {
        if self.recovery.is_some() {
            return Err(Error::Unsupported(format!(
                "the database is damaged and Nugget is running on an empty temporary store, so it does not {what}; restore from a backup first"
            )));
        }
        Ok(())
    }

    async fn status(&self) -> Result<Status> {
        let paths = self.paths().clone();
        let key = self.key.clone();
        let recovery = self.recovery.clone();
        let running = self.lock.try_lock().is_err();
        let usable = recovery.is_none();
        blocking(move || {
            let s = disk::Settings::load(&paths)?;
            let passphrase_set = match &key {
                Some(k) => disk::envelope_for(&paths, k).ok().flatten().is_some(),
                None => false,
            };
            let next_due = (usable && passphrase_set)
                .then(|| next_due(&s))
                .flatten()
                .map(disk::rfc3339);
            let recovery = recovery.map(|mut r| {
                r.latest_backup = s
                    .folder()
                    .and_then(|f| disk::list_backups(&f).into_iter().next());
                r
            });
            Ok(Status {
                folder: s.folder.clone(),
                interval_hours: s.interval_hours,
                keep: s.keep,
                passphrase_set,
                running,
                last_backup: s.last_backup.clone(),
                last_error: s.last_error.clone(),
                next_due,
                restore_pending: restore::is_pending(&paths),
                recovery,
            })
        })
        .await
    }

    async fn broadcast(&self) {
        match self.status().await {
            Ok(s) => self
                .state
                .hub
                .broadcast(CHANGED, serde_json::to_value(s).unwrap_or(Value::Null)),
            Err(e) => tracing::warn!(error = %e, "backup status unavailable"),
        }
    }

    async fn configure(self: &Arc<Self>, p: ConfigureParams) -> Result<Status> {
        let folder = match p.folder.as_deref().map(str::trim) {
            None => None,
            Some("") => Some(None),
            Some(f) => Some(Some(folder_param(f)?.display().to_string())),
        };
        if let Some(h) = p.interval_hours
            && h > disk::MAX_INTERVAL_HOURS
        {
            return Err(Error::Invalid(format!(
                "interval_hours must be at most {}",
                disk::MAX_INTERVAL_HOURS
            )));
        }
        let (lo, hi) = disk::KEEP_RANGE;
        if let Some(k) = p.keep
            && !(lo..=hi).contains(&k)
        {
            return Err(Error::Invalid(format!(
                "keep must be between {lo} and {hi}"
            )));
        }
        self.update_settings(move |s| {
            if let Some(f) = folder {
                s.folder = f;
            }
            if let Some(h) = p.interval_hours {
                s.interval_hours = h;
            }
            if let Some(k) = p.keep {
                s.keep = k;
            }
        })
        .await?;
        self.changed.notify_one();
        self.status().await
    }

    async fn set_passphrase(self: &Arc<Self>, passphrase: String) -> Result<()> {
        disk::check_passphrase(&passphrase)?;
        let key = self.key()?.clone();
        let paths = self.paths().clone();
        blocking(move || disk::set_passphrase(&paths, &key, &passphrase)).await?;
        tracing::info!("backup passphrase set");
        self.changed.notify_one();
        Ok(())
    }

    /// One backup (to `to`, else the configured folder), recorded in the settings and
    /// broadcast. Fails at once if one is already running.
    async fn backup(self: Arc<Self>, to: Option<PathBuf>) -> Result<BackupInfo> {
        self.refuse_in_recovery("back up")?;
        let Ok(guard) = self.lock.try_lock() else {
            return Err(Error::Other(
                "a backup, export or restore is already running".into(),
            ));
        };
        let settings = self.settings().await?;
        let configured = settings.folder();
        let folder = match to.clone().or_else(|| configured.clone()) {
            Some(f) => f,
            None => {
                return Err(Error::Invalid(
                    "no backup folder configured (backup.configure {folder})".into(),
                ));
            }
        };
        let keep = (configured.as_ref() == Some(&folder)).then_some(settings.keep);
        self.broadcast().await;
        let key = self.key()?.clone();
        let (paths, store, vault) = (
            self.paths().clone(),
            self.state.store.clone(),
            self.live_vault(),
        );
        let started = std::time::Instant::now();
        let result = blocking(move || {
            disk::run_backup(&disk::BackupRequest {
                paths: &paths,
                store: &store,
                key: &key,
                folder: &folder,
                keep,
                vault_root: vault.as_deref(),
            })
        })
        .await;
        let recorded = match &result {
            Ok(info) => {
                tracing::info!(path = %info.path, items = info.items, bytes = info.bytes,
                    took_ms = started.elapsed().as_millis() as u64, "backup written");
                let info = info.clone();
                self.update_settings(move |s| {
                    s.last_backup = Some(info);
                    s.last_error = None;
                    s.last_failure_at = None;
                })
                .await
            }
            Err(e) => {
                tracing::warn!(error = %e, "backup failed");
                let msg = e.to_string();
                self.update_settings(move |s| {
                    s.last_error = Some(msg);
                    s.last_failure_at = Some(disk::rfc3339(OffsetDateTime::now_utc()));
                })
                .await
            }
        };
        if let Err(e) = recorded {
            tracing::warn!(error = %e, "cannot record the backup in backup.json");
        }
        drop(guard);
        self.broadcast().await;
        result
    }

    async fn restore(self: Arc<Self>, p: RestoreParams) -> Result<()> {
        let dir = PathBuf::from(p.path.trim());
        if !dir.is_absolute() {
            return Err(Error::Invalid(format!(
                "the backup path must be absolute: {}",
                p.path
            )));
        }
        let Ok(guard) = self.lock.try_lock() else {
            return Err(Error::Other(
                "a backup, export or restore is already running".into(),
            ));
        };
        let paths = self.paths().clone();
        let key = self.key.clone();
        let aside = if self.recovery.is_some() {
            restore::Aside::Corrupt
        } else {
            restore::Aside::PreRestore
        };
        let vault = self.live_vault().or_else(|| self.vault_root.clone());
        let passphrase = p.passphrase;
        blocking(move || {
            restore::stage(
                &paths,
                &restore::StageRequest {
                    dir: &dir,
                    current: key.as_ref(),
                    passphrase: passphrase.as_deref(),
                    aside,
                    vault_root: vault.as_deref(),
                },
            )
        })
        .await?;
        drop(guard);
        tracing::warn!("restore staged: nuggetd stops now and applies it at its next start");
        let stop = self.state.shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(RESTORE_EXIT_DELAY).await;
            stop.cancel();
        });
        Ok(())
    }

    async fn export(self: Arc<Self>, folder: PathBuf) -> Result<export::ExportResult> {
        self.refuse_in_recovery("export")?;
        let Ok(_guard) = self.lock.try_lock() else {
            return Err(Error::Other(
                "a backup, export or restore is already running".into(),
            ));
        };
        let (store, vault, hub) = (
            self.state.store.clone(),
            self.live_vault(),
            self.state.hub.clone(),
        );
        blocking(move || {
            export::run_export(&store, &folder, vault.as_deref(), &mut |phase, done| {
                hub.broadcast(EXPORT_PROGRESS, json!({"phase": phase, "done": done}));
            })
        })
        .await
    }

    /// Run scheduled backups until `stop`.
    async fn scheduler(self: Arc<Self>, stop: tokio_util::sync::CancellationToken) {
        if self.recovery.is_some() {
            return;
        }
        loop {
            let wait = match self.due_in().await {
                Some(d) => d,
                None => Duration::from_secs(365 * 24 * 3600),
            };
            tokio::select! {
                () = stop.cancelled() => return,
                () = self.changed.notified() => continue,
                () = tokio::time::sleep(wait) => {}
            }
            if self.due_in().await.is_some_and(|d| d.is_zero()) {
                tracing::info!("scheduled backup");
                let _ = self.clone().backup(None).await;
            }
        }
    }

    /// How long until the next scheduled backup (`None`: nothing scheduled).
    async fn due_in(&self) -> Option<Duration> {
        let key = self.key.clone()?;
        let paths = self.paths().clone();
        let at = blocking(move || {
            let s = disk::Settings::load(&paths)?;
            if disk::envelope_for(&paths, &key)?.is_none() {
                return Ok(None);
            }
            Ok(next_due(&s))
        })
        .await
        .ok()
        .flatten()?;
        let left = at - OffsetDateTime::now_utc();
        Some(left.try_into().unwrap_or(Duration::ZERO))
    }
}

/// When the next scheduled backup is due: `interval_hours` after the last one, a failed attempt
/// retried after at most an hour; now if there was none. `None` when manual only or without a
/// folder.
fn next_due(s: &disk::Settings) -> Option<OffsetDateTime> {
    if s.interval_hours == 0 || s.folder.is_none() {
        return None;
    }
    let interval = hour() * s.interval_hours;
    let mut due = s
        .last_backup
        .as_ref()
        .and_then(|b| disk::parse_time(&b.created_at))
        .map(|t| t + interval);
    if let Some(failed) = s.last_failure_at.as_deref().and_then(disk::parse_time) {
        let retry = failed + interval.min(hour());
        due = Some(due.map_or(retry, |d| d.max(retry)));
    }
    Some(due.unwrap_or_else(OffsetDateTime::now_utc))
}
