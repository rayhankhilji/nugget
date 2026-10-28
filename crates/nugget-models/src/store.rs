//! Content-addressed model store with active/previous slots.
//!
//! ```text
//! <root>/blobs/sha256/<hex>      verified model files, named by content hash
//! <root>/staging/<hex>/          in-progress downloads (resumable)
//! <root>/slots.json              model id -> { active, previous }
//! ```

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nugget_core::egress::EgressSink;
use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::download::{
    CancelOnDrop, DATA_FILE, Download, DownloadOptions, Phase, Progress, ProgressFn, Reporter,
    sha256_file, staged_bytes,
};
use crate::manifest::{ModelEntry, is_sha256_hex};
use crate::preflight::{DiskSpace, SystemDiskSpace, check_disk_space};

const SLOTS_FILE: &str = "slots.json";
const SLOTS_VERSION: u32 = 1;

/// One installed, hash-verified model file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledModel {
    pub sha256: String,
    pub size: u64,
    pub revision: String,
    pub file_name: String,
    pub family: String,
    pub license: String,
    #[serde(with = "time::serde::rfc3339")]
    pub installed_at: OffsetDateTime,
    /// Last successful full re-hash (install counts as one).
    #[serde(with = "time::serde::rfc3339::option")]
    pub verified_at: Option<OffsetDateTime>,
}

/// The active version of a model and the one it replaced (for rollback).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub active: InstalledModel,
    pub previous: Option<InstalledModel>,
}

/// What [`ModelStore::gc`] removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    /// SHA-256 of each removed blob.
    pub removed: Vec<String>,
    pub freed_bytes: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SlotsFile {
    version: u32,
    models: BTreeMap<String, Slot>,
}

/// Verified local model storage. Share it behind an `Arc`; all methods take `&self`.
///
/// One `ModelStore` per root is assumed (the daemon owns it); operations are serialised in-process.
pub struct ModelStore {
    root: PathBuf,
    client: reqwest::Client,
    egress: Arc<dyn EgressSink>,
    disk: Arc<dyn DiskSpace>,
    opts: DownloadOptions,
    app_version: semver::Version,
    /// Serialises slot updates, blob placement and gc.
    lock: tokio::sync::Mutex<()>,
    /// SHA-256 of installs in flight.
    installing: Mutex<HashSet<String>>,
}

impl std::fmt::Debug for ModelStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelStore")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

/// Build the default HTTP client: rustls, system proxy settings, conservative timeouts.
pub fn default_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("nugget/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| Error::Other(format!("cannot build HTTP client: {e}")))
}

impl ModelStore {
    /// Store rooted at [`Paths::models_dir`].
    pub fn open(paths: &Paths, egress: Arc<dyn EgressSink>) -> Result<Self> {
        Self::new(paths.models_dir(), egress)
    }

