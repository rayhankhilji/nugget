//! Restore: verify a backup and stage it under `<data>/restore-pending/`, then apply the staged
//! restore when the store is next opened (daemon start, or right away for the offline CLI).
//!
//! Staging never touches the live data. It checks the SHA-256 of every file in the manifest,
//! that the key opens the copy (the store key when its id matches the manifest, else the key
//! sealed in `key.json` opened with the passphrase), SQLite's full `integrity_check`, and a full
//! [`Store::check_consistency`] (audit chain, search and vector indexes) on a scratch copy. It
//! then writes `ready.json` last and renames the staging directory into place.
//!
//! Applying ([`apply_pending`]) moves the current database and its `-wal`/`-shm` aside
//! (`nugget.db.pre-restore-<ts>`, or `nugget.db.corrupt-<ts>` when it was damaged), installs
//! the backup's key if it differs (the old key is kept by [`StoreKey::install`] as
//! `store.key.replaced-<ts>`), moves the staged database in, moves the current `config.toml`
//! aside and puts the backup's in its place, and writes vault files only into a missing or
//! empty vault directory. `ready.json` then becomes `applied.json`; [`finish_applied`] opens
//! the store, re-anchors the audit log and records the restore in it, and removes the staging
//! directory. Every step is a rename that can be repeated, so a crash at any point leaves
//! either the old or the new data in place and the next start finishes the job. Nothing is
//! ever deleted except the staging directory itself.
//!
//! When the backup's key is not the current store key, staging keeps it until the restart in
//! `restore-pending/staged-key.json`, sealed under a random token stored in the owner-only
//! `ready.json` beside it: no stronger than the `0600` key file used when there is no keychain,
//! and it exists only between staging and the next start.

use std::fs;
use std::path::{Path, PathBuf};

use nugget_core::paths::Paths;
use nugget_core::{Error, Result};
use nugget_store::{AuditEntry, FileHealth, KeyEnvelope, Store, StoreKey};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;

use super::disk::{
    self, KEY_FILE, MANIFEST_FILE, Manifest, STORE_FILE, compact, io_err, rfc3339, sha256_file,
    write_private,
};

/// Staging directory name under `<data>`.
const PENDING: &str = "restore-pending";
/// Written last when staging; its presence means "apply at the next open".
const READY: &str = "ready.json";
/// `ready.json` after the files were swapped; the store still needs re-anchoring.
const APPLIED: &str = "applied.json";
/// The verified, stripped database.
const STAGED_DB: &str = "store.db";
/// Files carried in the backup (`files/config/...`, `files/vault/...`).
const STAGED_FILES: &str = "files";
/// The backup's `key.json` (becomes `backup-key.json` when the key changes).
const STAGED_ENVELOPE: &str = "envelope.json";
/// The backup's store key, sealed under `Ready::key_token`.
const STAGED_KEY: &str = "staged-key.json";
/// Tiny Argon2 cost for [`STAGED_KEY`]: the token is a random 256-bit secret, not a passphrase.
const TOKEN_KDF: (u32, u32, u32) = (8 * 1024, 1, 1);

/// `<data>/restore-pending`.
pub(crate) fn pending_dir(paths: &Paths) -> PathBuf {
    paths.data.join(PENDING)
}

/// Whether a verified restore waits for the next start.
pub(crate) fn is_pending(paths: &Paths) -> bool {
    pending_dir(paths).join(READY).is_file()
}

/// What the current database is called once moved aside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Aside {
    /// `nugget.db.pre-restore-<ts>`.
    PreRestore,
    /// `nugget.db.corrupt-<ts>` (the database was damaged).
    Corrupt,
}

impl Aside {
    fn label(self) -> &'static str {
        match self {
            Self::PreRestore => "pre-restore",
            Self::Corrupt => "corrupt",
        }
    }
}

/// `restore-pending/ready.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Ready {
    pub format: u32,
    /// The backup directory restored from.
    pub source: String,
    /// The backup's `created_at`.
    pub created_at: String,
    pub items: u64,
    /// Key id of the backup (and so of the restored store).
    pub key_id: String,
    pub staged_at: String,
    /// Timestamp used in the names of everything moved aside (fixed, so a repeated apply
    /// after a crash continues with the same names).
    pub stamp: String,
    pub aside: Aside,
    /// Where vault files go (only into a missing or empty directory).
    pub vault_root: Option<String>,
    /// Opens [`STAGED_KEY`] when the backup's key is not the current store key.
    pub key_token: Option<String>,
}

