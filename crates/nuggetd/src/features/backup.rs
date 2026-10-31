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
//! **Before the store opens** ([`open_store`], called by the daemon): an interrupted staging
//! folder is removed, a staged restore is applied, otherwise the database is checked read-only
//! with [`nugget_store::check_file`]. A database that is damaged, or that this home's key does
//! not open (a damaged first page, or a file from another install), is never opened or
//! written: the daemon runs on an empty in-memory store in *recovery mode*
//! (`backup.status.recovery`). Then:
//! - knowledge extraction and the vault are off ([`prepare_config`]), so the empty graph can
//!   never re-render or prune the user's vault pages;
//! - no connector kinds are registered and source credentials are kept in memory only
//!   ([`in_recovery`], used by the daemon), so no source runs and `sources.add`/`remove` in the
//!   empty store can never reuse a source id and overwrite or delete the real keychain
//!   credentials stored under it;
//! - backups and exports are refused (a backup of the empty store would rotate good backups
//!   out). Restoring moves the unopened file aside as `nugget.db.corrupt-<ts>`.
//!
//! **Folders.** Backup and export folders inside (or containing) the data, config or vault
//! directory are refused.
//!
//! **Scheduler.** One task: the next backup is due `interval_hours` after the last one (a
//! failed attempt is retried after at most an hour); nothing runs with `interval_hours = 0`,
//! without a folder or passphrase, or in recovery mode. One backup runs at a time: when another
//! backup, export or restore holds the lock, the scheduler retries a minute later; repeated
//! failures back off exponentially (from a minute up to an hour), also when they cannot be
//! recorded in `backup.json`. The very first scheduled backup waits a minute after the folder or
//! passphrase is set (or the daemon starts, e.g. on a just-restored home), so setting up does not immediately run one beside the user's own; a manual
//! `backup.run` that finds a backup running waits for it (up to 30 minutes) and then runs its
//! own, while export and restore refuse while busy. It never sleeps more than 15 minutes at once and recomputes the due
//! time from the wall clock each time, so a laptop that slept through the due time backs up
//! soon after waking.
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
use zeroize::Zeroizing;

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
    /// The vault directory used when `[vault] path` is unset.
    default_vault: Option<PathBuf>,
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
/// (recovery mode, see the module docs). Blocking.
pub(crate) fn open_store(paths: &Paths, key: StoreKey) -> Result<Store> {
    let (store, key, recovery) = open_checked(paths, key)?;
    boots().insert(
        paths.data.clone(),
        Boot {
            key: Some(key),
            recovery,
            vault_root: None,
            default_vault: None,
        },
    );
    Ok(store)
}

