//! Backups on disk: settings (`<data>/backup.json`), the passphrase envelope
//! (`<data>/backup-key.json`), the backup directory format (version 1), listing and retention.
//!
//! A backup is `<folder>/nugget-backup-YYYYMMDDTHHMMSSZ[-n]/` with `store.nbk`, `key.json` and
//! `manifest.json` (written last). While it is written the directory is called `<name>.partial`
//! and holds the marker file [`PARTIAL_MARKER`]; it is renamed to its final name once
//! everything is synced. Retention only ever deletes directories that match this naming *and*
//! hold our manifest or marker, and only removes the files this module writes, then the
//! (then empty) directory: a folder with anything else in it is left alone.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use nugget_store::{KeyEnvelope, Store, StoreKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Backup directory format version (`manifest.json` `format`).
pub(crate) const FORMAT: u32 = 1;
/// Prefix of backup directory names.
pub(crate) const BACKUP_PREFIX: &str = "nugget-backup-";
/// Prefix of export directory names.
pub(crate) const EXPORT_PREFIX: &str = "nugget-export-";
/// Suffix of a backup directory being written.
const PARTIAL_SUFFIX: &str = ".partial";
/// Marker file inside a `.partial` backup directory.
pub(crate) const PARTIAL_MARKER: &str = ".nugget-backup-partial";
/// The encrypted database copy in a backup.
pub(crate) const STORE_FILE: &str = "store.nbk";
/// The sealed store key in a backup.
pub(crate) const KEY_FILE: &str = "key.json";
/// The plaintext manifest, written last.
pub(crate) const MANIFEST_FILE: &str = "manifest.json";
/// A `.partial` directory this old (by its marker's modification time) is an abandoned backup.
const STALE_PARTIAL: Duration = Duration::from_secs(12 * 3600);
/// Vault files larger than this are not put in a backup (the vault is Markdown).
const MAX_VAULT_FILE: u64 = 32 * 1024 * 1024;
/// Suffix of the vault's temporary files (`nugget_vault::paths::TMP_SUFFIX`).
const VAULT_TMP_SUFFIX: &str = ".nugget-tmp";

/// Default `interval_hours`.
pub(crate) const DEFAULT_INTERVAL_HOURS: u32 = 24;
/// Default `keep`.
pub(crate) const DEFAULT_KEEP: u32 = 7;
/// Bounds of `keep`.
pub(crate) const KEEP_RANGE: (u32, u32) = (1, 365);
/// Largest `interval_hours` (one year).
pub(crate) const MAX_INTERVAL_HOURS: u32 = 24 * 366;
/// Shortest backup passphrase, in characters.
pub(crate) const MIN_PASSPHRASE_CHARS: usize = 8;

/// One backup on disk, as the IPC reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BackupInfo {
    /// The backup directory.
    pub path: String,
    /// RFC 3339.
    pub created_at: String,
    pub items: u64,
    /// Total size of the backup's files.
    pub bytes: u64,
    pub schema_version: u32,
    pub app_version: String,
    /// `linux`, `macos` or `windows` (the OS the backup was made on).
    pub os: String,
}

/// `<data>/backup.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    pub folder: Option<String>,
    /// 0 = manual only.
    pub interval_hours: u32,
    pub keep: u32,
    pub last_backup: Option<BackupInfo>,
    pub last_error: Option<String>,
    /// RFC 3339 time of the last failed attempt (retries are spaced from it).
    pub last_failure_at: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            folder: None,
            interval_hours: DEFAULT_INTERVAL_HOURS,
            keep: DEFAULT_KEEP,
            last_backup: None,
            last_error: None,
            last_failure_at: None,
        }
    }
}

/// `<data>/backup.json`.
pub(crate) fn settings_path(paths: &Paths) -> PathBuf {
    paths.data.join("backup.json")
}

/// `<data>/backup-key.json`: the store key sealed under the backup passphrase.
pub(crate) fn envelope_path(paths: &Paths) -> PathBuf {
    paths.data.join("backup-key.json")
}