/// How to stage a restore.
pub(crate) struct StageRequest<'a> {
    /// The backup directory.
    pub dir: &'a Path,
    /// The current store key, if this home has one.
    pub current: Option<&'a StoreKey>,
    pub passphrase: Option<&'a str>,
    pub aside: Aside,
    pub vault_root: Option<&'a Path>,
}

/// Verify the backup at `req.dir` fully and stage it (see the module docs). Replaces a restore
/// staged earlier. Returns the staged record. Blocking.
pub(crate) fn stage(paths: &Paths, req: &StageRequest<'_>) -> Result<Ready> {
    let dir = req.dir;
    let manifest = Manifest::read(dir)?;
    if manifest.schema_version > nugget_store::SCHEMA_VERSION {
        return Err(Error::Unsupported(format!(
            "the backup was made by a newer Nugget (store schema {}, this one reads up to {}); update Nugget first",
            manifest.schema_version,
            nugget_store::SCHEMA_VERSION
        )));
    }
    for f in [STORE_FILE, KEY_FILE] {
        if !manifest.files.contains_key(f) {
            return Err(Error::Integrity(format!(
                "{MANIFEST_FILE} in {} does not list {f}",
                dir.display()
            )));
        }
    }
    // key.json is checked here; store.nbk on the staged copy (what is restored is what is hashed).
    verify_entry(&manifest, dir, KEY_FILE)?;
    let key = backup_key(&manifest, dir, req.current, req.passphrase)?;

    let root = pending_dir(paths);
    let partial = paths.data.join(format!("{PENDING}.partial"));
    crate::fsutil::create_private_dir(&paths.data).map_err(|e| io_err(&paths.data, e))?;
    remove_tree(&partial);
    crate::fsutil::create_private_dir(&partial).map_err(|e| io_err(&partial, e))?;
    let staged = (|| {
        let db = partial.join(STAGED_DB);
        fs::copy(dir.join(STORE_FILE), &db).map_err(|e| io_err(&dir.join(STORE_FILE), e))?;
        let got = sha256_file(&db)?;
        let want = &manifest.files[STORE_FILE];
        if &got != want {
            return Err(Error::Integrity(format!(
                "{STORE_FILE} in {} does not match its manifest checksum (damaged or modified)",
                dir.display()
            )));
        }
        match nugget_store::check_file(&db, &key, true)? {
            FileHealth::Healthy => {}
            FileHealth::WrongKey => {
                return Err(Error::Integrity(format!(
                    "the key does not open {STORE_FILE} in {}",
                    dir.display()
                )));
            }
            FileHealth::Missing => return Err(Error::Integrity("staged copy vanished".into())),
            FileHealth::Corrupt(why) => {
                return Err(Error::Integrity(format!(
                    "{STORE_FILE} in {} is damaged: {why}",
                    dir.display()
                )));
            }
        }
        let files = nugget_store::read_backup_files(&db, &key)?;
        nugget_store::strip_backup_files(&db, &key)?;
        check_scratch(&partial, &db, &key)?;
        for (rel, content) in &files {
            if !disk::safe_rel(rel) {
                return Err(Error::Integrity(format!(
                    "unsafe file path in backup: {rel}"
                )));
            }
            let path = rel
                .split('/')
                .fold(partial.join(STAGED_FILES), |p, s| p.join(s));
            if let Some(parent) = path.parent() {
                crate::fsutil::create_private_dir(parent).map_err(|e| io_err(parent, e))?;
            }
            fs::write(&path, content).map_err(|e| io_err(&path, e))?;
        }
        fs::copy(dir.join(KEY_FILE), partial.join(STAGED_ENVELOPE))
            .map_err(|e| io_err(&dir.join(KEY_FILE), e))?;
        let same_key = req.current.is_some_and(|k| k.key_id() == key.key_id());
        let key_token = if same_key {
            None
        } else {
            let mut raw = [0u8; 32];
            getrandom::fill(&mut raw)
                .map_err(|e| Error::Other(format!("no OS randomness: {e}")))?;
            let token = hex::encode(raw);
            let (m, t, p) = TOKEN_KDF;
            let env = KeyEnvelope::seal_with(&key, &token, m, t, p)?;
            write_private(&partial.join(STAGED_KEY), &serde_json::to_vec(&env)?)?;
            Some(token)
        };
        let now = OffsetDateTime::now_utc();
        let ready = Ready {
            format: disk::FORMAT,
            source: dir.display().to_string(),
            created_at: manifest.created_at.clone(),
            items: manifest.items,
            key_id: key.key_id(),
            staged_at: rfc3339(now),
            stamp: compact(now),
            aside: req.aside,
            vault_root: req.vault_root.map(|p| p.display().to_string()),
            key_token,
        };
        write_private(&partial.join(READY), &serde_json::to_vec_pretty(&ready)?)?;
        Ok(ready)
    })();
    let ready = match staged {
        Ok(r) => r,
        Err(e) => {
            remove_tree(&partial);
            return Err(e);
        }
    };
    remove_tree(&root);
    if let Err(e) = fs::rename(&partial, &root) {
        remove_tree(&partial);
        return Err(io_err(&root, e));
    }
    disk::sync_dir(&paths.data);
    tracing::info!(backup = %ready.source, created_at = %ready.created_at, "restore staged; it is applied at the next start");
    Ok(ready)
}

