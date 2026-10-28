//! Verified installation of inference runtimes into the model store.
//!
//! ```text
//! <root>/runtimes/<archive sha256>/                 extracted release archive
//! <root>/runtimes/<archive sha256>/.nugget-runtime.json   install record (written last)
//! ```
//!
//! The archive is downloaded with the same resumable, verified machinery as models (staging under
//! `<root>/staging/<sha256>/`, SHA-256 checked before anything is unpacked), extracted with
//! [`crate::archive::extract_archive`] into a staging directory that is renamed into place, and
//! then deleted. A runtime counts as installed only once its install record exists.

use std::path::PathBuf;
use std::sync::Arc;

use nugget_core::egress::EgressSink;
use nugget_core::{Error, Result};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::archive::{ExtractLimits, ExtractedFile, extract_archive};
use crate::download::{
    CancelOnDrop, Download, Phase, Progress, ProgressFn, RUNTIME_EGRESS_FEATURE, Reporter,
    sha256_file, staged_bytes,
};
use crate::manifest::{ModelEntry, is_sha256_hex};
use crate::preflight::check_disk_space;
use crate::runtime::RuntimeEntry;
use crate::store::{InstallGuard, ModelStore, hash_with_progress, sync_dir};

const RECORD_FILE: &str = ".nugget-runtime.json";
const RECORD_VERSION: u32 = 1;
/// Extraction needs room for the archive plus its unpacked contents.
const EXTRACT_FACTOR: u64 = 4;

/// Install record of one extracted runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledRuntime {
    pub version: u32,
    pub id: String,
    /// Upstream release tag.
    pub release: String,
    /// SHA-256 of the archive the files came from.
    pub sha256: String,
    /// Relative path of the server binary.
    pub binary: String,
    pub files: Vec<ExtractedFile>,
    #[serde(with = "time::serde::rfc3339")]
    pub installed_at: OffsetDateTime,
}

impl ModelStore {
    /// Directory holding extracted runtimes.
    pub fn runtimes_dir(&self) -> PathBuf {
        self.root.join("runtimes")
    }

    /// Where `entry` is (or would be) extracted.
    pub fn runtime_dir(&self, entry: &RuntimeEntry) -> Result<PathBuf> {
        if !is_sha256_hex(&entry.sha256) {
            return Err(Error::Invalid(format!(
                "runtime `{}`: bad sha256",
                entry.id
            )));
        }
        Ok(self.runtimes_dir().join(&entry.sha256))
    }

    /// Install record of `entry`, if it is installed.
    pub fn installed_runtime(&self, entry: &RuntimeEntry) -> Result<Option<InstalledRuntime>> {
        let path = self.runtime_dir(entry)?.join(RECORD_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let rec: InstalledRuntime = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Storage(format!("{} is corrupt: {e}", path.display())))?;
        if rec.version != RECORD_VERSION || rec.sha256 != entry.sha256 || rec.binary != entry.binary
        {
            return Ok(None);
        }
        Ok(Some(rec))
    }

    /// Path of the installed server binary for `entry`, or `None` if it is not installed. Cheap
    /// check (record present, binary present with the recorded size); [`Self::verify_runtime`]
    /// re-hashes everything.
    pub fn runtime_binary(&self, entry: &RuntimeEntry) -> Result<Option<PathBuf>> {
        let Some(rec) = self.installed_runtime(entry)? else {
            return Ok(None);
        };
        let path = rel_path(&self.runtime_dir(entry)?, &entry.binary);
        let want = rec.files.iter().find(|f| f.path == entry.binary);
        match (std::fs::metadata(&path), want) {
            (Ok(md), Some(f)) if md.is_file() && md.len() == f.size => Ok(Some(path)),
            _ => Ok(None),
        }
    }

    /// Download, verify and extract `entry`; returns the server binary path. A no-op if already
    /// installed. Egress is recorded to this store's sink under [`RUNTIME_EGRESS_FEATURE`].
    pub async fn install_runtime(
        &self,
        entry: &RuntimeEntry,
        on_progress: impl Fn(Progress) + Send + Sync + 'static,
    ) -> Result<PathBuf> {
        self.install_runtime_with_egress(entry, Arc::clone(&self.egress), on_progress)
            .await
    }

    /// [`Self::install_runtime`] recording egress to `egress` instead of the store's sink.
    pub async fn install_runtime_with_egress(
        &self,
        entry: &RuntimeEntry,
        egress: Arc<dyn EgressSink>,
        on_progress: impl Fn(Progress) + Send + Sync + 'static,
    ) -> Result<PathBuf> {
        let f: ProgressFn = Arc::new(on_progress);
        let reporter = Reporter::new(&entry.id, entry.size, f, self.opts.progress_interval);
        let cancel = CancelOnDrop::new();
        let res = self
            .install_runtime_inner(entry, egress, &reporter, &cancel)
            .await;
        match &res {
            Ok(_) => reporter.emit(Phase::Installed, entry.size),
            Err(e) => reporter.fail(e),
        }
        res
    }