impl Settings {
    /// Load the settings; defaults when the file is missing. A damaged file is reported.
    pub fn load(paths: &Paths) -> Result<Self> {
        let path = settings_path(paths);
        match fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b)
                .map_err(|e| Error::Invalid(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(io_err(&path, e)),
        }
    }

    /// Write the settings atomically (owner-only).
    pub fn save(&self, paths: &Paths) -> Result<()> {
        write_private(&settings_path(paths), &serde_json::to_vec_pretty(self)?)
    }

    /// The configured folder, if any.
    pub fn folder(&self) -> Option<PathBuf> {
        self.folder.as_deref().map(PathBuf::from)
    }
}

/// The stored passphrase envelope, if any.
pub(crate) fn load_envelope(paths: &Paths) -> Result<Option<KeyEnvelope>> {
    let path = envelope_path(paths);
    match fs::read(&path) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| Error::Integrity(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(&path, e)),
    }
}

/// The envelope, only if it seals `key` (a passphrase set for an older key does not count).
pub(crate) fn envelope_for(paths: &Paths, key: &StoreKey) -> Result<Option<KeyEnvelope>> {
    Ok(load_envelope(paths)?.filter(|e| e.key_id() == key.key_id()))
}

/// Check a new backup passphrase (at least [`MIN_PASSPHRASE_CHARS`] characters).
pub(crate) fn check_passphrase(passphrase: &str) -> Result<()> {
    if passphrase.chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(Error::Invalid(format!(
            "the backup passphrase must be at least {MIN_PASSPHRASE_CHARS} characters"
        )));
    }
    Ok(())
}

/// Seal `key` under `passphrase` (default Argon2id cost) and store it as the backup envelope.
pub(crate) fn set_passphrase(paths: &Paths, key: &StoreKey, passphrase: &str) -> Result<()> {
    check_passphrase(passphrase)?;
    let env = KeyEnvelope::seal(key, passphrase)?;
    write_private(&envelope_path(paths), &serde_json::to_vec_pretty(&env)?)
}

/// `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub format: u32,
    pub created_at: String,
    pub app_version: String,
    pub schema_version: u32,
    pub os: String,
    pub key_id: String,
    pub items: u64,
    pub chunks: u64,
    pub entities: u64,
    pub audit_entries: u64,
    pub files: BTreeMap<String, FileEntry>,
}

/// Size and checksum of one backup file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileEntry {
    pub bytes: u64,
    /// Lowercase hex SHA-256.
    pub sha256: String,
}

impl Manifest {
    /// Read `<dir>/manifest.json`. `Error::NotFound` when missing, `Error::Integrity` when it
    /// does not parse, `Error::Unsupported` for another format.
    pub fn read(dir: &Path) -> Result<Self> {
        let path = dir.join(MANIFEST_FILE);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotFound(format!(
                    "{} is not a complete Nugget backup (no {MANIFEST_FILE})",
                    dir.display()
                )));
            }
            Err(e) => return Err(io_err(&path, e)),
        };
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Integrity(format!("{}: {e}", path.display())))?;
        let format = value.get("format").and_then(serde_json::Value::as_u64);
        if format != Some(u64::from(FORMAT)) {
            return Err(Error::Unsupported(format!(
                "{}: backup format {} (this Nugget reads format {FORMAT})",
                path.display(),
                format.map_or_else(|| "unknown".into(), |f| f.to_string())
            )));
        }
        serde_json::from_value(value)
            .map_err(|e| Error::Integrity(format!("{}: {e}", path.display())))
    }

    /// The [`BackupInfo`] of the backup in `dir`.
    pub fn info(&self, dir: &Path) -> BackupInfo {
        BackupInfo {
            path: dir.display().to_string(),
            created_at: self.created_at.clone(),
            items: self.items,
            bytes: self.files.values().map(|f| f.bytes).sum(),
            schema_version: self.schema_version,
            app_version: self.app_version.clone(),
            os: self.os.clone(),
        }
    }
}