/// Check one manifest entry against the file.
fn verify_entry(manifest: &Manifest, dir: &Path, name: &str) -> Result<()> {
    let got = sha256_file(&dir.join(name))?;
    if got != manifest.files[name] {
        return Err(Error::Integrity(format!(
            "{name} in {} does not match its manifest checksum (damaged or modified)",
            dir.display()
        )));
    }
    Ok(())
}

/// The key that opens the backup: the current store key when its id matches, else the one
/// sealed in `key.json` (needs the passphrase).
fn backup_key(
    manifest: &Manifest,
    dir: &Path,
    current: Option<&StoreKey>,
    passphrase: Option<&str>,
) -> Result<StoreKey> {
    if let Some(k) = current.filter(|k| k.key_id() == manifest.key_id) {
        return Ok(k.clone());
    }
    let Some(passphrase) = passphrase.filter(|p| !p.is_empty()) else {
        return Err(Error::Unauthorized(
            "this backup was made with another store key (another computer, or a key that was replaced): enter its backup passphrase".into(),
        ));
    };
    let path = dir.join(KEY_FILE);
    let bytes = fs::read(&path).map_err(|e| io_err(&path, e))?;
    let env: KeyEnvelope = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Integrity(format!("{}: {e}", path.display())))?;
    if env.key_id() != manifest.key_id {
        return Err(Error::Integrity(format!(
            "{KEY_FILE} in {} seals another key than the backup's",
            dir.display()
        )));
    }
    let key = env.open(passphrase)?;
    if key.key_id() != manifest.key_id {
        return Err(Error::Integrity(format!(
            "{KEY_FILE} in {} does not hold the backup's key",
            dir.display()
        )));
    }
    Ok(key)
}

/// Open a scratch copy of the staged database as a store and run the full consistency check.
fn check_scratch(partial: &Path, db: &Path, key: &StoreKey) -> Result<()> {
    let scratch_dir = partial.join("check");
    crate::fsutil::create_private_dir(&scratch_dir).map_err(|e| io_err(&scratch_dir, e))?;
    let scratch = scratch_dir.join("nugget.db");
    fs::copy(db, &scratch).map_err(|e| io_err(&scratch, e))?;
    let report = {
        let store = Store::open(&scratch, key)?;
        let r = store.check_consistency();
        store.flush_egress();
        r
    }?;
    remove_tree(&scratch_dir);
    if !report.ok {
        return Err(Error::Integrity(format!(
            "the backup does not pass the consistency check: {}",
            report.problems.join("; ")
        )));
    }
    Ok(())
}

/// What applying a staged restore did.
#[derive(Debug, Clone)]
pub(crate) struct Applied {
    pub ready: Ready,
    /// Where the previous database went, if there was one.
    pub moved_to: Option<PathBuf>,
}