    async fn install_runtime_inner(
        &self,
        entry: &RuntimeEntry,
        egress: Arc<dyn EgressSink>,
        reporter: &Reporter,
        cancel: &CancelOnDrop,
    ) -> Result<PathBuf> {
        entry.validate()?;
        let _guard = InstallGuard::acquire(&self.installing, &entry.sha256)?;
        if let Some(bin) = self.runtime_binary(entry)? {
            return Ok(bin);
        }

        let staging = self.staging_dir(&entry.sha256);
        let as_model = pseudo_model(entry);
        check_disk_space(
            self.disk.as_ref(),
            &self.root,
            entry.size.saturating_mul(EXTRACT_FACTOR),
            staged_bytes(&staging, &as_model),
        )?;
        let data = Download {
            client: &self.client,
            entry: &as_model,
            dir: staging.clone(),
            opts: &self.opts,
            reporter: reporter.clone(),
            egress,
            feature: RUNTIME_EGRESS_FEATURE,
        }
        .run()
        .await?;

        let len = std::fs::metadata(&data)?.len();
        let actual = if len == entry.size {
            Some(hash_with_progress(&data, reporter.clone(), &cancel.flag()).await?)
        } else {
            None
        };
        if actual.as_deref() != Some(entry.sha256.as_str()) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(Error::Integrity(match actual {
                Some(h) => format!(
                    "runtime `{}`: SHA-256 mismatch (expected {}, got {h}); staging discarded",
                    entry.id, entry.sha256
                ),
                None => format!(
                    "runtime `{}`: size mismatch (expected {}, got {len}); staging discarded",
                    entry.id, entry.size
                ),
            }));
        }

        let _lock = self.lock.lock().await;
        let dest = self.runtime_dir(entry)?;
        // A directory without a valid record is a leftover from an interrupted install.
        match std::fs::remove_dir_all(&dest) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        let (archive, format, dest2) = (data.clone(), entry.archive, dest.clone());
        let report = tokio::task::spawn_blocking(move || {
            extract_archive(&archive, format, &dest2, &ExtractLimits::default())
        })
        .await
        .map_err(|e| Error::Other(format!("extract task failed: {e}")))??;
        let _ = std::fs::remove_dir_all(&staging);

        let bin = rel_path(&dest, &entry.binary);
        if report.file(&entry.binary).is_none() || !bin.is_file() {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(Error::Integrity(format!(
                "runtime `{}`: archive has no `{}`",
                entry.id, entry.binary
            )));
        }
        set_executable(&bin)?;

        let rec = InstalledRuntime {
            version: RECORD_VERSION,
            id: entry.id.clone(),
            release: entry.version.clone(),
            sha256: entry.sha256.clone(),
            binary: entry.binary.clone(),
            files: report.files,
            installed_at: OffsetDateTime::now_utc(),
        };
        let tmp = dest.join(format!("{RECORD_FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec_pretty(&rec)?)?;
        std::fs::rename(&tmp, dest.join(RECORD_FILE))?;
        sync_dir(&dest);
        tracing::info!(runtime = %entry.id, release = %entry.version, "runtime installed");
        Ok(bin)
    }

    /// Re-hash every extracted file of `entry` against its install record.
    pub async fn verify_runtime(&self, entry: &RuntimeEntry) -> Result<()> {
        let rec = self
            .installed_runtime(entry)?
            .ok_or_else(|| Error::NotFound(format!("runtime `{}` is not installed", entry.id)))?;
        let dir = self.runtime_dir(entry)?;
        let cancel = CancelOnDrop::new();
        let flag = cancel.flag();
        let id = entry.id.clone();
        tokio::task::spawn_blocking(move || {
            for f in &rec.files {
                let p = rel_path(&dir, &f.path);
                let h = sha256_file(&p, &flag, |_| {})
                    .map_err(|e| Error::Integrity(format!("runtime `{id}`: {}: {e}", f.path)))?;
                if h != f.sha256 {
                    return Err(Error::Integrity(format!(
                        "runtime `{id}`: {} was modified",
                        f.path
                    )));
                }
            }
            Ok(())
        })
        .await
        .map_err(|e| Error::Other(format!("hash task failed: {e}")))?
    }

    /// Delete an installed runtime (no-op if absent). Do not call while it is running.
    pub async fn remove_runtime(&self, entry: &RuntimeEntry) -> Result<()> {
        let _lock = self.lock.lock().await;
        match std::fs::remove_dir_all(self.runtime_dir(entry)?) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

fn rel_path(dir: &std::path::Path, rel: &str) -> PathBuf {
    rel.split('/').fold(dir.to_path_buf(), |p, c| p.join(c))
}

/// The download machinery is keyed on a [`ModelEntry`]; only id, sources, size and hash matter.
fn pseudo_model(entry: &RuntimeEntry) -> ModelEntry {
    ModelEntry {
        id: entry.id.clone(),
        family: "runtime".into(),
        file_name: "runtime-archive".into(),
        url: entry.url.clone(),
        mirrors: entry.mirrors.clone(),
        revision: String::new(),
        size: entry.size,
        sha256: entry.sha256.clone(),
        min_ram_gb: 0,
        tiers: Vec::new(),
        license: entry.license.clone(),
        min_app_version: "0.0.0".into(),
    }
}

fn set_executable(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