/// This OS as a manifest names it.
pub(crate) fn os_name() -> &'static str {
    std::env::consts::OS
}

/// `YYYYMMDDTHHMMSSZ` (UTC).
pub(crate) fn compact(t: OffsetDateTime) -> String {
    let t = t.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// RFC 3339 (UTC, whole seconds).
pub(crate) fn rfc3339(t: OffsetDateTime) -> String {
    t.to_offset(time::UtcOffset::UTC)
        .replace_nanosecond(0)
        .unwrap_or(t)
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// Parse an RFC 3339 time.
pub(crate) fn parse_time(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).ok()
}

/// Whether `name` is `<prefix>YYYYMMDDTHHMMSSZ` optionally followed by `-<n>`.
pub(crate) fn is_dated_name(name: &str, prefix: &str) -> bool {
    let Some(rest) = name.strip_prefix(prefix) else {
        return false;
    };
    let (stamp, suffix) = rest.split_at_checked(16).unwrap_or((rest, ""));
    let b = stamp.as_bytes();
    let stamp_ok = b.len() == 16
        && b[8] == b'T'
        && b[15] == b'Z'
        && b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit);
    let suffix_ok = suffix.is_empty()
        || suffix.strip_prefix('-').is_some_and(|n| {
            !n.is_empty() && n.len() <= 4 && n.bytes().all(|c| c.is_ascii_digit())
        });
    stamp_ok && suffix_ok
}

/// A fresh `<prefix><stamp>[-n]` directory name in `folder`: no directory of that name (or its
/// `.partial`) exists yet.
pub(crate) fn fresh_name(folder: &Path, prefix: &str, at: OffsetDateTime) -> Result<String> {
    let base = format!("{prefix}{}", compact(at));
    for n in 1..10_000u32 {
        let name = if n == 1 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        let taken = [name.clone(), format!("{name}{PARTIAL_SUFFIX}")]
            .iter()
            .any(|n| fs::symlink_metadata(folder.join(n)).is_ok());
        if !taken {
            return Ok(name);
        }
    }
    Err(Error::Other(format!(
        "too many backups named {base} in {}",
        folder.display()
    )))
}