    /// Store rooted at `root` (created if missing).
    pub fn new(root: impl Into<PathBuf>, egress: Arc<dyn EgressSink>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            client: default_client()?,
            egress,
            disk: Arc::new(SystemDiskSpace),
            opts: DownloadOptions::default(),
            app_version: semver::Version::parse(crate::APP_VERSION)
                .map_err(|e| Error::Other(format!("bad app version: {e}")))?,
            lock: tokio::sync::Mutex::new(()),
            installing: Mutex::new(HashSet::new()),
        })
    }

    /// Use a custom HTTP client (proxy, TLS roots, tests).
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    pub fn with_download_options(mut self, opts: DownloadOptions) -> Self {
        self.opts = opts;
        self
    }

    /// Replace the free-space source used by the preflight.
    pub fn with_disk_space(mut self, disk: Arc<dyn DiskSpace>) -> Self {
        self.disk = disk;
        self
    }

    /// Override the app version used for `min_app_version` checks.
    pub fn with_app_version(mut self, v: semver::Version) -> Self {
        self.app_version = v;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn blob_dir(&self) -> PathBuf {
        self.root.join("blobs").join("sha256")
    }

    fn blob_path(&self, sha256: &str) -> PathBuf {
        self.blob_dir().join(sha256)
    }

    fn staging_dir(&self, sha256: &str) -> PathBuf {
        self.root.join("staging").join(sha256)
    }

    /// Download (resuming if possible), verify and activate `entry`. The previously active version
    /// of the same model id is kept as `previous` for [`ModelStore::rollback`].
    ///
    /// `entry` must come from a manifest accepted by [`crate::verify_manifest`]; it is re-validated
    /// here regardless. Cancel by dropping the future: staged bytes are kept for the next call,
    /// and a hash in progress stops within 1 MiB.
    pub async fn install(
        &self,
        entry: &ModelEntry,
        on_progress: impl Fn(Progress) + Send + Sync + 'static,
    ) -> Result<InstalledModel> {
        let f: ProgressFn = Arc::new(on_progress);
        let reporter = Reporter::new(&entry.id, entry.size, f, self.opts.progress_interval);
        // Dropped with this future: stops any blocking hash it started.
        let cancel = CancelOnDrop::new();
        let res = self.install_inner(entry, &reporter, &cancel.flag()).await;
        match &res {
            Ok(_) => reporter.emit(Phase::Installed, entry.size),
            Err(e) => reporter.fail(e),
        }
        res
    }

    async fn install_inner(
        &self,
        entry: &ModelEntry,
        reporter: &Reporter,
        cancel: &Arc<AtomicBool>,
    ) -> Result<InstalledModel> {
        entry.validate()?;
        if !entry.compatible_with(&self.app_version) {
            return Err(Error::Unsupported(format!(
                "model `{}` needs Nugget >= {} (this is {})",
                entry.id, entry.min_app_version, self.app_version
            )));
        }
        let _guard = InstallGuard::acquire(&self.installing, &entry.sha256)?;

        // Already have these exact bytes (another id, or a reinstall)? Re-verify and activate.
        let blob = self.blob_path(&entry.sha256);
        if blob.exists() {
            if hash_with_progress(&blob, reporter.clone(), cancel).await? == entry.sha256 {
                // The hash ran without the lock, so gc may have removed the (then unreferenced)
                // blob since. Activate only if it is still there; otherwise download it again.
                if let Some(installed) = self.activate_existing(entry).await? {
                    return Ok(installed);
                }
                tracing::warn!(model = %entry.id, "verified blob was removed meanwhile; re-downloading");
            } else {
                tracing::warn!(model = %entry.id, "existing blob failed verification; re-downloading");
                let _lock = self.lock.lock().await;
                match std::fs::remove_file(&blob) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                    _ => {}
                }
            }
        }

        let staging = self.staging_dir(&entry.sha256);
        check_disk_space(
            self.disk.as_ref(),
            &self.root,
            entry.size,
            staged_bytes(&staging, entry),
        )?;

        let data = Download {
            client: &self.client,
            entry,
            dir: staging.clone(),
            opts: &self.opts,
            reporter: reporter.clone(),
            egress: Arc::clone(&self.egress),
        }
        .run()
        .await?;

        let len = std::fs::metadata(&data)?.len();
        let actual = if len == entry.size {
            Some(hash_with_progress(&data, reporter.clone(), cancel).await?)
        } else {
            None
        };
        if actual.as_deref() != Some(entry.sha256.as_str()) {
            // Never resume from bytes that failed verification.
            let _ = std::fs::remove_dir_all(&staging);
            return Err(Error::Integrity(match actual {
                Some(h) => format!(
                    "model `{}`: SHA-256 mismatch (expected {}, got {h}); staging discarded",
                    entry.id, entry.sha256
                ),
                None => format!(
                    "model `{}`: size mismatch (expected {}, got {len}); staging discarded",
                    entry.id, entry.size
                ),
            }));
        }
        sync_file(&data)?;

        let _lock = self.lock.lock().await;
        let dir = self.blob_dir();
        std::fs::create_dir_all(&dir)?;
        std::fs::rename(&data, &blob)?;
        sync_dir(&dir);
        let _ = std::fs::remove_dir_all(&staging);
        self.activate_locked(entry)
    }

    /// Activate an already present blob, re-checking under the lock (which gc also takes) that
    /// it still exists with the expected size. `None` if it is gone or wrong.
    async fn activate_existing(&self, entry: &ModelEntry) -> Result<Option<InstalledModel>> {
        let _lock = self.lock.lock().await;
        match std::fs::metadata(self.blob_path(&entry.sha256)) {
            Ok(md) if md.is_file() && md.len() == entry.size => {
                self.activate_locked(entry).map(Some)
            }
            Ok(_) => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn activate_locked(&self, entry: &ModelEntry) -> Result<InstalledModel> {
        let now = OffsetDateTime::now_utc();
        let installed = InstalledModel {
            sha256: entry.sha256.clone(),
            size: entry.size,
            revision: entry.revision.clone(),
            file_name: entry.file_name.clone(),
            family: entry.family.clone(),
            license: entry.license.clone(),
            installed_at: now,
            verified_at: Some(now),
        };
        let mut slots = self.read_slots()?;
        let slot = match slots.models.remove(&entry.id) {
            Some(old) if old.active.sha256 == entry.sha256 => Slot {
                active: installed.clone(),
                previous: old.previous,
            },
            Some(old) => Slot {
                active: installed.clone(),
                previous: Some(old.active),
            },
            None => Slot {
                active: installed.clone(),
                previous: None,
            },
        };
        slots.models.insert(entry.id.clone(), slot);
        self.write_slots(&slots)?;
        tracing::info!(model = %entry.id, sha256 = %entry.sha256, "model installed");
        Ok(installed)
    }

    /// Path of the active version of `id`. Cheap check: the blob exists with the recorded size.
    /// Use [`ModelStore::verify`] for a full re-hash.
    pub fn path(&self, id: &str) -> Result<PathBuf> {
        let slot = self.slot(id)?.ok_or_else(|| not_installed(id))?;
        self.checked_blob(id, &slot.active)
    }

    fn checked_blob(&self, id: &str, m: &InstalledModel) -> Result<PathBuf> {
        if !is_sha256_hex(&m.sha256) {
            return Err(Error::Integrity(format!(
                "model `{id}`: corrupt slot record"
            )));
        }
        let p = self.blob_path(&m.sha256);
        match std::fs::metadata(&p) {
            Ok(md) if md.len() == m.size => Ok(p),
            Ok(md) => Err(Error::Integrity(format!(
                "model `{id}`: blob is {} bytes, expected {}",
                md.len(),
                m.size
            ))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::Integrity(format!(
                "model `{id}`: blob {} is missing",
                m.sha256
            ))),
            Err(e) => Err(e.into()),
        }
    }

    /// Fully re-hash the active version of `id`. On success records `verified_at`.
    pub async fn verify(&self, id: &str) -> Result<()> {
        let slot = self.slot(id)?.ok_or_else(|| not_installed(id))?;
        let path = self.checked_blob(id, &slot.active)?;
        let cancel = CancelOnDrop::new();
        let flag = cancel.flag();
        let actual = tokio::task::spawn_blocking(move || sha256_file(&path, &flag, |_| {}))
            .await
            .map_err(|e| Error::Other(format!("hash task failed: {e}")))??;
        if actual != slot.active.sha256 {
            return Err(Error::Integrity(format!(
                "model `{id}`: SHA-256 mismatch (expected {}, got {actual})",
                slot.active.sha256
            )));
        }
        let _lock = self.lock.lock().await;
        let mut slots = self.read_slots()?;
        if let Some(s) = slots.models.get_mut(id)
            && s.active.sha256 == slot.active.sha256
        {
            s.active.verified_at = Some(OffsetDateTime::now_utc());
            self.write_slots(&slots)?;
        }
        Ok(())
    }

    /// Make the previous version of `id` active again (the current one becomes `previous`, so a
    /// second rollback rolls forward). Returns the newly active version.
    pub async fn rollback(&self, id: &str) -> Result<InstalledModel> {
        let _lock = self.lock.lock().await;
        let mut slots = self.read_slots()?;
        let slot = slots.models.get_mut(id).ok_or_else(|| not_installed(id))?;
        let prev = slot
            .previous
            .clone()
            .ok_or_else(|| Error::NotFound(format!("model `{id}` has no previous version")))?;
        self.checked_blob(id, &prev)?;
        let old = std::mem::replace(&mut slot.active, prev.clone());
        slot.previous = Some(old);
        self.write_slots(&slots)?;
        tracing::info!(model = id, sha256 = %prev.sha256, "rolled back");
        Ok(prev)
    }

    /// Delete blobs no slot references (neither active nor previous) and stray temp files.
    pub async fn gc(&self) -> Result<GcReport> {
        let _lock = self.lock.lock().await;
        let slots = self.read_slots()?;
        let keep: HashSet<&str> = slots
            .models
            .values()
            .flat_map(|s| std::iter::once(&s.active).chain(s.previous.as_ref()))
            .map(|m| m.sha256.as_str())
            .collect();
        let mut report = GcReport::default();
        let entries = match std::fs::read_dir(self.blob_dir()) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
            Err(e) => return Err(e.into()),
        };
        for e in entries {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if keep.contains(name.as_str()) || !e.file_type()?.is_file() {
                continue;
            }
            let size = e.metadata().map_or(0, |m| m.len());
            std::fs::remove_file(e.path())?;
            report.freed_bytes += size;
            if is_sha256_hex(&name) {
                report.removed.push(name);
            }
        }
        report.removed.sort();
        Ok(report)
    }

    /// Discard partially downloaded data for `entry` (a later install starts from zero).
    pub fn discard_staging(&self, entry: &ModelEntry) -> Result<()> {
        if !is_sha256_hex(&entry.sha256) {
            return Err(Error::Invalid("bad sha256".into()));
        }
        if crate::download::lock(&self.installing).contains(&entry.sha256) {
            return Err(Error::Invalid(format!(
                "model `{}` is being installed",
                entry.id
            )));
        }
        match std::fs::remove_dir_all(self.staging_dir(&entry.sha256)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// Slot for `id`, if installed.
    pub fn slot(&self, id: &str) -> Result<Option<Slot>> {
        Ok(self.read_slots()?.models.remove(id))
    }

    /// All installed models.
    pub fn installed(&self) -> Result<BTreeMap<String, Slot>> {
        Ok(self.read_slots()?.models)
    }

    /// `true` if `entry.id` is installed but its active bytes differ from `entry` (an update).
    pub fn needs_update(&self, entry: &ModelEntry) -> Result<bool> {
        Ok(self
            .slot(&entry.id)?
            .is_some_and(|s| s.active.sha256 != entry.sha256))
    }

    /// Bytes already staged for `entry` (resumable progress).
    pub fn staged_bytes(&self, entry: &ModelEntry) -> u64 {
        staged_bytes(&self.staging_dir(&entry.sha256), entry)
    }

    fn read_slots(&self) -> Result<SlotsFile> {
        let path = self.root.join(SLOTS_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SlotsFile {
                    version: SLOTS_VERSION,
                    models: BTreeMap::new(),
                });
            }
            Err(e) => return Err(e.into()),
        };
        let slots: SlotsFile = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Storage(format!("{} is corrupt: {e}", path.display())))?;
        if slots.version != SLOTS_VERSION {
            return Err(Error::Storage(format!(
                "{} has unsupported version {}",
                path.display(),
                slots.version
            )));
        }
        Ok(slots)
    }

    fn write_slots(&self, slots: &SlotsFile) -> Result<()> {
        let tmp = self.root.join(format!("{SLOTS_FILE}.tmp"));
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(slots)?)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, self.root.join(SLOTS_FILE))?;
        sync_dir(&self.root);
        Ok(())
    }

    /// Staging file of an in-progress download (tests and diagnostics).
    #[doc(hidden)]
    pub fn staging_data_path(&self, entry: &ModelEntry) -> PathBuf {
        self.staging_dir(&entry.sha256).join(DATA_FILE)
    }
}