fn open_checked(paths: &Paths, key: StoreKey) -> Result<(Store, StoreKey, Option<Recovery>)> {
    let db = paths.db_file();
    // The daemon holds its socket here, so no restore can be staging meanwhile.
    restore::sweep_partial(paths);
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
        FileHealth::Missing | FileHealth::Healthy => {
            let store = Store::open(&db, &key)?;
            Ok((store, key, None))
        }
        health @ (FileHealth::Corrupt(_) | FileHealth::WrongKey) => {
            let reason = match health {
                FileHealth::Corrupt(r) => r,
                _ => "the database cannot be opened with this computer's store key: it is damaged, or it comes from another install".into(),
            };
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

/// Whether [`open_store`] put this home in recovery mode (the daemon then registers no
/// connector kinds and keeps source credentials in memory; see the module docs).
pub(crate) fn in_recovery(paths: &Paths) -> bool {
    boots()
        .get(&paths.data)
        .is_some_and(|b| b.recovery.is_some())
}

/// Adjust the daemon's config before knowledge and the vault are built: in recovery mode turn
/// both off (an empty graph must never re-render or prune the vault). Also records the vault
/// directory (resolved, and the default used without `[vault] path`) for backups and restores.
pub(crate) fn prepare_config(
    paths: &Paths,
    config: &mut Config,
    vault_dir: &Path,
    default_vault: &Path,
) {
    let mut boots = boots();
    let boot = boots.entry(paths.data.clone()).or_default();
    boot.vault_root = Some(vault_dir.to_path_buf());
    boot.default_vault = Some(default_vault.to_path_buf());
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
            default_vault: boot.default_vault,
            lock: tokio::sync::Mutex::new(()),
            backoff: Mutex::new(Backoff::default()),
            // A start counts as setting up: a home whose settings name a folder but record no
            // backup yet (e.g. just restored, taking the backup's `backup.json`) must not back
            // up the moment it starts, beside whatever else is starting.
            setup_at: Mutex::new(Some(OffsetDateTime::now_utc())),
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
    /// The vault directory used without `[vault] path`.
    default_vault: Option<PathBuf>,
    /// Scheduler retries (in memory, so they work even when `backup.json` cannot be written).
    backoff: Mutex<Backoff>,
    /// When the folder or passphrase was last set here, or the daemon started (grace before
    /// the first scheduled backup, one schedule minute).
    setup_at: Mutex<Option<OffsetDateTime>>,
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

#[derive(Deserialize)]
struct PassphraseParams {
    passphrase: Zeroizing<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FolderParams {
    folder: Option<String>,
}

#[derive(Deserialize)]
struct RestoreParams {
    path: String,
    passphrase: Option<Zeroizing<String>>,
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
            Some(f) => {
                let f = folder_param(f)?;
                self.check_outside(&f)?;
                Some(Some(f.display().to_string()))
            }
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
        self.mark_setup();
        self.changed.notify_one();
        self.status().await
    }

    async fn set_passphrase(self: &Arc<Self>, passphrase: Zeroizing<String>) -> Result<()> {
        disk::check_passphrase(&passphrase)?;
        let key = self.key()?.clone();
        let paths = self.paths().clone();
        blocking(move || disk::set_passphrase(&paths, &key, &passphrase)).await?;
        tracing::info!("backup passphrase set");
        self.mark_setup();
        self.changed.notify_one();
        Ok(())
    }

    /// One backup (to `to`, else the configured folder), recorded in the settings and
    /// broadcast. If another backup, export or restore is running, waits for it (up to
    /// [`MANUAL_WAIT`]) and then runs its own: "Back up now" right after the passphrase was set
    /// may meet the first scheduled backup.
    async fn backup(self: Arc<Self>, to: Option<PathBuf>) -> Result<BackupInfo> {
        self.refuse_in_recovery("back up")?;
        let Ok(guard) = tokio::time::timeout(MANUAL_WAIT, self.lock.lock()).await else {
            return Err(Error::Other(
                "another backup, export or restore is still running; try again later".into(),
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
        self.check_outside(&folder)?;
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
        let default_vault = self.default_vault.clone();
        let passphrase = p.passphrase;
        blocking(move || {
            restore::stage(
                &paths,
                &restore::StageRequest {
                    dir: &dir,
                    current: key.as_ref(),
                    passphrase: passphrase.as_deref().map(String::as_str),
                    aside,
                    default_vault: default_vault.as_deref(),
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
        self.check_outside(&folder)?;
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

    /// Settings or the passphrase changed: the first scheduled backup waits [`FIRST_GRACE`]
    /// after this, so setting up does not immediately run one beside the user's own.
    fn mark_setup(&self) {
        *self.setup_at.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(OffsetDateTime::now_utc());
    }

    /// Refuse `folder` inside (or containing) Nugget's data, config or vault directory.
    fn check_outside(&self, folder: &Path) -> Result<()> {
        let paths = self.paths();
        let vault = self.live_vault().or_else(|| self.vault_root.clone());
        let mut protected: Vec<(&str, &Path)> =
            vec![("data", &paths.data), ("config", &paths.config)];
        if let Some(v) = vault.as_deref() {
            protected.push(("vault", v));
        }
        disk::check_folder_outside(folder, &protected)
    }

    /// Run scheduled backups until `stop`.
    async fn scheduler(self: Arc<Self>, stop: tokio_util::sync::CancellationToken) {
        if self.recovery.is_some() {
            return;
        }
        loop {
            let wait = self.due_in().await.unwrap_or(MAX_SLEEP).min(MAX_SLEEP);
            tokio::select! {
                () = stop.cancelled() => return,
                () = self.changed.notified() => continue,
                () = tokio::time::sleep(wait) => {}
            }
            if !self.due_in().await.is_some_and(|d| d.is_zero()) {
                continue;
            }
            let busy = self.lock.try_lock().is_err();
            let outcome = if busy {
                tracing::debug!("scheduled backup waits: another backup, export or restore runs");
                Err(Miss::Busy)
            } else {
                tracing::info!("scheduled backup");
                self.clone()
                    .backup(None)
                    .await
                    .map(drop)
                    .map_err(|_| Miss::Failed)
            };
            self.backoff
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .record(outcome, OffsetDateTime::now_utc(), hour());
        }
    }

    /// How long until the next scheduled backup (`None`: nothing scheduled), from the wall clock.
    async fn due_in(&self) -> Option<Duration> {
        let key = self.key.clone()?;
        let paths = self.paths().clone();
        let (at, first) = blocking(move || {
            let s = disk::Settings::load(&paths)?;
            if disk::envelope_for(&paths, &key)?.is_none() {
                return Ok(None);
            }
            Ok(next_due(&s).map(|at| (at, s.last_backup.is_none())))
        })
        .await
        .ok()
        .flatten()?;
        let setup = *self.setup_at.lock().unwrap_or_else(PoisonError::into_inner);
        let at = match setup {
            Some(t) if first => at.max(t + hour() / 60),
            _ => at,
        };
        let until = self
            .backoff
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .until;
        let at = until.map_or(at, |u| at.max(u));
        let left = at - OffsetDateTime::now_utc();
        Some(left.try_into().unwrap_or(Duration::ZERO))
    }
}

/// How long a manual backup waits for one already running.
const MANUAL_WAIT: Duration = Duration::from_secs(30 * 60);

/// Longest single scheduler sleep (the due time is recomputed from the wall clock after it).
const MAX_SLEEP: Duration = Duration::from_secs(15 * 60);

/// A scheduled attempt that did not back up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Miss {
    /// Another backup, export or restore held the lock.
    Busy,
    /// The backup failed.
    Failed,
}

/// Scheduler retries after a busy lock or a failure.
#[derive(Debug, Default)]
struct Backoff {
    /// No scheduled attempt before this.
    until: Option<OffsetDateTime>,
    /// Failures in a row.
    failures: u32,
}

impl Backoff {
    /// Record an attempt at `now`; `hour` is one schedule hour. Busy: retry in a minute.
    /// Failed: a minute, doubling per failure in a row, at most an hour. Success: clear.
    fn record(
        &mut self,
        outcome: std::result::Result<(), Miss>,
        now: OffsetDateTime,
        hour: Duration,
    ) {
        let minute = (hour / 60).max(Duration::from_millis(100));
        match outcome {
            Ok(()) => *self = Self::default(),
            Err(Miss::Busy) => self.until = Some(now + minute),
            Err(Miss::Failed) => {
                self.failures = self.failures.saturating_add(1);
                let doubled = minute.saturating_mul(1u32 << self.failures.saturating_sub(1).min(6));
                self.until = Some(now + doubled.min(hour.max(minute)));
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_busy_or_failing_scheduler_backs_off() {
        let now = OffsetDateTime::now_utc();
        let hour = Duration::from_secs(3600);
        let mut b = Backoff::default();
        b.record(Err(Miss::Busy), now, hour);
        assert_eq!(
            b.until,
            Some(now + Duration::from_secs(60)),
            "busy: a minute, not a spin"
        );
        assert_eq!(b.failures, 0);
        let mut waits = Vec::new();
        for _ in 0..10 {
            b.record(Err(Miss::Failed), now, hour);
            waits.push((b.until.unwrap() - now).whole_seconds());
        }
        assert_eq!(&waits[..4], &[60, 120, 240, 480]);
        assert_eq!(*waits.last().unwrap(), 3600, "capped at an hour");
        b.record(Ok(()), now, hour);
        assert!(b.until.is_none() && b.failures == 0);
        // Even with a tiny test hour, never a zero wait.
        let mut b = Backoff::default();
        b.record(Err(Miss::Busy), now, Duration::from_secs(1));
        assert!(b.until.unwrap() > now);
    }

    #[test]
    fn sleeps_are_capped() {
        assert!(MAX_SLEEP <= Duration::from_secs(15 * 60));
    }
}