/// Lowercase hex SHA-256 and size of a file, streamed.
pub(crate) fn sha256_file(path: &Path) -> Result<FileEntry> {
    let mut f = File::open(path).map_err(|e| io_err(path, e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut bytes = 0u64;
    loop {
        let n = f.read(&mut buf).map_err(|e| io_err(path, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        bytes += n as u64;
    }
    Ok(FileEntry {
        bytes,
        sha256: hex::encode(h.finalize()),
    })
}

/// What a backup needs besides the store.
pub(crate) struct BackupRequest<'a> {
    pub paths: &'a Paths,
    pub store: &'a Store,
    pub key: &'a StoreKey,
    /// The folder to write into (must exist).
    pub folder: &'a Path,
    /// Apply retention in `folder` afterwards, keeping this many complete backups.
    pub keep: Option<u32>,
    /// The vault root to include, if the vault is in use.
    pub vault_root: Option<&'a Path>,
}

/// Make one backup (see the module docs), then apply retention. Blocking.
pub(crate) fn run_backup(req: &BackupRequest<'_>) -> Result<BackupInfo> {
    let folder = req.folder;
    if !folder.is_dir() {
        return Err(Error::NotFound(format!(
            "the backup folder {} does not exist (is the drive connected?)",
            folder.display()
        )));
    }
    let envelope = envelope_for(req.paths, req.key)?.ok_or_else(|| {
        Error::Unauthorized("set a backup passphrase first (backup.set_passphrase)".into())
    })?;
    let now = OffsetDateTime::now_utc();
    let name = fresh_name(folder, BACKUP_PREFIX, now)?;
    let partial = folder.join(format!("{name}{PARTIAL_SUFFIX}"));
    let done = folder.join(&name);
    fs::create_dir(&partial).map_err(|e| io_err(&partial, e))?;
    let result = (|| {
        write_synced(
            &partial.join(PARTIAL_MARKER),
            format!("{{\"created_at\":\"{}\"}}\n", rfc3339(now)).as_bytes(),
        )?;
        let files = collect_files(req.paths, req.vault_root)?;
        let stats = req.store.backup_into(&partial.join(STORE_FILE), &files)?;
        write_synced(
            &partial.join(KEY_FILE),
            &serde_json::to_vec_pretty(&envelope)?,
        )?;
        let mut entries = BTreeMap::new();
        for f in [STORE_FILE, KEY_FILE] {
            entries.insert(f.to_string(), sha256_file(&partial.join(f))?);
        }
        let manifest = Manifest {
            format: FORMAT,
            created_at: rfc3339(now),
            app_version: env!("CARGO_PKG_VERSION").into(),
            schema_version: req.store.schema_version()?,
            os: os_name().into(),
            key_id: req.key.key_id(),
            items: stats.items,
            chunks: stats.chunks,
            entities: stats.entities,
            audit_entries: stats.audit_entries,
            files: entries,
        };
        write_synced(
            &partial.join(MANIFEST_FILE),
            &serde_json::to_vec_pretty(&manifest)?,
        )?;
        fs::remove_file(partial.join(PARTIAL_MARKER))
            .map_err(|e| io_err(&partial.join(PARTIAL_MARKER), e))?;
        sync_dir(&partial);
        fs::rename(&partial, &done).map_err(|e| io_err(&done, e))?;
        sync_dir(folder);
        Ok(manifest.info(&done))
    })();
    match result {
        Ok(info) => {
            if let Some(keep) = req.keep {
                retain(folder, keep, &done);
            }
            Ok(info)
        }
        Err(e) => {
            remove_backup_dir(&partial);
            Err(e)
        }
    }
}

/// The files a backup carries: `config/config.toml`, `config/backup.json` and every regular
/// file under the vault root (`vault/<relative path>`, including the vault's `.nugget/` state
/// so pages and their merge bases come back together; temporary files are skipped).
pub(crate) fn collect_files(
    paths: &Paths,
    vault_root: Option<&Path>,
) -> Result<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::new();
    for (name, path) in [
        ("config/config.toml", paths.config_file()),
        ("config/backup.json", settings_path(paths)),
    ] {
        match fs::read(&path) {
            Ok(b) => files.push((name.to_string(), b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(&path, e)),
        }
    }
    if let Some(root) = vault_root.filter(|r| r.is_dir()) {
        let mut found = Vec::new();
        walk_files(root, root, &mut found)?;
        for (rel, path) in found {
            if rel.ends_with(VAULT_TMP_SUFFIX) || !safe_rel(&rel) {
                continue;
            }
            match fs::metadata(&path) {
                Ok(m) if m.len() > MAX_VAULT_FILE => {
                    tracing::warn!(file = %path.display(), bytes = m.len(), "vault file too large for a backup; skipped");
                    continue;
                }
                _ => {}
            }
            match fs::read(&path) {
                Ok(b) => files.push((format!("vault/{rel}"), b)),
                // Removed meanwhile (the vault is live).
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_err(&path, e)),
            }
        }
    }
    Ok(files)
}

/// Every regular file under `dir` (symlinks are not followed), as (`/`-separated path
/// relative to `root`, path).
pub(crate) fn walk_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(|e| io_err(dir, e))?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Ok(ft) = e.file_type() else { continue };
        let path = e.path();
        if ft.is_dir() {
            walk_files(root, &path, out)?;
        } else if ft.is_file() {
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let parts: Option<Vec<&str>> =
                rel.components().map(|c| c.as_os_str().to_str()).collect();
            if let Some(parts) = parts {
                out.push((parts.join("/"), path.clone()));
            }
        }
    }
    Ok(())
}