/// Apply a staged restore, if one is pending (see the module docs). Returns the key to open
/// the restored store with (the installed one) and what was done; `None` when nothing is
/// pending. `current` is the key the store would otherwise be opened with. Blocking.
pub(crate) fn apply_pending(
    paths: &Paths,
    current: Option<&StoreKey>,
) -> Result<Option<(StoreKey, Applied)>> {
    let root = pending_dir(paths);
    let ready_path = root.join(READY);
    let bytes = match fs::read(&ready_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err(&ready_path, e)),
    };
    let ready: Ready = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Integrity(format!("{}: {e}", ready_path.display())))?;
    let key = match (&ready.key_token, current) {
        (_, Some(k)) if k.key_id() == ready.key_id => k.clone(),
        (Some(token), _) => {
            let path = root.join(STAGED_KEY);
            let b = fs::read(&path).map_err(|e| io_err(&path, e))?;
            let env: KeyEnvelope = serde_json::from_slice(&b)
                .map_err(|e| Error::Integrity(format!("{}: {e}", path.display())))?;
            env.open(token)?
        }
        (None, _) => {
            return Err(Error::Integrity(
                "the staged restore needs the store key it was staged with, which is gone".into(),
            ));
        }
    };
    if key.key_id() != ready.key_id {
        return Err(Error::Integrity(
            "the staged key is not the backup's".into(),
        ));
    }
    let key_changed = current.is_none_or(|k| k.key_id() != key.key_id());
    let tag = format!("{}-{}", ready.aside.label(), ready.stamp);
    let db = paths.db_file();
    let staged_db = root.join(STAGED_DB);

    // 1. The key first: if this fails nothing has moved. `install` keeps a different old key.
    let key = if key_changed {
        let source = StoreKey::install(paths, &key)?;
        tracing::info!(?source, "restored store key installed");
        StoreKey::load_existing(paths)?
            .filter(|k| k.key_id() == key.key_id())
            .unwrap_or(key)
    } else {
        key
    };

    // 2. The current database aside (side files first, so a new database never meets an old
    //    WAL), then the staged one in. Each rename is skipped once done.
    let aside = sibling(&db, &tag);
    let mut moved_to = None;
    if staged_db.exists() {
        for suffix in ["-wal", "-shm", "-journal", ""] {
            let from = suffixed(&db, suffix);
            let to = suffixed(&aside, suffix);
            if from.exists() {
                if to.exists() {
                    return Err(Error::Other(format!(
                        "cannot move {} aside: {} already exists",
                        from.display(),
                        to.display()
                    )));
                }
                fs::rename(&from, &to).map_err(|e| io_err(&from, e))?;
                tracing::warn!(from = %from.display(), to = %to.display(), "previous database moved aside (not deleted)");
            }
        }
        disk::sync_dir(&paths.data);
        fs::rename(&staged_db, &db).map_err(|e| io_err(&db, e))?;
        disk::sync_dir(&paths.data);
    }
    if aside.exists() {
        moved_to = Some(aside);
    }

    // 3. Settings and vault.
    let files = root.join(STAGED_FILES);
    let config = files.join("config").join("config.toml");
    if config.is_file() {
        let live = paths.config_file();
        if live.exists() {
            let to = sibling(&live, &format!("pre-restore-{}", ready.stamp));
            if !to.exists() {
                fs::rename(&live, &to).map_err(|e| io_err(&live, e))?;
            }
        }
        crate::fsutil::create_private_dir(&paths.config).map_err(|e| io_err(&paths.config, e))?;
        fs::rename(&config, &live)
            .or_else(|_| fs::copy(&config, &live).map(|_| ()))
            .map_err(|e| io_err(&live, e))?;
    }
    let settings = files.join("config").join("backup.json");
    let live_settings = disk::settings_path(paths);
    if settings.is_file() && !live_settings.exists() {
        // This computer's settings win when it has some; otherwise take the backup's, without
        // its last error.
        if let Ok(mut s) =
            serde_json::from_slice::<disk::Settings>(&fs::read(&settings).unwrap_or_default())
        {
            s.last_error = None;
            s.last_failure_at = None;
            s.save(paths)?;
        }
    }
    if key_changed {
        // The passphrase envelope must seal the store key, or later backups could not be
        // opened: take the backup's (same passphrase as the backup), keep the old one aside.
        let live = disk::envelope_path(paths);
        if live.exists() {
            let to = sibling(&live, &format!("pre-restore-{}", ready.stamp));
            if !to.exists() {
                fs::rename(&live, &to).map_err(|e| io_err(&live, e))?;
            }
        }
        let env = fs::read(root.join(STAGED_ENVELOPE))
            .map_err(|e| io_err(&root.join(STAGED_ENVELOPE), e))?;
        write_private(&live, &env)?;
    }
    if let Some(vault) = ready.vault_root.as_deref().map(PathBuf::from) {
        restore_vault(&files.join("vault"), &vault)?;
    }

    // 4. Done swapping.
    fs::rename(&ready_path, root.join(APPLIED)).map_err(|e| io_err(&ready_path, e))?;
    disk::sync_dir(&root);
    Ok(Some((key, Applied { ready, moved_to })))
}