fn not_installed(id: &str) -> Error {
    Error::NotFound(format!("model `{id}` is not installed"))
}

async fn hash_with_progress(
    path: &Path,
    reporter: Reporter,
    cancel: &Arc<AtomicBool>,
) -> Result<String> {
    let path = path.to_path_buf();
    let cancel = cancel.clone();
    reporter.emit(Phase::Verifying, 0);
    tokio::task::spawn_blocking(move || {
        sha256_file(&path, &cancel, |n| reporter.emit(Phase::Verifying, n))
    })
    .await
    .map_err(|e| Error::Other(format!("hash task failed: {e}")))?
}

fn sync_file(path: &Path) -> Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .sync_all()?;
    Ok(())
}

/// Make a rename durable. Directories cannot be opened for sync on Windows; NTFS journals
/// metadata, so this is a no-op there.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

struct InstallGuard<'a> {
    set: &'a Mutex<HashSet<String>>,
    key: String,
}

impl<'a> InstallGuard<'a> {
    fn acquire(set: &'a Mutex<HashSet<String>>, key: &str) -> Result<Self> {
        if !crate::download::lock(set).insert(key.to_string()) {
            return Err(Error::Invalid(format!(
                "an install of {key} is already in progress"
            )));
        }
        Ok(Self {
            set,
            key: key.to_string(),
        })
    }
}

impl Drop for InstallGuard<'_> {
    fn drop(&mut self) {
        crate::download::lock(self.set).remove(&self.key);
    }
}