/// A relative `/`-separated path that is safe to store and to write back out (the rules of
/// `nugget_store::backup`: no empty, `.` or `..` segments, backslashes, colons or NULs).
pub(crate) fn safe_rel(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.contains(['\\', ':', '\0'])
        && rel
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

/// The complete backups in `folder`, newest first. A missing folder has none; unreadable or
/// foreign entries are skipped.
pub(crate) fn list_backups(folder: &Path) -> Vec<BackupInfo> {
    let mut found: Vec<(String, String, BackupInfo)> = complete_backups(folder)
        .into_iter()
        .map(|(name, dir, m)| (m.created_at.clone(), name, m.info(&dir)))
        .collect();
    found.sort_by(|a, b| (&b.0, &b.1).cmp(&(&a.0, &a.1)));
    found.into_iter().map(|(_, _, i)| i).collect()
}

/// (name, dir, manifest) of every complete backup in `folder`.
fn complete_backups(folder: &Path) -> Vec<(String, PathBuf, Manifest)> {
    let Ok(rd) = fs::read_dir(folder) else {
        return Vec::new();
    };
    rd.filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            if !is_dated_name(&name, BACKUP_PREFIX) {
                return None;
            }
            let dir = e.path();
            let m = Manifest::read(&dir).ok()?;
            Some((name, dir, m))
        })
        .collect()
}

/// Delete complete backups beyond the newest `keep` (never `current`) and abandoned `.partial`
/// directories. Failures are logged.
pub(crate) fn retain(folder: &Path, keep: u32, current: &Path) {
    let mut all = complete_backups(folder);
    all.sort_by(|a, b| (&b.2.created_at, &b.0).cmp(&(&a.2.created_at, &a.0)));
    for (_, dir, _) in all.into_iter().skip(keep.max(1) as usize) {
        if dir == current {
            continue;
        }
        tracing::info!(backup = %dir.display(), "removing old backup (retention)");
        remove_backup_dir(&dir);
    }
    let Ok(rd) = fs::read_dir(folder) else { return };
    for e in rd.filter_map(|e| e.ok()) {
        let Some(name) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(base) = name.strip_suffix(PARTIAL_SUFFIX) else {
            continue;
        };
        if !is_dated_name(base, BACKUP_PREFIX) || !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let dir = e.path();
        let marker = dir.join(PARTIAL_MARKER);
        let stamp = fs::metadata(&marker)
            .or_else(|_| fs::metadata(dir.join(MANIFEST_FILE)))
            .and_then(|m| m.modified());
        let Ok(stamp) = stamp else {
            continue; // not ours
        };
        let age = SystemTime::now().duration_since(stamp).unwrap_or_default();
        if age >= STALE_PARTIAL {
            tracing::info!(backup = %dir.display(), "removing an unfinished backup");
            remove_backup_dir(&dir);
        }
    }
}

/// Remove the files a backup directory may hold (ours only), then the directory if it is
/// empty. Anything else in it is kept, and so is the directory.
pub(crate) fn remove_backup_dir(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.filter_map(|e| e.ok()) {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        let ours = name == KEY_FILE
            || name == MANIFEST_FILE
            || name == PARTIAL_MARKER
            || name == STORE_FILE
            || name.starts_with(&format!("{STORE_FILE}-"));
        if ours
            && e.file_type().is_ok_and(|t| t.is_file())
            && let Err(err) = fs::remove_file(e.path())
        {
            tracing::warn!(file = %e.path().display(), error = %err, "cannot remove backup file");
        }
    }
    if let Err(err) = fs::remove_dir(dir) {
        tracing::warn!(dir = %dir.display(), error = %err, "backup directory kept (not empty or not removable)");
    }
}

/// Write `bytes` to a new file at `path` and sync it.
pub(crate) fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = File::create_new(path).map_err(|e| io_err(path, e))?;
    f.write_all(bytes).map_err(|e| io_err(path, e))?;
    f.sync_all().map_err(|e| io_err(path, e))
}

/// Atomically replace `path` with `bytes`, owner-only (`0600` on Unix): temp file, sync, rename.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        crate::fsutil::create_private_dir(dir).map_err(|e| io_err(dir, e))?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let written = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(io_err(path, e));
    }
    if let Some(dir) = path.parent() {
        sync_dir(dir);
    }
    Ok(())
}