/// Copy restored vault files into `vault` if it is missing or empty; otherwise leave it.
fn restore_vault(from: &Path, vault: &Path) -> Result<()> {
    if !from.is_dir() {
        return Ok(());
    }
    let empty = match fs::read_dir(vault) {
        Ok(mut rd) => rd.next().is_none(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => return Err(io_err(vault, e)),
    };
    if !empty {
        tracing::info!(vault = %vault.display(), "vault is not empty; its pages are kept and re-rendered from the restored store");
        return Ok(());
    }
    let mut found = Vec::new();
    disk::walk_files(from, from, &mut found)?;
    for (rel, path) in found {
        let to = rel.split('/').fold(vault.to_path_buf(), |p, s| p.join(s));
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
        }
        fs::copy(&path, &to).map_err(|e| io_err(&to, e))?;
    }
    tracing::info!(vault = %vault.display(), "vault restored");
    Ok(())
}

/// After [`apply_pending`] (now or at an earlier start that stopped half way): re-anchor the
/// audit log of the opened, restored `store`, record the restore, remove the staging directory.
pub(crate) fn finish_applied(
    paths: &Paths,
    store: &Store,
    applied: Option<&Applied>,
) -> Result<()> {
    let root = pending_dir(paths);
    let applied_path = root.join(APPLIED);
    let ready: Option<Ready> = match applied {
        Some(a) => Some(a.ready.clone()),
        None => match fs::read(&applied_path) {
            Ok(b) => serde_json::from_slice(&b).ok(),
            Err(_) => return Ok(()),
        },
    };
    // Never forced: a restore only re-anchors a genuine rollback (the anchor is ahead of the
    // restored log). An anchor this log contradicts (rewritten log, or an anchor of the same key
    // from another install) is kept as evidence and reported by the startup audit check
    // (`audit_chain_ok: false`); replacing it is left to an explicit user decision.
    if let Err(e) = store.reset_audit_anchor(false) {
        tracing::error!(error = %e,
            "AUDIT ANCHOR NOT RESET: the restored audit log does not match the stored anchor; the anchor is kept and the audit check will report it");
    }
    let detail = match &ready {
        Some(r) => json!({
            "backup": r.source,
            "created_at": r.created_at,
            "items": r.items,
            "key_changed": r.key_token.is_some(),
            "moved_to": applied.and_then(|a| a.moved_to.as_ref()).map(|p| p.display().to_string()),
        }),
        None => json!({}),
    };
    store.append_audit(&AuditEntry {
        actor: "nuggetd".into(),
        action: "backup.restore".into(),
        target: "store".into(),
        detail,
    })?;
    remove_tree(&root);
    tracing::warn!("restore applied");
    Ok(())
}

/// Whether an earlier start swapped the files but did not finish.
pub(crate) fn has_applied(paths: &Paths) -> bool {
    pending_dir(paths).join(APPLIED).is_file()
}

/// `path` with `.tag` appended to its file name.
fn sibling(path: &Path, tag: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(tag);
    path.with_file_name(name)
}

/// `path` with `suffix` appended to its file name.
fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Remove a directory tree of ours (staging), logging failures.
fn remove_tree(dir: &Path) {
    match fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "cannot remove staging directory")
        }
    }
}