/// Make renames in `dir` durable (Unix; best effort elsewhere, where it is a no-op).
pub(crate) fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// An I/O error naming the path.
pub(crate) fn io_err(path: &Path, e: std::io::Error) -> Error {
    Error::Io(std::io::Error::new(
        e.kind(),
        format!("{}: {e}", path.display()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dated_names() {
        assert!(is_dated_name(
            "nugget-backup-20261030T091500Z",
            BACKUP_PREFIX
        ));
        assert!(is_dated_name(
            "nugget-backup-20261030T091500Z-12",
            BACKUP_PREFIX
        ));
        assert!(!is_dated_name(
            "nugget-backup-20261030T091500Z-",
            BACKUP_PREFIX
        ));
        assert!(!is_dated_name(
            "nugget-backup-20261030T091500",
            BACKUP_PREFIX
        ));
        assert!(!is_dated_name(
            "nugget-backup-2026103XT091500Z",
            BACKUP_PREFIX
        ));
        assert!(!is_dated_name(
            "nugget-backup-20261030T091500Z.partial",
            BACKUP_PREFIX
        ));
        assert!(!is_dated_name("my-backup-20261030T091500Z", BACKUP_PREFIX));
        assert!(!is_dated_name(
            "nugget-backup-ü0261030T091500Z",
            BACKUP_PREFIX
        ));
        let t = OffsetDateTime::from_unix_timestamp(1_793_351_700).unwrap();
        assert_eq!(compact(t), "20261030T091500Z");
        assert!(is_dated_name(
            &format!("{EXPORT_PREFIX}{}", compact(t)),
            EXPORT_PREFIX
        ));
    }

    #[test]
    fn safe_relative_paths() {
        assert!(safe_rel("entities/a.md"));
        assert!(safe_rel(".nugget/state.json"));
        for bad in ["", "/a", "a//b", "../a", "a/./b", "a\\b", "c:x", "a/"] {
            assert!(!safe_rel(bad), "{bad}");
        }
    }

    #[test]
    fn retention_only_touches_our_backups() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path();
        let mk = |name: &str, created: &str| {
            let d = f.join(name);
            fs::create_dir(&d).unwrap();
            let m = Manifest {
                format: FORMAT,
                created_at: created.into(),
                app_version: "0".into(),
                schema_version: 1,
                os: "linux".into(),
                key_id: "00".into(),
                items: 1,
                chunks: 0,
                entities: 0,
                audit_entries: 0,
                files: BTreeMap::new(),
            };
            fs::write(d.join(MANIFEST_FILE), serde_json::to_vec(&m).unwrap()).unwrap();
            fs::write(d.join(STORE_FILE), b"x").unwrap();
            d
        };
        let a = mk("nugget-backup-20260101T000000Z", "2026-01-01T00:00:00Z");
        let b = mk("nugget-backup-20260102T000000Z", "2026-01-02T00:00:00Z");
        let c = mk("nugget-backup-20260103T000000Z", "2026-01-03T00:00:00Z");
        // Foreign: our name but no manifest; our manifest under another name; ours with an
        // extra file.
        fs::create_dir(f.join("nugget-backup-19990101T000000Z")).unwrap();
        let other = mk("holiday-photos", "2000-01-01T00:00:00Z");
        let extra = mk("nugget-backup-20250101T000000Z", "2025-01-01T00:00:00Z");
        fs::write(extra.join("notes.txt"), b"mine").unwrap();
        retain(f, 2, &c);
        assert!(c.exists() && b.exists());
        assert!(!a.exists());
        assert!(f.join("nugget-backup-19990101T000000Z").exists());
        assert!(other.join(STORE_FILE).exists());
        assert!(extra.join("notes.txt").exists() && !extra.join(STORE_FILE).exists());
        assert_eq!(list_backups(f).len(), 2);
        assert_eq!(list_backups(f)[0].created_at, "2026-01-03T00:00:00Z");
    }
}
